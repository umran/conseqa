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
//! locks, and the observed-state guard.
//!
//! The observed-state guard is a real conditional mutation or nothing.
//! It conditions a mutation the reader already performs on the same
//! instance — an update turned compare-and-set, or a comparison added
//! to a transition or cursor advance — on what the read
//! observed; where the transaction mutates no such instance, the route
//! invents no assertion and leaves the gap to a lock or serializable
//! isolation. It needs one qualification more. A guard *rejects*, and
//! what an operation does when its transaction is rejected is its
//! author's decision, not a mechanical edit. So an update becomes a
//! compare-and-set only where the transaction already declares a
//! `rejected` arm: the author has then said what a rejection does, and
//! the regression check (§18.4) holds that arm to every other declared
//! requirement. Where there is no arm, the route escalates. So do
//! ordering obstacles, which are not in the catalogue at all.

use std::collections::{BTreeMap, BTreeSet};

use crate::analyzer::verification::TransactionOrderingObstacle;
use crate::analyzer::verification::transaction_conflicts::AccessFields;
use crate::analyzer::verification::transaction_conflicts::{DependencyGap, TransactionRef};
use crate::analyzer::verification::transaction_serializability::TransactionSerializabilityObstacle;
use crate::spec::{
    AdvanceCursor, CompareAndSet, CompareCondition, CursorAdvanceRule, FieldPath, FieldSelection,
    Id, IdempotencyGuarantee, IdempotencyKey, Lock, LockMode, LockOrder, ObjectSelector,
    OperationBlock, SelectorValue, TransactionIsolation, TransactionOrderingRequirement,
    TransactionStep, ValueRef, ValueSource,
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

    /// Condition the reader's own later mutation of the observed
    /// instance on what the read observed — its version, or the fields
    /// the conflict touches: the observed-state guard route.
    ObservedGuard,

    /// Point a cursor or fence at the requirement's position.
    OrderingPosition,

    /// Persist the position through a cursor advance, where the
    /// transaction already writes it with an update or compare-and-set.
    OrderedCursor,

    /// Deduplicate a transaction's commit by the governing key: replay
    /// route B, which recovers the one keyed commit instead of
    /// re-executing it.
    KeyedCommit,
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
/// observed-state guard on another, and neither route alone closes
/// both.
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

    let guard_route =
        |programs: &Programs, gaps: &[&DependencyGap]| observed_guard(programs, gaps, versions);

    let mut remedies: Vec<Remedy> = [
        isolation.clone(),
        serializable_closure(programs, obstacles),
        strict_locks(programs, &gaps),
        guard_route(programs, &gaps),
        composed(strict_locks),
        isolation.as_ref().and_then(|base| {
            let then = guard_route(&base.programs, &gaps)?;

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
    step.selector()
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

/// The observed-state guard route: the reader conditions its own first
/// later mutation of the observed instance on what the read observed,
/// so a stale observation rejects instead of committing.
///
/// On a versioned object the comparison is the version the read
/// observed — added to the read where it is narrowed — which covers
/// every field of the live instance. On an unversioned one it is the
/// observed fields themselves: those the gap names as uncovered, or
/// every field the read selects. An update becomes a compare-and-set,
/// which rejects, so only where the author declared a `rejected` arm; a
/// transition, cursor advance, or compare-and-set already rejects and
/// gains the comparison. A fence is passed over — on an equal token it
/// holds no write protection. Where the transaction performs no
/// guardable mutation of the read instance, nothing is invented.
fn observed_guard(
    programs: &Programs,
    gaps: &[&DependencyGap],
    versions: &BTreeMap<Id, FieldPath>,
) -> Option<Remedy> {
    let mut programs = programs.clone();

    let mut edited: BTreeSet<Tx> = BTreeSet::new();
    let mut added = 0;

    // Per guarded read: the fields to compare beyond the version, when
    // the gap names them.
    let mut unguarded: Vec<(Tx, usize, Option<BTreeSet<FieldPath>>)> = Vec::new();

    for gap in gaps {
        let (transaction, step, fields) = match gap {
            DependencyGap::ObservedStateGuardMissing {
                transaction, step, ..
            } => (transaction, *step, None),

            DependencyGap::ObservedStateGuardDoesNotCoverConflict {
                transaction,
                step,
                fields,
                ..
            } => (
                transaction,
                *step,
                match fields {
                    AccessFields::Only(fields) => Some(fields.clone()),
                    AccessFields::All | AccessFields::Unknown => None,
                },
            ),

            _ => continue,
        };

        let Some(id) = in_scope(&programs, transaction) else {
            continue;
        };

        match unguarded
            .iter_mut()
            .find(|(other, other_step, _)| *other == id && *other_step == step)
        {
            Some((_, _, existing)) => {
                if let (Some(existing), Some(fields)) = (existing.as_mut(), fields) {
                    existing.extend(fields);
                }
            }

            None => unguarded.push((id, step, fields)),
        }
    }

    for (transaction, step, fields) in unguarded {
        let declares_rejection =
            programs
                .get(&transaction.0)?
                .executions()
                .into_iter()
                .any(|(_, execution)| {
                    execution.transaction.id == transaction.1 && execution.rejected.is_some()
                });

        let body = &mut transaction_mut(&mut programs, &transaction)?.steps;

        let TransactionStep::Read(read) = body.get(step)? else {
            return None;
        };

        let (bind, target) = (read.bind.clone(), read.target.clone());

        let compared: BTreeSet<FieldPath> = match versions.get(&target.object) {
            Some(version) => [version.clone()].into(),

            None => match (fields, &read.fields) {
                (Some(fields), _) => fields,
                (None, FieldSelection::Only(fields)) => fields.clone(),
                (None, FieldSelection::All) => return None,
            },
        };

        // The read must observe what is compared.
        if let TransactionStep::Read(read) = &mut body[step]
            && let FieldSelection::Only(fields) = &mut read.fields
        {
            fields.extend(compared.iter().cloned());
        }

        let mutation = body
            .iter()
            .enumerate()
            .skip(step + 1)
            .position(|(_, inner)| {
                // A fence holds no write protection on an equal token,
                // so its comparison would guard nothing.
                matches!(
                    inner,
                    TransactionStep::Update(_)
                        | TransactionStep::CompareAndSet(_)
                        | TransactionStep::Transition(_)
                        | TransactionStep::AdvanceCursor(_)
                ) && inner.selector() == Some(&target)
            })
            .map(|offset| offset + step + 1)?;

        let conditions: Vec<CompareCondition> = compared
            .into_iter()
            .map(|field| CompareCondition {
                expected: SelectorValue::Value(ValueRef {
                    source: ValueSource::TransactionRead(bind.clone()),
                    path: field.clone(),
                }),
                field,
            })
            .collect();

        let extend = |compare: &mut Vec<CompareCondition>| {
            for condition in &conditions {
                if !compare
                    .iter()
                    .any(|existing| existing.field == condition.field)
                {
                    compare.push(condition.clone());
                }
            }
        };

        match &mut body[mutation] {
            TransactionStep::Update(update) => {
                // A compare-and-set rejects. Only an author says what a
                // rejection does.
                if !declares_rejection {
                    return None;
                }

                let mut compare = Vec::new();

                extend(&mut compare);

                body[mutation] = TransactionStep::CompareAndSet(CompareAndSet {
                    target: update.target.clone(),
                    compare,
                    fields: update.fields.clone(),
                    values: update.values.clone(),
                });
            }

            TransactionStep::CompareAndSet(cas) => extend(&mut cas.compare),
            TransactionStep::Transition(transition) => extend(&mut transition.compare),
            TransactionStep::AdvanceCursor(advance) => extend(&mut advance.compare),

            _ => return None,
        }

        added += 1;

        edited.insert(transaction);
    }

    if edited.is_empty() {
        return None;
    }

    Some(Remedy {
        kind: RemedyKind::ObservedGuard,
        programs,
        transactions_edited: edited.len(),
        steps_added: added,
        summary: format!(
            "condition the later mutation of each observed instance on the version or state \
             its read observed, in {}",
            listed(&edited)
        ),
    })
}

// ---------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------

/// One unproven ordering requirement: the transaction it is declared on,
/// the requirement, and why it is unproven.
pub struct OrderingGap<'a> {
    pub operation: Id,
    pub transaction: Id,
    pub requirement: &'a TransactionOrderingRequirement,
    pub obstacles: &'a [TransactionOrderingObstacle],
}

/// Every candidate the catalogue offers for unproven ordering: the
/// position a cursor or fence consumes corrected to the requirement's,
/// and — where nothing persists the position — the update of it
/// turned into a cursor advance, under each advance rule. (The
/// serializability an ordering proof rests on is repaired by
/// [`candidates`], from the obstacles an ordering verdict embeds.)
///
/// A cursor rejects a stale position, and what an operation does when
/// its transaction is rejected is its author's decision: as with the
/// observed-state guard, a cursor is a candidate only where the
/// transaction already declares a `rejected` arm.
pub fn ordering_candidates(programs: &Programs, gaps: &[OrderingGap<'_>]) -> Vec<Remedy> {
    let mut remedies: Vec<Remedy> = [ordering_position(programs, gaps)]
        .into_iter()
        .flatten()
        .chain(
            [
                CursorAdvanceRule::MonotonicAfter,
                CursorAdvanceRule::Successor,
            ]
            .into_iter()
            .filter_map(|rule| ordered_cursor(programs, gaps, rule)),
        )
        .filter(|remedy| &remedy.programs != programs)
        .collect();

    // Stable: the permissive rule stays ahead of `successor` unless a
    // stated fact reorders them.
    remedies.sort_by_key(Remedy::invasiveness);

    remedies
}

fn ordering_position(programs: &Programs, gaps: &[OrderingGap<'_>]) -> Option<Remedy> {
    let mut programs = programs.clone();
    let mut edited: BTreeSet<Tx> = BTreeSet::new();

    for gap in gaps {
        for obstacle in gap.obstacles {
            let TransactionOrderingObstacle::OrderingPositionMismatch { step, .. } = obstacle
            else {
                continue;
            };

            let id = (gap.operation.clone(), gap.transaction.clone());

            let Some(body) = transaction_mut(&mut programs, &id).map(|tx| &mut tx.steps) else {
                continue;
            };

            match body.get_mut(*step) {
                Some(TransactionStep::AdvanceCursor(advance)) => {
                    advance.incoming = gap.requirement.position.clone();
                }

                Some(TransactionStep::Fence(fence)) => {
                    fence.token = gap.requirement.position.clone();
                }

                _ => continue,
            }

            edited.insert(id);
        }
    }

    if edited.is_empty() {
        return None;
    }

    Some(Remedy {
        kind: RemedyKind::OrderingPosition,
        programs,
        transactions_edited: edited.len(),
        steps_added: 0,
        summary: format!(
            "advance each cursor or fence by the requirement's position in {}",
            listed(&edited)
        ),
    })
}

/// Whether a written field is the one that records the position: its
/// name carries the position's own name (`sequence` → `last_sequence`).
/// Which field records it is otherwise a judgment, and this catalogue
/// makes none.
fn records_position(field: &FieldPath, position: &ValueRef) -> bool {
    let (Some(field), Some(position)) = (field.0.last(), position.path.0.last()) else {
        return false;
    };

    let field = field.to_lowercase();
    let position = position.to_lowercase();

    !position.is_empty() && field.contains(&position)
}

fn ordered_cursor(
    programs: &Programs,
    gaps: &[OrderingGap<'_>],
    rule: CursorAdvanceRule,
) -> Option<Remedy> {
    let mut programs = programs.clone();
    let mut edited: BTreeSet<Tx> = BTreeSet::new();

    for gap in gaps {
        if !gap.obstacles.iter().any(|obstacle| {
            matches!(
                obstacle,
                TransactionOrderingObstacle::OrderingMissingCursorOrFence
            )
        }) {
            continue;
        }

        let id = (gap.operation.clone(), gap.transaction.clone());

        let declares_rejection =
            programs
                .get(&gap.operation)?
                .executions()
                .into_iter()
                .any(|(_, execution)| {
                    execution.transaction.id == gap.transaction && execution.rejected.is_some()
                });

        if !declares_rejection {
            continue;
        }

        let body = &mut transaction_mut(&mut programs, &id)?.steps;

        // An update or compare-and-set recording the position: the
        // comparisons a compare-and-set carries go with the cursor.
        let Some((position, field)) = body.iter().enumerate().find_map(|(index, step)| {
            let fields = match step {
                TransactionStep::Update(update) => &update.fields,
                TransactionStep::CompareAndSet(cas) => &cas.fields,
                _ => return None,
            };

            let mut matching = fields
                .iter()
                .filter(|field| records_position(field, &gap.requirement.position));

            // Exactly one written field may record the position.
            match (matching.next(), matching.next()) {
                (Some(field), None) => Some((index, field.clone())),
                _ => None,
            }
        }) else {
            continue;
        };

        let (target, remaining, compare) = match &mut body[position] {
            TransactionStep::Update(update) => {
                update.fields.remove(&field);

                (update.target.clone(), update.fields.len(), Vec::new())
            }

            TransactionStep::CompareAndSet(cas) => {
                cas.fields.remove(&field);

                // Where the compare-and-set keeps other fields it keeps
                // its comparisons; where it dissolves into the cursor,
                // the cursor carries them.
                let compare = if cas.fields.is_empty() {
                    std::mem::take(&mut cas.compare)
                } else {
                    Vec::new()
                };

                (cas.target.clone(), cas.fields.len(), compare)
            }

            _ => continue,
        };

        let advance = TransactionStep::AdvanceCursor(AdvanceCursor {
            target,
            field,
            incoming: gap.requirement.position.clone(),
            rule,
            compare,
        });

        if remaining == 0 {
            body[position] = advance;
        } else {
            body.insert(position, advance);
        }

        edited.insert(id);
    }

    if edited.is_empty() {
        return None;
    }

    let rule = match rule {
        CursorAdvanceRule::MonotonicAfter => "monotonic_after",
        CursorAdvanceRule::Successor => "successor",
    };

    Some(Remedy {
        kind: RemedyKind::OrderedCursor,
        programs,
        transactions_edited: edited.len(),
        steps_added: edited.len(),
        summary: format!(
            "persist the position through a {rule} cursor advance instead of an ordinary \
             update, in {}",
            listed(&edited)
        ),
    })
}

// ---------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------

/// A transaction whose replay routes lack a keyed commit, and the key
/// of the requirement that found it so.
pub struct KeyedCommitGap {
    pub operation: Id,
    pub transaction: Id,
    pub key: IdempotencyKey,
}

/// Deduplicate each named transaction's commit by its requirement's
/// governing key. It changes no step, so it is also composed onto every
/// other candidate ([`with_keyed_commits`]).
///
/// A keyed commit is a declaration the execution environment must honor
/// — a durable deduplication record per key — and like a lock it is an
/// application-level mechanism the author could have written; the
/// analyzer, not this function, decides whether it proves anything.
pub fn keyed_commits(programs: &Programs, gaps: &[KeyedCommitGap]) -> Option<Remedy> {
    let after = apply_keyed_commits(programs, gaps);

    let edited = count_edited(programs, &after);

    if edited == 0 {
        return None;
    }

    Some(Remedy {
        kind: RemedyKind::KeyedCommit,
        programs: after,
        transactions_edited: edited,
        steps_added: 0,
        summary: format!(
            "deduplicate the commit of {} by the requirement's key",
            listed(
                &gaps
                    .iter()
                    .map(|gap| (gap.operation.clone(), gap.transaction.clone()))
                    .collect()
            )
        ),
    })
}

/// `remedy` with the keyed commits composed onto it, when they change
/// anything it does not already.
pub fn with_keyed_commits(
    before: &Programs,
    remedy: &Remedy,
    gaps: &[KeyedCommitGap],
) -> Option<Remedy> {
    let after = apply_keyed_commits(&remedy.programs, gaps);

    if after == remedy.programs {
        return None;
    }

    Some(Remedy {
        kind: remedy.kind,
        transactions_edited: count_edited(before, &after),
        steps_added: remedy.steps_added,
        summary: format!(
            "{}, and deduplicate the commits the replay requirements name by their key",
            remedy.summary
        ),
        programs: after,
    })
}

fn apply_keyed_commits(programs: &Programs, gaps: &[KeyedCommitGap]) -> Programs {
    let mut programs = programs.clone();

    for gap in gaps {
        let id = (gap.operation.clone(), gap.transaction.clone());

        // Set where missing, and replaced where the commit is keyed by
        // something other than the governing key: a commit key whose root
        // is not replay-stable addresses a different commit per attempt.
        if let Some(transaction) = transaction_mut(&mut programs, &id) {
            transaction.idempotency = IdempotencyGuarantee::DeduplicatedBy {
                key: gap.key.clone(),
            };
        }
    }

    programs
}
