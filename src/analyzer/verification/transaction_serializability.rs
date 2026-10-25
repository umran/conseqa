//! Verification of transaction serializability requirements
//! (§8, §35–§53 of the DSL v4 revision).
//!
//! > `SerializableBy(K)`: the committed state history of the
//! > transaction's executions — together with every transaction they
//! > may conflict with — is equivalent to some serial order.
//!
//! Two routes discharge the obligation, both over the requirement
//! transaction's conflict closure (`transaction_conflicts`):
//!
//! - **Serializable-isolation closure** (§42): every template in the
//!   closure declares `isolation: serializable`. One serializable
//!   transaction among weaker conflicting ones is not enough — a
//!   serializable transaction is serializable only with respect to
//!   other serializable ones.
//! - **Serialization graph** (§43–§52): the potential dependency graph
//!   over the closure — write-read, read-write anti-dependencies, and
//!   write-write — has no cyclic strongly connected component
//!   containing a dependency that is not commit-order constrained by
//!   declared evidence: committed reads and atomic write order under
//!   declared isolation, strict S/X locking, an atomic conditional
//!   mutation (a compare-and-set, transition, cursor advance, or upsert
//!   — including one comparing a read's observed state or version), or
//!   a shared ordered cursor. A read-only transaction that
//!   observes committed state at one instant takes that instant as its
//!   serialization point, which orders its anti-dependencies too. An
//!   apparent cycle every edge of which is constrained would imply a
//!   cycle in that order, so it cannot occur in a committed history.
//!
//! Anti-dependencies are what make the second route necessary:
//! repeatable or snapshot-stable reads still admit write skew, so
//! snapshot stability never by itself proves serializability of a
//! transaction that writes. Every
//! refusal names the concrete transaction chain and the unconstrained
//! dependencies on it. All evidence here is L0: no runtime fact
//! participates, and no runtime fact could.

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{Id, Model, StepLocation, TransactionIsolation, ValueRef};

use super::transaction_conflicts::{
    AccessMode, CommitArtifact, CommitOrderEvidence, ConflictIndex, DependencyEvidence,
    DependencyGap, DependencyKind, GuardCoverage, TransactionRef, isolation_label,
};
use super::{ProofScope, RemedyLayer};

/// The verdict for one declared transaction serializability
/// requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionSerializabilityCheck {
    pub operation: Id,
    pub transaction: Id,
    pub location: StepLocation,

    /// Index into `transaction.requirements.serializability`.
    pub requirement: usize,

    /// The requirement key, copied so the check is self-contained.
    pub key: ValueRef,

    pub verdict: TransactionSerializabilityVerdict,

    /// The artifacts the transaction commits beside its mutations —
    /// outbox admissions, ordinary and transition-scoped — reported so
    /// a reader sees what the proven commit history carries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<CommitArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionSerializabilityVerdict {
    Proven {
        proof: TransactionSerializabilityProof,
        scope: ProofScope,
    },
    Unproven {
        obstacles: Vec<TransactionSerializabilityObstacle>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionSerializabilityProof {
    /// Every template in the conflict closure declares serializable
    /// isolation (§42).
    SerializableIsolationClosure {
        root: TransactionRef,
        key: ValueRef,
        closure: Vec<TransactionRef>,
    },

    /// No cyclic component of the closure's dependency graph contains
    /// an unconstrained dependency (§52); every dependency is listed
    /// with the evidence that constrains it, or with none where it
    /// lies on no cycle.
    ConflictGraph {
        root: TransactionRef,
        key: ValueRef,
        closure: Vec<TransactionRef>,
        dependencies: Vec<DependencyEvidence>,
    },
}

impl TransactionSerializabilityProof {
    /// Serializability rests on transaction primitives alone; no
    /// runtime fact ever participates.
    pub fn scope(&self) -> ProofScope {
        ProofScope::L0Only
    }

    pub fn root(&self) -> &TransactionRef {
        match self {
            Self::SerializableIsolationClosure { root, .. } | Self::ConflictGraph { root, .. } => {
                root
            }
        }
    }

    pub fn closure(&self) -> &[TransactionRef] {
        match self {
            Self::SerializableIsolationClosure { closure, .. }
            | Self::ConflictGraph { closure, .. } => closure,
        }
    }
}

/// A closure member and its declared isolation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolationFact {
    pub transaction: TransactionRef,
    pub isolation: TransactionIsolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionSerializabilityObstacle {
    /// The requirement's transaction is not in the access index — a
    /// model verification was not promised; recorded rather than
    /// panicked.
    TransactionNotIndexed { operation: Id, transaction: Id },

    /// The closure route fails: these closure members declare weaker
    /// isolation than serializable.
    SerializableClosureContainsWeakerIsolation { transactions: Vec<IsolationFact> },

    /// A cyclic conflict component containing at least one
    /// unconstrained dependency: the concrete transaction chain a
    /// non-serializable committed history could pass through.
    TransactionSerializabilityUnconstrainedCycle { cycle: Vec<TransactionRef> },

    /// A read-write anti-dependency on such a cycle that neither strict
    /// locking nor an atomic conditional mutation constrains.
    TransactionSerializabilityUnprotectedReadWriteDependency { dependency: DependencyEvidence },

    /// A write-read or write-write dependency on such a cycle with no
    /// commit-order evidence: an unspecified isolation on the side
    /// whose behaviour would have supplied it.
    TransactionSerializabilityUnconstrainedDependency { dependency: DependencyEvidence },
}

/// Checks every transaction serializability requirement declared by
/// the model.
pub fn check(model: &Model) -> Vec<TransactionSerializabilityCheck> {
    let index = ConflictIndex::build(model);

    let mut checks = Vec::new();

    for (position, template) in index.templates.iter().enumerate() {
        for (requirement, declared) in template
            .transaction
            .requirements
            .serializability
            .iter()
            .enumerate()
        {
            let verdict = match prove(&index, position, &declared.key) {
                Ok(proof) => TransactionSerializabilityVerdict::Proven {
                    scope: proof.scope(),
                    proof,
                },

                Err(obstacles) => TransactionSerializabilityVerdict::Unproven { obstacles },
            };

            checks.push(TransactionSerializabilityCheck {
                operation: template.reference.operation.clone(),
                transaction: template.reference.transaction.clone(),
                location: template.reference.location.clone(),
                requirement,
                key: declared.key.clone(),
                verdict,
                artifacts: template.artifacts.clone(),
            });
        }
    }

    checks
}

/// Attempts both routes for the template at `root`. The ordering
/// verifier calls this too: an ordering obligation rests on the
/// serializability of the same closure, declared or not.
pub fn prove(
    index: &ConflictIndex<'_>,
    root: usize,
    key: &ValueRef,
) -> Result<TransactionSerializabilityProof, Vec<TransactionSerializabilityObstacle>> {
    let closure = index.closure(root);

    let references: Vec<TransactionRef> = closure
        .iter()
        .map(|&position| index.templates[position].reference.clone())
        .collect();

    let root_reference = index.templates[root].reference.clone();

    let weaker: Vec<IsolationFact> = closure
        .iter()
        .map(|&position| &index.templates[position])
        .filter(|template| template.transaction.isolation != TransactionIsolation::Serializable)
        .map(|template| IsolationFact {
            transaction: template.reference.clone(),
            isolation: template.transaction.isolation,
        })
        .collect();

    if weaker.is_empty() {
        return Ok(
            TransactionSerializabilityProof::SerializableIsolationClosure {
                root: root_reference,
                key: key.clone(),
                closure: references,
            },
        );
    }

    let dependencies = index.dependencies(&closure);
    let cycles = index.unconstrained_cycles(&closure, &dependencies);

    if cycles.is_empty() {
        return Ok(TransactionSerializabilityProof::ConflictGraph {
            root: root_reference,
            key: key.clone(),
            closure: references,
            dependencies,
        });
    }

    let mut obstacles = vec![
        TransactionSerializabilityObstacle::SerializableClosureContainsWeakerIsolation {
            transactions: weaker,
        },
    ];

    for cycle in cycles {
        obstacles.push(
            TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedCycle {
                cycle: cycle.members,
            },
        );

        for dependency in cycle.unconstrained {
            obstacles.push(match dependency.kind {
                DependencyKind::ReadWriteAntiDependency => {
                    TransactionSerializabilityObstacle::TransactionSerializabilityUnprotectedReadWriteDependency {
                        dependency,
                    }
                }

                DependencyKind::WriteRead | DependencyKind::WriteWrite => {
                    TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedDependency {
                        dependency,
                    }
                }
            });
        }
    }

    Err(obstacles)
}

impl TransactionSerializabilityCheck {
    /// Every obstacle names an application fact: isolation, a lock, a
    /// guarded mutation, a cursor. No runtime declaration can help.
    pub fn remedy(&self) -> Option<RemedyLayer> {
        match &self.verdict {
            TransactionSerializabilityVerdict::Proven { .. } => None,
            TransactionSerializabilityVerdict::Unproven { .. } => Some(RemedyLayer::Application),
        }
    }

    /// The diagnostic for an unproven requirement; a proven one
    /// produces none.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let TransactionSerializabilityVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let transaction = &self.transaction;
        let requirement = self.requirement;

        Some(Diagnostic {
            code: DiagnosticCode::Verification(
                VerificationCode::TransactionSerializabilityUnproven,
            ),
            severity: Severity::Unknown,
            subject: Some(transaction.clone()),
            message: format!(
                "Serializability requirement {requirement} of `{transaction}` in \
                 `{operation}` (SerializableBy({})) is not established: the committed \
                 state history of its conflict closure is not proven equivalent to a \
                 serial order.",
                value_ref_label(&self.key)
            ),
            evidence: obstacles
                .iter()
                .map(TransactionSerializabilityObstacle::evidence)
                .collect(),
        })
    }
}

impl TransactionSerializabilityObstacle {
    pub fn evidence(&self) -> Evidence {
        match self {
            Self::TransactionNotIndexed {
                operation,
                transaction,
            } => Evidence {
                subject: Some(transaction.clone()),
                message: format!(
                    "`{transaction}` of `{operation}` is not among the indexed transaction \
                     templates, so no conflict analysis covers it."
                ),
            },

            Self::SerializableClosureContainsWeakerIsolation { transactions } => Evidence {
                subject: transactions
                    .first()
                    .map(|fact| fact.transaction.transaction.clone()),
                message: format!(
                    "The serializable-isolation route does not apply: {} in the conflict \
                     closure declare{} weaker isolation than serializable ({}). Every \
                     transaction that may conflict must be serializable for that route, \
                     since a serializable transaction is serializable only with respect \
                     to other serializable ones.",
                    if transactions.len() == 1 {
                        "one transaction".to_string()
                    } else {
                        format!("{} transactions", transactions.len())
                    },
                    if transactions.len() == 1 { "s" } else { "" },
                    transactions
                        .iter()
                        .map(|fact| format!(
                            "`{}` is {}",
                            fact.transaction,
                            isolation_label(fact.isolation)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },

            Self::TransactionSerializabilityUnconstrainedCycle { cycle } => Evidence {
                subject: cycle.first().map(|member| member.transaction.clone()),
                message: format!(
                    "A cyclic conflict component through {} contains a dependency no \
                     declared fact commit-orders, so a non-serializable committed \
                     history through that chain cannot be excluded.",
                    chain(cycle)
                ),
            },

            Self::TransactionSerializabilityUnprotectedReadWriteDependency { dependency } => {
                Evidence {
                    subject: Some(dependency.source.transaction.clone()),
                    message: format!(
                        "{}: `{}` may read `{}` (step {}) before `{}` {} (step {}), \
                         and neither strict locking nor an atomic conditional mutation \
                         constrains the commit order — the anti-dependency behind write \
                         skew. {}",
                        capitalize(&dependency.kind.to_string()),
                        dependency.source,
                        dependency.object,
                        dependency.source_step + 1,
                        dependency.target,
                        write_phrase(dependency.target_mode),
                        dependency.target_step + 1,
                        gap_sentences(&dependency.gaps)
                    ),
                }
            }

            Self::TransactionSerializabilityUnconstrainedDependency { dependency } => Evidence {
                subject: Some(dependency.source.transaction.clone()),
                message: format!(
                    "{}: `{}` ({}, step {}) and `{}` ({}, step {}) on `{}` have no \
                     commit-order evidence. {}",
                    capitalize(&dependency.kind.to_string()),
                    dependency.source,
                    dependency.source_mode,
                    dependency.source_step + 1,
                    dependency.target,
                    dependency.target_mode,
                    dependency.target_step + 1,
                    dependency.object,
                    gap_sentences(&dependency.gaps)
                ),
            },
        }
    }
}

/// A transaction chain as a sentence names it: `a → b → a`.
pub fn chain(members: &[TransactionRef]) -> String {
    let mut labels: Vec<String> = members
        .iter()
        .map(|member| format!("`{}`", member.transaction))
        .collect();

    if let Some(first) = labels.first().cloned() {
        labels.push(first);
    }

    labels.join(" → ")
}

/// The gaps of an unconstrained dependency, as sentences.
pub fn gap_sentences(gaps: &[DependencyGap]) -> String {
    if gaps.is_empty() {
        return String::new();
    }

    gaps.iter()
        .map(|gap| gap_sentence(gap) + ".")
        .collect::<Vec<_>>()
        .join(" ")
}

fn gap_sentence(gap: &DependencyGap) -> String {
    match gap {
        DependencyGap::LockCoverageMissing {
            transaction,
            side,
            object,
            step,
        } => format!(
            "Lock coverage is missing on the {side} side: `{transaction}` holds no \
             {} covering its step-{} access to `{object}`",
            match side {
                super::transaction_conflicts::DependencySide::Reader => "shared or exclusive lock",
                super::transaction_conflicts::DependencySide::Writer => "exclusive lock",
            },
            step + 1
        ),

        DependencyGap::LockAcquiredAfterProtectedAccess {
            transaction,
            side,
            lock_step,
            access_step,
        } => format!(
            "The {side}-side lock of `{transaction}` is acquired at step {} — after the \
             step-{} access it would have to protect, and a lock protects no earlier \
             observation",
            lock_step + 1,
            access_step + 1
        ),

        DependencyGap::ObservedStateGuardMissing {
            transaction,
            object,
            step,
        } => format!(
            "`{transaction}` reads this `{object}` instance at step {} and later commits a \
             decision that can conflict with another writer, but it does not lock the \
             observation, run in a serializable closure, or condition a mutation of the \
             instance on the observed state — a stale observation can still participate in \
             a successful commit",
            step + 1
        ),

        DependencyGap::ObservedStateGuardDoesNotCoverConflict {
            transaction,
            object,
            step,
            guard_step,
            fields,
        } => format!(
            "The guarded mutation of `{transaction}` at step {} compares part of what its \
             step-{} read of `{object}` observed, but not {fields}, which the conflict \
             touches; compare those fields against their observed values, or the object's \
             version against the observed version",
            guard_step + 1,
            step + 1
        ),

        DependencyGap::ObservedStateNotIdentified {
            transaction,
            object,
            step,
        } => format!(
            "`{transaction}` observes a set of `{object}` instances at step {}, not one \
             identified instance, and a conditional mutation guards only the instance it \
             identifies: a concurrent insert of a new matching instance escapes it, so only \
             a lock or serializable isolation protects this observation",
            step + 1
        ),

        DependencyGap::ObservedVersionMayRepeat {
            transaction,
            object,
            step,
            deleted_by,
        } => format!(
            "`{transaction}` guards its step-{} read of `{object}` by the observed version, \
             but the conflict is an insertion and `{deleted_by}` deletes `{object}` \
             instances: an instance inserted after a deletion establishes its token afresh \
             and may repeat the observed one",
            step + 1
        ),

        DependencyGap::IsolationUnspecified { transaction } => format!(
            "`{transaction}` declares no isolation, so nothing says it observes only \
             committed writes or installs conflicting writes in commit order"
        ),

        DependencyGap::TransactionConflictUnknownSelectorOverlap { object } => format!(
            "The selected `{object}` domains could not be proven disjoint, so the \
             dependency is assumed (unknown selector overlap is never treated as \
             disjoint)"
        ),

        DependencyGap::TransactionConflictUnknownFieldOverlap { object } => format!(
            "A field footprint on `{object}` is undeclared, so the dependency is \
             assumed (unknown field overlap is never treated as disjoint)"
        ),
    }
}

/// The evidence constraining one dependency, as a sentence.
pub fn evidence_sentence(dependency: &DependencyEvidence) -> String {
    let edge = format!(
        "the {} from `{}` (step {}) to `{}` (step {}) on `{}`",
        dependency.kind,
        dependency.source,
        dependency.source_step + 1,
        dependency.target,
        dependency.target_step + 1,
        dependency.object
    );

    match &dependency.evidence {
        CommitOrderEvidence::IntrinsicCommittedRead { isolation } => format!(
            "{edge} is commit-ordered: `{}` reads under {} isolation, so it observes \
             only writes that had already committed",
            dependency.target,
            isolation_label(*isolation)
        ),

        CommitOrderEvidence::AtomicWriteOrder {
            source_isolation,
            target_isolation,
        } => format!(
            "{edge} is commit-ordered: conflicting writes under declared isolation \
             ({} and {}) are installed in commit order",
            isolation_label(*source_isolation),
            isolation_label(*target_isolation)
        ),

        CommitOrderEvidence::StrictLock {
            reader_lock,
            writer_lock,
        } => format!(
            "{edge} is commit-ordered by strict locking: `{}` locks the domain ({}) at \
             step {} before observing it, `{}` acquires an exclusive lock at step {} \
             before mutating it, and both hold their locks to termination, so the \
             reader commits before the writer can acquire",
            reader_lock.transaction,
            match reader_lock.mode {
                crate::spec::LockMode::Shared => "shared",
                crate::spec::LockMode::Exclusive => "exclusive",
            },
            reader_lock.step + 1,
            writer_lock.transaction,
            writer_lock.step + 1
        ),

        CommitOrderEvidence::AtomicConditionalMutation {
            guarded_by,
            step,
            object,
            mechanism,
            guard,
            compared_fields,
        } => {
            let at = format!(
                "`{guarded_by}`'s {mechanism} of `{object}` at step {}",
                step + 1
            );

            match guard {
                GuardCoverage::Atomic => format!(
                    "{edge} is commit-ordered by {at}: the access is part of that atomic \
                     conditional mutation, whose write protection is held to commit"
                ),

                GuardCoverage::HeldProtection => format!(
                    "{edge} is commit-ordered by {at}: the access follows it, under the \
                     write protection the mutation holds on the instance to commit"
                ),

                GuardCoverage::ObservedState { read, .. } => format!(
                    "{edge} is commit-ordered by {at}: it compares {} against the values \
                     `{read}` observed, atomically with the mutation, so a stale observation \
                     rejects instead of committing",
                    field_list(compared_fields)
                ),

                GuardCoverage::ObservedVersion {
                    read,
                    version_field,
                    ..
                } => format!(
                    "{edge} is commit-ordered by {at}: it conditions its mutation on the \
                     version `{read}` observed (`{version_field}`). Every committed mutation \
                     of the versioned instance publishes a newer token, so a stale \
                     observation cannot participate in a successful commit"
                ),

                GuardCoverage::LockedReader { reader_lock } => format!(
                    "{edge} is commit-ordered: `{}` locks the instance at step {} before \
                     observing it and holds the lock to termination, and {at} must acquire \
                     the instance's write protection, so the reader commits first",
                    reader_lock.transaction,
                    reader_lock.step + 1
                ),
            }
        }

        CommitOrderEvidence::ReadOnlyObservation { isolation } => format!(
            "{edge} needs no mechanism: `{}` only reads, and observes committed state at one \
             instant under {} isolation, which is its serialization point — every write it \
             observed committed before it and every write it missed commits after it, so no \
             cycle passes through it",
            dependency.source,
            isolation_label(*isolation)
        ),

        CommitOrderEvidence::OrderedCursor {
            object,
            field,
            rule,
        } => format!(
            "{edge} is commit-ordered by the cursor `{object}.{field}` ({rule}): an \
             older accepted position cannot commit after a newer one"
        ),

        CommitOrderEvidence::None => {
            format!("{edge} lies on no cyclic component and needs no commit-order evidence")
        }
    }
}

fn field_list(fields: &std::collections::BTreeSet<crate::spec::FieldPath>) -> String {
    fields
        .iter()
        .map(|field| format!("`{field}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

/// The verb phrase for how a write-capable access installs its
/// conflicting change, so a phantom-shaped conflict (a predicate read
/// racing a concurrent insert or delete) does not read as ordinary
/// write skew.
fn write_phrase(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Insert => "inserts a matching instance",
        AccessMode::Delete => "deletes the matching instance",
        AccessMode::UpsertReadWrite => "upserts it",
        AccessMode::VersionPublish => "publishes a newer version of it",
        _ => "writes it",
    }
}

fn capitalize(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}
