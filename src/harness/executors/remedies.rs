//! The remedy catalogue (§18.2 of the System One orchestration
//! revision).
//!
//! An unproven serializability obligation names its obstacles, and each
//! obstacle names its gaps: the transaction, the side, the object, the
//! step. A gap admits a small set of mechanical edits. A remedy is a
//! pure function from the operation's program and those obstacles to a
//! candidate program — nothing here reads the engine, asks a model, or
//! decides anything.
//!
//! A remedy does not have to be right. Every candidate is judged by the
//! analyzer before it is considered (§18.4), so a remedy that does not
//! prove its target is simply not admissible. What a remedy must never
//! do is edit outside the operations it was given: the repair task's
//! scope is the programs of one conflict closure (§18.3), and a gap in
//! a transaction outside it is left alone.
//!
//! The first catalogue covers serializability through the routes that
//! need no judgment about the application: declared isolation, strict
//! locks, and the version protocol.
//!
//! The version protocol needs one qualification. A version guard
//! *rejects*, and what an operation does when its transaction is
//! rejected is its author's decision, not a mechanical edit. So the
//! guard is a candidate only where the transaction already declares a
//! `rejected` arm: the author has then said what a rejection does, and
//! the regression check (§18.4) holds that arm to every other declared
//! requirement. Where there is no arm, the route escalates. So do
//! ordering obstacles, which are not in the catalogue at all.

use std::collections::{BTreeMap, BTreeSet};

use crate::analyzer::verification::transaction_conflicts::{DependencyGap, TransactionRef};
use crate::analyzer::verification::transaction_serializability::TransactionSerializabilityObstacle;
use crate::spec::{
    BumpVersion, FieldPath, FieldSelection, Id, Lock, LockMode, LockOrder, ObjectSelector,
    OperationBlock, TransactionIsolation, TransactionStep, ValidateVersion, ValueRef, ValueSource,
};

/// Which route a remedy takes. The order is the catalogue order: the
/// last tie-break of the deterministic preference (§18.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RemedyKind {
    /// Declare `read_committed` where no isolation is declared, which
    /// is what supplies commit-order evidence for write-read and
    /// write-write dependencies.
    DeclaredIsolation,

    /// Declare `serializable` on every weaker member of the conflict
    /// closure: the closure route.
    SerializableClosure,

    /// Take an exclusive lock on each unprotected domain before the
    /// first access it protects: the strict-lock route.
    StrictLocks,

    /// Validate at commit the version a read observed, and advance it
    /// beside the mutation: the version-protocol route.
    VersionProtocol,
}

/// The programs a remedy reads and rewrites, by operation.
pub type Programs = BTreeMap<Id, OperationBlock>;

/// One candidate repair of the programs in scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remedy {
    pub kind: RemedyKind,

    /// Every program in scope after the edit; compare with the input to
    /// find the ones it changed ([`Remedy::edited`]).
    pub programs: Programs,

    /// How invasive it is, for the deterministic preference.
    pub transactions_edited: usize,
    pub steps_added: usize,

    /// What it does, in one line, for the commit summary and hand-off.
    pub summary: String,
}

impl Remedy {
    /// Least invasive first: fewest transactions edited, then fewest
    /// steps added, then catalogue order.
    pub fn invasiveness(&self) -> (usize, usize, RemedyKind) {
        (self.transactions_edited, self.steps_added, self.kind)
    }

    /// The programs this remedy changes relative to `before`.
    pub fn edited<'a>(
        &'a self,
        before: &'a Programs,
    ) -> impl Iterator<Item = (&'a Id, &'a OperationBlock)> {
        self.programs
            .iter()
            .filter(move |(operation, program)| before.get(*operation) != Some(program))
    }
}

/// A transaction of one program in scope.
type Tx = (Id, Id);

fn transaction_mut<'a>(
    programs: &'a mut Programs,
    (operation, transaction): &Tx,
) -> Option<&'a mut crate::spec::Transaction> {
    programs.get_mut(operation)?.transaction_mut(transaction)
}

/// Every candidate the catalogue offers for `obstacles`, each an edit
/// of the programs in scope alone. Candidates that would be identical
/// to another are dropped; one that would change nothing is not a
/// candidate.
///
/// Besides each route alone, the catalogue offers declared isolation
/// composed with each step-adding route: an obstacle's gaps can mix an
/// unspecified isolation on one dependency with a missing lock or
/// version guard on another, and neither route alone closes both.
///
/// `versions` maps each versioned object to its version field — a fact
/// of the data model, which a remedy reads and never edits.
pub fn candidates(
    programs: &Programs,
    obstacles: &[&TransactionSerializabilityObstacle],
    versions: &BTreeMap<Id, FieldPath>,
) -> Vec<Remedy> {
    let gaps: Vec<&DependencyGap> = obstacles
        .iter()
        .flat_map(|obstacle| match obstacle {
            TransactionSerializabilityObstacle::TransactionSerializabilityUnprotectedReadWriteDependency {
                dependency,
            }
            | TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedDependency {
                dependency,
            } => dependency.gaps.iter().collect::<Vec<_>>(),

            _ => Vec::new(),
        })
        .collect();

    let isolation = declared_isolation(programs, &gaps);

    // Declared isolation edits no step, so the step indices the gaps
    // name still hold in the program it produces.
    let composed = |route: fn(&Programs, &[&DependencyGap]) -> Option<Remedy>| {
        let base = isolation.as_ref()?;
        let then = route(&base.programs, &gaps)?;

        Some(Remedy {
            kind: then.kind,
            transactions_edited: count_edited(programs, &then.programs),
            steps_added: then.steps_added,
            summary: format!("{}, and {}", base.summary, then.summary),
            programs: then.programs,
        })
    };

    let version_route =
        |programs: &Programs, gaps: &[&DependencyGap]| version_protocol(programs, gaps, versions);

    let mut remedies: Vec<Remedy> = [
        isolation.clone(),
        serializable_closure(programs, obstacles),
        strict_locks(programs, &gaps),
        version_route(programs, &gaps),
        composed(strict_locks),
        isolation.as_ref().and_then(|base| {
            let then = version_route(&base.programs, &gaps)?;

            Some(Remedy {
                kind: then.kind,
                transactions_edited: count_edited(programs, &then.programs),
                steps_added: then.steps_added,
                summary: format!("{}, and {}", base.summary, then.summary),
                programs: then.programs,
            })
        }),
    ]
    .into_iter()
    .flatten()
    .filter(|remedy| &remedy.programs != programs)
    .collect();

    remedies.sort_by_key(Remedy::invasiveness);

    remedies.dedup_by(|later, earlier| later.programs == earlier.programs);

    remedies
}

/// How many transactions differ between two sets of programs.
fn count_edited(before: &Programs, after: &Programs) -> usize {
    after
        .iter()
        .map(|(operation, program)| {
            let Some(old) = before.get(operation) else {
                return program.transactions().len();
            };

            program
                .transactions()
                .into_iter()
                .filter(|(_, transaction)| old.transaction(&transaction.id) != Some(*transaction))
                .count()
        })
        .sum()
}

/// The transaction a gap names, when its program is in scope.
fn in_scope(programs: &Programs, transaction: &TransactionRef) -> Option<Tx> {
    programs.contains_key(&transaction.operation).then(|| {
        (
            transaction.operation.clone(),
            transaction.transaction.clone(),
        )
    })
}

fn listed(transactions: &BTreeSet<Tx>) -> String {
    let single = transactions
        .iter()
        .map(|(operation, _)| operation)
        .collect::<BTreeSet<_>>()
        .len()
        <= 1;

    transactions
        .iter()
        .map(|(operation, transaction)| {
            if single {
                format!("`{transaction}`")
            } else {
                format!("`{transaction}` of {operation}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `IsolationUnspecified`: nothing says the transaction reads committed
/// data or installs conflicting writes in commit order. `read_committed`
/// is the weakest declaration that says both.
fn declared_isolation(programs: &Programs, gaps: &[&DependencyGap]) -> Option<Remedy> {
    let undeclared: BTreeSet<Tx> = gaps
        .iter()
        .filter_map(|gap| match gap {
            DependencyGap::IsolationUnspecified { transaction } => in_scope(programs, transaction),
            _ => None,
        })
        .collect();

    if undeclared.is_empty() {
        return None;
    }

    let mut programs = programs.clone();

    for transaction in &undeclared {
        transaction_mut(&mut programs, transaction)?.isolation =
            TransactionIsolation::ReadCommitted;
    }

    Some(Remedy {
        kind: RemedyKind::DeclaredIsolation,
        programs,
        transactions_edited: undeclared.len(),
        steps_added: 0,
        summary: format!(
            "declare read_committed isolation on {}",
            listed(&undeclared)
        ),
    })
}

/// The closure route needs *every* member serializable, so it is a
/// candidate only when every weaker member is in scope.
fn serializable_closure(
    programs: &Programs,
    obstacles: &[&TransactionSerializabilityObstacle],
) -> Option<Remedy> {
    let weaker: Vec<&TransactionRef> = obstacles
        .iter()
        .flat_map(|obstacle| match obstacle {
            TransactionSerializabilityObstacle::SerializableClosureContainsWeakerIsolation {
                transactions,
            } => transactions
                .iter()
                .map(|fact| &fact.transaction)
                .collect::<Vec<_>>(),

            _ => Vec::new(),
        })
        .collect();

    let ours: BTreeSet<Tx> = weaker
        .iter()
        .filter_map(|transaction| in_scope(programs, transaction))
        .collect();

    if ours.is_empty()
        || weaker
            .iter()
            .any(|member| !programs.contains_key(&member.operation))
    {
        return None;
    }

    let mut programs = programs.clone();

    for transaction in &ours {
        transaction_mut(&mut programs, transaction)?.isolation = TransactionIsolation::Serializable;
    }

    Some(Remedy {
        kind: RemedyKind::SerializableClosure,
        programs,
        transactions_edited: ours.len(),
        steps_added: 0,
        summary: format!("declare serializable isolation on {}", listed(&ours)),
    })
}

/// The selector a step accesses. An insert selects nothing: the
/// instance it creates cannot be locked before it exists.
fn selector_of(step: &TransactionStep) -> Option<&ObjectSelector> {
    match step {
        TransactionStep::Read(read) => Some(&read.target),
        TransactionStep::Write(write) => Some(&write.target),
        TransactionStep::Delete(delete) => Some(&delete.target),
        TransactionStep::Lock(lock) => Some(&lock.target),
        TransactionStep::Transition(transition) => Some(&transition.subject),
        TransactionStep::ValidateVersion(validate) => Some(&validate.target),
        TransactionStep::BumpVersion(bump) => Some(&bump.target),
        TransactionStep::AdvanceCursor(advance) => Some(&advance.target),
        TransactionStep::Fence(fence) => Some(&fence.target),

        TransactionStep::Insert(_)
        | TransactionStep::EstablishEffectIntent(_)
        | TransactionStep::EstablishTransactionOutput(_)
        | TransactionStep::WriteOutbox(_) => None,
    }
}

/// The strict-lock route: the lock is held from before the first access
/// it protects to termination. Exclusive on both sides — a template
/// that reads a domain and then writes it needs it, and the analyzer,
/// not this function, decides whether a weaker mode would have done.
///
/// A covering lock acquired too late is moved, not duplicated.
fn strict_locks(programs: &Programs, gaps: &[&DependencyGap]) -> Option<Remedy> {
    let mut programs = programs.clone();

    let mut edited: BTreeSet<Tx> = BTreeSet::new();
    let mut added = 0;

    // Late locks first, by step index descending, so that moving one
    // does not shift the index another gap names.
    let mut late: Vec<(Tx, usize)> = gaps
        .iter()
        .filter_map(|gap| match gap {
            DependencyGap::LockAcquiredAfterProtectedAccess {
                transaction,
                lock_step,
                ..
            } => in_scope(&programs, transaction).map(|id| (id, *lock_step)),
            _ => None,
        })
        .collect();

    late.sort_by(|a, b| b.1.cmp(&a.1));
    late.dedup();

    for (transaction, lock_step) in late {
        let body = &mut transaction_mut(&mut programs, &transaction)?.steps;

        if !matches!(body.get(lock_step), Some(TransactionStep::Lock(_))) {
            continue;
        }

        let lock = body.remove(lock_step);

        body.insert(0, lock);

        edited.insert(transaction.clone());
    }

    // Then the missing ones. Selectors are collected before anything is
    // inserted, against the body the gaps' step indices describe.
    let mut missing: Vec<(Tx, ObjectSelector)> = Vec::new();

    for gap in gaps {
        let DependencyGap::LockCoverageMissing {
            transaction, step, ..
        } = gap
        else {
            continue;
        };

        let Some(id) = in_scope(&programs, transaction) else {
            continue;
        };

        let Some(selector) = programs
            .get(&id.0)
            .and_then(|program| program.transaction(&id.1))
            .and_then(|transaction| transaction.steps.get(*step))
            .and_then(selector_of)
        else {
            continue;
        };

        let wanted = (id, selector.clone());

        if !missing.contains(&wanted) {
            missing.push(wanted);
        }
    }

    for (transaction, selector) in missing {
        let body = &mut transaction_mut(&mut programs, &transaction)?.steps;

        let held = body.iter().any(|step| {
            matches!(step, TransactionStep::Lock(lock)
                if lock.target == selector && lock.mode == LockMode::Exclusive)
        });

        if held {
            continue;
        }

        body.insert(
            0,
            TransactionStep::Lock(Lock {
                target: selector,
                mode: LockMode::Exclusive,
                order: LockOrder::Unspecified,
            }),
        );

        added += 1;

        edited.insert(transaction);
    }

    if edited.is_empty() {
        return None;
    }

    Some(Remedy {
        kind: RemedyKind::StrictLocks,
        programs,
        transactions_edited: edited.len(),
        steps_added: added,
        summary: format!(
            "hold an exclusive lock on each unprotected instance from the start of {}",
            listed(&edited)
        ),
    })
}

/// The version-protocol route: the reader validates at commit the
/// version its read observed, and the writer advances it, so a stale
/// observation rejects instead of committing.
///
/// The guard goes directly after the read it guards and reuses that
/// read's selector, so it identifies the very instance that was
/// observed; the bump goes after the last step that mutates the object.
fn version_protocol(
    programs: &Programs,
    gaps: &[&DependencyGap],
    versions: &BTreeMap<Id, FieldPath>,
) -> Option<Remedy> {
    let mut programs = programs.clone();

    let mut edited: BTreeSet<Tx> = BTreeSet::new();
    let mut added = 0;

    let mut unvalidated: Vec<(Tx, &Id)> = Vec::new();
    let mut unbumped: Vec<(Tx, &Id)> = Vec::new();

    for gap in gaps {
        match gap {
            DependencyGap::VersionValidationMissing {
                transaction,
                object,
            } => {
                if let Some(id) = in_scope(&programs, transaction)
                    && !unvalidated.contains(&(id.clone(), object))
                {
                    unvalidated.push((id, object));
                }
            }

            DependencyGap::VersionBumpMissing {
                transaction,
                object,
            } => {
                if let Some(id) = in_scope(&programs, transaction)
                    && !unbumped.contains(&(id.clone(), object))
                {
                    unbumped.push((id, object));
                }
            }

            _ => {}
        }
    }

    for (transaction, object) in unvalidated {
        let version = versions.get(object)?;

        // A guard rejects. Only an author says what a rejection does.
        let declares_rejection =
            programs
                .get(&transaction.0)?
                .executions()
                .into_iter()
                .any(|(_, execution)| {
                    execution.transaction.id == transaction.1 && execution.rejected.is_some()
                });

        if !declares_rejection {
            return None;
        }

        let body = &mut transaction_mut(&mut programs, &transaction)?.steps;

        let position = body.iter().position(
            |step| matches!(step, TransactionStep::Read(read) if &read.target.object == object),
        )?;

        let TransactionStep::Read(read) = &mut body[position] else {
            return None;
        };

        if let FieldSelection::Only(fields) = &mut read.fields {
            fields.insert(version.clone());
        }

        let guard = TransactionStep::ValidateVersion(ValidateVersion {
            target: read.target.clone(),
            expected: ValueRef {
                source: ValueSource::TransactionRead(read.bind.clone()),
                path: version.clone(),
            },
        });

        body.insert(position + 1, guard);

        added += 1;

        edited.insert(transaction);
    }

    for (transaction, object) in unbumped {
        if !versions.contains_key(object) {
            return None;
        }

        let body = &mut transaction_mut(&mut programs, &transaction)?.steps;

        let mutation = body.iter().rposition(|step| {
            matches!(
                step,
                TransactionStep::Write(_)
                    | TransactionStep::Transition(_)
                    | TransactionStep::AdvanceCursor(_)
                    | TransactionStep::Fence(_)
            ) && selector_of(step).is_some_and(|selector| &selector.object == object)
        })?;

        let target = selector_of(&body[mutation])?.clone();

        body.insert(
            mutation + 1,
            TransactionStep::BumpVersion(BumpVersion { target }),
        );

        added += 1;

        edited.insert(transaction);
    }

    if edited.is_empty() {
        return None;
    }

    Some(Remedy {
        kind: RemedyKind::VersionProtocol,
        programs,
        transactions_edited: edited.len(),
        steps_added: added,
        summary: format!(
            "validate at commit the version each read observed, and advance it beside the \
             mutation, in {}",
            listed(&edited)
        ),
    })
}
