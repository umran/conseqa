//! Verification of transaction ordering requirements (§9, §54–§58 of
//! the DSL v4 revision).
//!
//! > `OrderedBy(K, P)`: within each domain identified by `key`, the
//! > committed executions of the transaction take effect in the order
//! > of their `position` values.
//!
//! The proof has two legs. First, the transaction's conflict closure
//! must be serializable — the same argument the serializability
//! verifier makes, invoked here whether or not the designer declared
//! `SerializableBy(K)` beside the ordering requirement. Second, the
//! transaction must persist the position through a state-level commit
//! guard whose accepted values order the commits:
//!
//! - **cursor** (§55): an `advance_cursor` step whose `incoming` is
//!   canonically the requirement's position, over a cursor whose
//!   selector the key identifies, advanced under one rule by every
//!   template that mutates it and by nothing else; or
//! - **fence** (§56): a `fence` step whose `token` is the position,
//!   over a fence field no ordinary write touches. Equal tokens
//!   establish no order, which is the fence's declared limit.
//!
//! No runtime fact is ever a route (§57): transport precedence, member
//! assignment, and member concurrency describe how work ordinarily
//! arrives, and none of them survives redelivery, worker replacement,
//! or reordering after failure. The cursor or fence does, because an
//! out-of-order or stale execution rejects before it can commit.

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{
    CursorAdvanceRule, Id, Model, ObjectSelector, StepLocation, TransactionOrderingRequirement,
    TransactionStep, ValueRef,
};

use super::transaction_conflicts::{
    CommitArtifact, ConflictIndex, ManagedFieldRef, TransactionRef,
};
use super::transaction_serializability::{
    TransactionSerializabilityObstacle, TransactionSerializabilityProof,
};
use super::{ProofScope, RemedyLayer};

/// The verdict for one declared transaction ordering requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionOrderingCheck {
    pub operation: Id,
    pub transaction: Id,
    pub location: StepLocation,

    /// Index into `transaction.requirements.ordering`.
    pub requirement: usize,

    pub key: ValueRef,
    pub position: ValueRef,

    pub verdict: TransactionOrderingVerdict,

    /// The artifacts the transaction commits beside its mutations —
    /// outbox admissions, ordinary and transition-scoped — reported so
    /// a reader sees what the ordered commit carries, and what it does
    /// not claim about later consumption.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<CommitArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionOrderingVerdict {
    Proven {
        proof: TransactionOrderingProof,
        scope: ProofScope,
    },
    Unproven {
        obstacles: Vec<TransactionOrderingObstacle>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionOrderingProof {
    /// The position is persisted through an ordered cursor the key
    /// identifies, over a serializable conflict closure.
    Cursor {
        key: ValueRef,
        position: ValueRef,
        cursor: ManagedFieldRef,
        rule: CursorAdvanceRule,

        /// The step of the transaction that advances the cursor.
        step: usize,

        serializability: Box<TransactionSerializabilityProof>,
    },

    /// The position is a fencing token over a fence the key
    /// identifies, over a serializable conflict closure.
    Fence {
        key: ValueRef,
        position: ValueRef,
        fence: ManagedFieldRef,

        /// The step of the transaction that fences.
        step: usize,

        serializability: Box<TransactionSerializabilityProof>,
    },
}

impl TransactionOrderingProof {
    /// Ordering rests on the transaction's own commit guard and the
    /// closure's serializability, both L0.
    pub fn scope(&self) -> ProofScope {
        ProofScope::L0Only
    }

    pub fn serializability(&self) -> &TransactionSerializabilityProof {
        match self {
            Self::Cursor {
                serializability, ..
            }
            | Self::Fence {
                serializability, ..
            } => serializability,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionOrderingObstacle {
    /// The requirement's transaction is not in the access index.
    TransactionNotIndexed { operation: Id, transaction: Id },

    /// The conflict closure is not proven serializable, so no order of
    /// commits is an order of state.
    OrderingMissingSerializability {
        obstacles: Vec<TransactionSerializabilityObstacle>,
    },

    /// No `advance_cursor` or `fence` step persists the position.
    OrderingMissingCursorOrFence,

    /// A cursor or fence step exists, but its incoming value is not
    /// canonically the requirement's position.
    OrderingPositionMismatch { step: usize, incoming: ValueRef },

    /// The cursor or fence selector is not identified by the
    /// requirement key: equal keys do not select one guarded
    /// instance, or different keys may share one.
    OrderingKeyDomainMismatch { step: usize, object: Id },

    /// The managed field is written outside the protocol — by an
    /// ordinary write, or by a cursor advance under another rule —
    /// so accepted positions do not order every commit.
    OrderingUncontrolledManagedFieldWriter {
        transaction: TransactionRef,
        step: usize,
        field: ManagedFieldRef,
    },
}

/// Checks every transaction ordering requirement declared by the
/// model.
pub fn check(model: &Model) -> Vec<TransactionOrderingCheck> {
    let index = ConflictIndex::build(model);

    let mut checks = Vec::new();

    for (position, template) in index.templates.iter().enumerate() {
        for (requirement, declared) in template
            .transaction
            .requirements
            .ordering
            .iter()
            .enumerate()
        {
            let verdict = match prove(&index, position, declared) {
                Ok(proof) => TransactionOrderingVerdict::Proven {
                    scope: proof.scope(),
                    proof,
                },

                Err(obstacles) => TransactionOrderingVerdict::Unproven { obstacles },
            };

            checks.push(TransactionOrderingCheck {
                operation: template.reference.operation.clone(),
                transaction: template.reference.transaction.clone(),
                location: template.reference.location.clone(),
                requirement,
                key: declared.key.clone(),
                position: declared.position.clone(),
                verdict,
                artifacts: template.artifacts.clone(),
            });
        }
    }

    checks
}

/// The guard step of the template that persists the position.
enum Route<'a> {
    Cursor {
        step: usize,
        target: &'a ObjectSelector,
        field: &'a crate::spec::FieldPath,
        rule: CursorAdvanceRule,
    },

    Fence {
        step: usize,
        target: &'a ObjectSelector,
        field: &'a crate::spec::FieldPath,
    },
}

fn prove(
    index: &ConflictIndex<'_>,
    position: usize,
    requirement: &TransactionOrderingRequirement,
) -> Result<TransactionOrderingProof, Vec<TransactionOrderingObstacle>> {
    let template = &index.templates[position];

    let Some(operation) = index.model.operations.get(&template.reference.operation) else {
        return Err(vec![TransactionOrderingObstacle::TransactionNotIndexed {
            operation: template.reference.operation.clone(),
            transaction: template.reference.transaction.clone(),
        }]);
    };

    let mut obstacles = Vec::new();

    // The route: the first guard whose incoming value is the position.
    let mut route = None;
    let mut mismatches = Vec::new();

    for (step, inner) in template.transaction.steps.iter().enumerate() {
        match inner {
            TransactionStep::AdvanceCursor(advance) => {
                if index.same_value(operation, &advance.incoming, &requirement.position) {
                    route = Some(Route::Cursor {
                        step,
                        target: &advance.target,
                        field: &advance.field,
                        rule: advance.rule,
                    });

                    break;
                }

                mismatches.push(TransactionOrderingObstacle::OrderingPositionMismatch {
                    step,
                    incoming: advance.incoming.clone(),
                });
            }

            TransactionStep::Fence(fence) => {
                if index.same_value(operation, &fence.token, &requirement.position) {
                    route = Some(Route::Fence {
                        step,
                        target: &fence.target,
                        field: &fence.field,
                    });

                    break;
                }

                mismatches.push(TransactionOrderingObstacle::OrderingPositionMismatch {
                    step,
                    incoming: fence.token.clone(),
                });
            }

            _ => {}
        }
    }

    match &route {
        Some(Route::Cursor {
            step,
            target,
            field,
            rule,
        }) => {
            if !index.key_identifies_domain(operation, target, &requirement.key) {
                obstacles.push(TransactionOrderingObstacle::OrderingKeyDomainMismatch {
                    step: *step,
                    object: target.object.clone(),
                });
            }

            for (transaction, step) in
                index.uncontrolled_managed_writers(&target.object, field, Some(*rule))
            {
                obstacles.push(
                    TransactionOrderingObstacle::OrderingUncontrolledManagedFieldWriter {
                        transaction,
                        step,
                        field: ManagedFieldRef {
                            object: target.object.clone(),
                            field: (*field).clone(),
                        },
                    },
                );
            }
        }

        Some(Route::Fence {
            step,
            target,
            field,
        }) => {
            if !index.key_identifies_domain(operation, target, &requirement.key) {
                obstacles.push(TransactionOrderingObstacle::OrderingKeyDomainMismatch {
                    step: *step,
                    object: target.object.clone(),
                });
            }

            for (transaction, step) in
                index.uncontrolled_managed_writers(&target.object, field, None)
            {
                obstacles.push(
                    TransactionOrderingObstacle::OrderingUncontrolledManagedFieldWriter {
                        transaction,
                        step,
                        field: ManagedFieldRef {
                            object: target.object.clone(),
                            field: (*field).clone(),
                        },
                    },
                );
            }
        }

        None => {
            if mismatches.is_empty() {
                obstacles.push(TransactionOrderingObstacle::OrderingMissingCursorOrFence);
            } else {
                obstacles.extend(mismatches);
            }
        }
    }

    let serializability =
        match super::transaction_serializability::prove(index, position, &requirement.key) {
            Ok(proof) => Some(proof),

            Err(nested) => {
                obstacles.push(
                    TransactionOrderingObstacle::OrderingMissingSerializability {
                        obstacles: nested,
                    },
                );

                None
            }
        };

    if !obstacles.is_empty() {
        return Err(obstacles);
    }

    let serializability = Box::new(serializability.expect("no obstacle recorded"));

    Ok(match route.expect("no obstacle recorded") {
        Route::Cursor {
            step,
            target,
            field,
            rule,
        } => TransactionOrderingProof::Cursor {
            key: requirement.key.clone(),
            position: requirement.position.clone(),
            cursor: ManagedFieldRef {
                object: target.object.clone(),
                field: field.clone(),
            },
            rule,
            step,
            serializability,
        },

        Route::Fence {
            step,
            target,
            field,
        } => TransactionOrderingProof::Fence {
            key: requirement.key.clone(),
            position: requirement.position.clone(),
            fence: ManagedFieldRef {
                object: target.object.clone(),
                field: field.clone(),
            },
            step,
            serializability,
        },
    })
}

impl TransactionOrderingCheck {
    /// Every obstacle names an application fact.
    pub fn remedy(&self) -> Option<RemedyLayer> {
        match &self.verdict {
            TransactionOrderingVerdict::Proven { .. } => None,
            TransactionOrderingVerdict::Unproven { .. } => Some(RemedyLayer::Application),
        }
    }

    /// The diagnostic for an unproven requirement; a proven one
    /// produces none.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let TransactionOrderingVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let transaction = &self.transaction;
        let requirement = self.requirement;

        Some(Diagnostic {
            code: DiagnosticCode::Verification(VerificationCode::TransactionOrderingUnproven),
            severity: Severity::Unknown,
            subject: Some(transaction.clone()),
            message: format!(
                "Ordering requirement {requirement} of `{transaction}` in `{operation}` \
                 (OrderedBy({}, {})) is not established: committed executions sharing \
                 the key are not proven to take effect in position order.",
                value_ref_label(&self.key),
                value_ref_label(&self.position)
            ),
            evidence: obstacles
                .iter()
                .flat_map(TransactionOrderingObstacle::evidence)
                .collect(),
        })
    }
}

impl TransactionOrderingObstacle {
    pub fn evidence(&self) -> Vec<Evidence> {
        match self {
            Self::TransactionNotIndexed {
                operation,
                transaction,
            } => vec![Evidence {
                subject: Some(transaction.clone()),
                message: format!(
                    "`{transaction}` of `{operation}` is not among the indexed transaction \
                     templates, so no conflict analysis covers it."
                ),
            }],

            Self::OrderingMissingSerializability { obstacles } => {
                let mut evidence = vec![Evidence {
                    subject: None,
                    message: "Ordering rests on the serializability of the transaction's \
                              conflict closure, which is not established:"
                        .to_string(),
                }];

                evidence.extend(
                    obstacles
                        .iter()
                        .map(TransactionSerializabilityObstacle::evidence),
                );

                evidence
            }

            Self::OrderingMissingCursorOrFence => vec![Evidence {
                subject: None,
                message: "No `advance_cursor` or `fence` step of the transaction persists \
                          the position: without a state-level commit guard, nothing rejects \
                          an out-of-order or stale execution before it commits. Transport \
                          precedence and worker topology are not routes — they do not \
                          survive redelivery, timeout, worker replacement, or reordering \
                          after failure."
                    .to_string(),
            }],

            Self::OrderingPositionMismatch { step, incoming } => vec![Evidence {
                subject: None,
                message: format!(
                    "The guard at step {} advances by `{}`, which is not canonically the \
                     requirement's position.",
                    step + 1,
                    value_ref_label(incoming)
                ),
            }],

            Self::OrderingKeyDomainMismatch { step, object } => vec![Evidence {
                subject: Some(object.clone()),
                message: format!(
                    "The guard at step {} selects `{object}` by a domain the requirement \
                     key does not identify: every identity field of the object must be \
                     pinned by a literal or by the key itself, so equal keys guard one \
                     instance and different keys never share one.",
                    step + 1
                ),
            }],

            Self::OrderingUncontrolledManagedFieldWriter {
                transaction,
                step,
                field,
            } => vec![Evidence {
                subject: Some(transaction.transaction.clone()),
                message: format!(
                    "`{transaction}` writes the managed field `{field}` at step {} outside \
                     the protocol — an ordinary write, or an advance under another rule — \
                     so accepted positions do not order every commit of the domain.",
                    step + 1
                ),
            }],
        }
    }
}

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}
