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
//!   declared isolation, strict S/X locking, version validation, or a
//!   shared ordered cursor. An apparent cycle every edge of which is
//!   constrained would imply a cycle in strict commit order, so it
//!   cannot occur in a committed history.
//!
//! Anti-dependencies are what make the second route necessary:
//! repeatable or snapshot-stable reads still admit write skew, so
//! snapshot stability never by itself proves serializability. Every
//! refusal names the concrete transaction chain and the unconstrained
//! dependencies on it. All evidence here is L0: no runtime fact
//! participates, and no runtime fact could.

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{Id, Model, StepLocation, TransactionIsolation, ValueRef};

use super::transaction_conflicts::{
    CommitArtifact, CommitOrderEvidence, ConflictIndex, DependencyEvidence, DependencyGap,
    DependencyKind, TransactionRef, isolation_label,
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

    /// A read-write anti-dependency on such a cycle that neither
    /// strict locking nor version validation constrains.
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
    /// version protocol, a cursor. No runtime declaration can help.
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
                        "{}: `{}` may read `{}` (step {}) before `{}` writes it (step {}), \
                         and neither strict locking nor version validation constrains \
                         the commit order — the anti-dependency behind write skew. {}",
                        capitalize(&dependency.kind.to_string()),
                        dependency.source,
                        dependency.object,
                        dependency.source_step + 1,
                        dependency.target,
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

        DependencyGap::VersionValidationMissing {
            transaction,
            object,
        } => format!(
            "`{transaction}` does not validate the version of `{object}` it observed at \
             commit, so a stale observation can still participate in a successful commit"
        ),

        DependencyGap::VersionBumpMissing {
            transaction,
            object,
        } => format!(
            "`{transaction}` mutates `{object}` without advancing the version the other \
             side validates"
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

        CommitOrderEvidence::VersionValidation {
            validated_by,
            object,
            field,
        } => format!(
            "{edge} is commit-ordered by version validation: `{validated_by}` validates \
             `{object}.{field}` at commit against the version it observed, and the \
             conflicting mutation advances that version, so a stale observation \
             rejects instead of committing"
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

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

fn capitalize(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}
