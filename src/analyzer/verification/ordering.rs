//! Verification of operation ordering requirements (§9 of the
//! semantics contract).
//!
//! > Same-key invocations for which a meaningful logical precedence
//! > exists must preserve that precedence through the operation's
//! > semantically relevant execution.
//!
//! Ordering is strictly stronger than serialization: a serialization
//! proof establishes non-overlap and says nothing about which same-key
//! invocation comes first. A proof must therefore say where the
//! precedence comes from *and* show that the execution mechanism
//! preserves it.
//!
//! ## Precedence
//!
//! V1 recognizes one precedence source: the order the subscribed
//! topic's runtime declares. A keyed transport orders same-key
//! messages, which is a precedence *for the ordering key* only when
//! that key is established to carry the topic key for every admitted
//! schema — the key identity the serialization verifier already
//! computes. A global transport orders every message, so any key
//! inherits it.
//!
//! Request boundaries have no precedence source at all. Routing a
//! request by a semantic key and running its domain on a serial member
//! establishes serialization, never ordering: arrival order of
//! unmodeled callers is not a logical precedence, so there is nothing
//! for the mechanism to preserve. A request-side ordering requirement
//! is unproven until the DSL grows a precedence source for requests.
//!
//! ## Mechanism
//!
//! Same-key deliveries route to one semantic domain (`topic_key`),
//! that domain has one active owning member (`MemberAssignment`), and
//! the member executes one invocation at a time
//! (`member_concurrency = bounded(1)`), so a later invocation cannot
//! overtake an earlier one.
//!
//! Dispatch additionally carries an order-preservation obligation: a
//! conforming runtime must not establish delivery A before B for the
//! same ordered key, leave A semantically incomplete, and then admit B
//! in a way that lets it overtake A — including through
//! failure-driven redelivery and ownership reassignment. That
//! obligation is what replaces the logical-lane semantics of the
//! previous model. A duplicate of an already completed delivery is a
//! repeated attempt at an invocation that took effect in order, whose
//! work is the idempotency requirement's obligation rather than
//! ordering's; the proof records which requirement covers it, or that
//! none does.
//!
//! Every proof here consumes topic-runtime, dispatch, and pool facts,
//! so an ordering proof is always `RuntimeDependent` — except the
//! vacuous one, which needs no facts at all.

use serde::{Deserialize, Serialize};

use crate::spec::{
    DeliverySemantics, FieldPath, Id, Input, MemberAssignment, MemberConcurrency, Model, Operation,
    OrderingRequirement, SubscriptionRoutingKey, TopicOrdering, ValueRef, ValueSource,
};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};

use super::ProofScope;
use super::describe::describe_value_ref;
use super::idempotency::{IdempotencyCheck, IdempotencyVerdict};
use super::serialization::{
    MessageKeyFact, SerializationObstacle, admits_no_messages, pool_is_serial, topic_key_facts,
};

/// The verdict for one declared ordering requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderingCheck {
    pub operation: Id,

    /// Index into `operation.requirements.ordering`.
    pub requirement: usize,

    /// The requirement's key, copied so the check is self-contained.
    pub key: ValueRef,

    pub verdict: OrderingVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrderingVerdict {
    Proven {
        proof: OrderingProof,
        scope: ProofScope,
    },
    Unproven {
        obstacles: Vec<OrderingObstacle>,
    },
}

impl OrderingVerdict {
    fn proven(proof: OrderingProof) -> Self {
        Self::Proven {
            scope: proof.scope(),
            proof,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrderingProof {
    /// The key's subscription input admits no message schemas, so no
    /// invocation bears the key and no precedence exists to preserve.
    NoAdmittedInvocations { input: Id },

    /// The topic runtime's declared order is the precedence; one
    /// owning member at concurrency one preserves it; redelivery
    /// cannot reorder it.
    RoutedOrder {
        input: Id,
        topic: Id,
        pool: Id,
        precedence: PrecedenceSource,
        routing_key: SubscriptionRoutingKey,
        member_assignment: MemberAssignment,
        duplicates: DuplicateHandling,
    },
}

impl OrderingProof {
    pub fn scope(&self) -> ProofScope {
        match self {
            Self::NoAdmittedInvocations { .. } => ProofScope::L0Only,
            Self::RoutedOrder { .. } => ProofScope::RuntimeDependent,
        }
    }
}

/// Where the preserved precedence comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrecedenceSource {
    /// The topic runtime orders same-key messages, and the ordering
    /// key is established to carry the topic key for every admitted
    /// schema.
    KeyedTopic { message_keys: Vec<MessageKeyFact> },

    /// The topic runtime orders every message, so same-key messages
    /// are ordered whatever the key.
    GlobalTopic,
}

/// Why redelivery cannot invert the precedence, and who answers for
/// the work a duplicate does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DuplicateHandling {
    /// `at_most_once` delivery: a logical message is never delivered
    /// again, so neither redelivery nor a duplicate exists.
    SingleDelivery,

    /// Dispatch must preserve the transport's established same-key
    /// precedence when admitting invocations to execution, including
    /// across failure-driven redelivery and ownership reassignment, so
    /// a redelivered message cannot be overtaken by a later one. A
    /// duplicate of a completed delivery is a repeated attempt at an
    /// invocation that already took effect in order, and what it does
    /// is the idempotency requirement's obligation — `idempotency`
    /// names the requirement keyed from this input when one is
    /// declared.
    OrderPreservingRedelivery {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idempotency: Option<DuplicateCoverage>,
    },
}

/// The idempotency requirement that answers for duplicate attempts
/// through an input, and its verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateCoverage {
    pub requirement: usize,
    pub proven: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrderingObstacle {
    /// The key is not sourced from an input declared by the
    /// operation, so no routing fact selects which invocations share
    /// it.
    KeyNotFromInput { source: ValueSource },

    /// Key-bearing invocations arrive through a request input, and
    /// the DSL declares no precedence fact for requests. Routing plus
    /// serial member concurrency establishes serialization only; it
    /// does not invent an order among independent requests.
    RequestInputHasNoPrecedenceSource { input: Id },

    /// The subscribed topic's runtime declares no order that could
    /// serve as the precedence.
    TopicOrderingProvidesNoPrecedence {
        input: Id,
        topic: Id,
        declared: TopicOrdering,
    },

    /// The keyed topic declares no ordering-key mapping for an
    /// admitted schema.
    TopicKeyMappingMissing { input: Id, topic: Id, schema: Id },

    /// The topic's order is per topic key, and the ordering key is
    /// not established to carry it for this schema, so the declared
    /// order says nothing about same-key invocations.
    KeyIdentityUnestablished {
        input: Id,
        topic: Id,
        schema: Id,
        topic_key: FieldPath,
    },

    /// The subscription declares no runtime, so nothing says where its
    /// deliveries execute or in what order they are admitted.
    NoSubscriptionRuntime { input: Id },

    /// Dispatch assigns deliveries to a pool but declares no routing,
    /// so same-key deliveries may be owned by different members and
    /// dispatched out of order.
    RoutingAbsent { input: Id, pool: Id },

    /// The target execution pool is not declared, so its member
    /// concurrency is unknown.
    PoolUndeclared { input: Id, pool: Id },

    /// The pool's declared member concurrency admits overlap, so a
    /// later invocation may overtake an earlier one.
    MemberConcurrencyNotSerial {
        input: Id,
        pool: Id,
        declared: MemberConcurrency,
    },
}

/// Checks every ordering requirement declared by the model. The
/// idempotency verdicts are read only to record which requirement
/// answers for duplicate attempts; no ordering verdict depends on
/// them.
pub fn check(model: &Model, idempotency: &[IdempotencyCheck]) -> Vec<OrderingCheck> {
    let mut checks = Vec::new();

    for (operation_id, operation) in &model.operations {
        for (index, requirement) in operation.requirements.ordering.iter().enumerate() {
            checks.push(OrderingCheck {
                operation: operation_id.clone(),
                requirement: index,
                key: requirement.key.clone(),
                verdict: check_requirement(
                    model,
                    operation_id,
                    operation,
                    requirement,
                    idempotency,
                ),
            });
        }
    }

    checks
}

fn check_requirement(
    model: &Model,
    operation_id: &Id,
    operation: &Operation,
    requirement: &OrderingRequirement,
    idempotency: &[IdempotencyCheck],
) -> OrderingVerdict {
    let ValueSource::Input(input_id) = &requirement.key.source else {
        return OrderingVerdict::Unproven {
            obstacles: vec![OrderingObstacle::KeyNotFromInput {
                source: requirement.key.source.clone(),
            }],
        };
    };

    let subscription = match operation.inputs.get(input_id) {
        Some(Input::Subscription(subscription)) => subscription,

        Some(Input::Request(_)) => {
            return OrderingVerdict::Unproven {
                obstacles: vec![OrderingObstacle::RequestInputHasNoPrecedenceSource {
                    input: input_id.clone(),
                }],
            };
        }

        None => {
            return OrderingVerdict::Unproven {
                obstacles: vec![OrderingObstacle::KeyNotFromInput {
                    source: requirement.key.source.clone(),
                }],
            };
        }
    };

    if admits_no_messages(model, subscription) {
        return OrderingVerdict::proven(OrderingProof::NoAdmittedInvocations {
            input: input_id.clone(),
        });
    }

    let topic_id = subscription.topic.clone();
    let mut obstacles = Vec::new();

    // The precedence source: the topic runtime's declared order, for
    // this key.
    let ordering = model.topic_ordering(&topic_id);

    let precedence = match &ordering {
        TopicOrdering::Keyed(_) => {
            match topic_key_facts(model, input_id, subscription, &requirement.key.path) {
                Ok((_, message_keys)) => Some(PrecedenceSource::KeyedTopic { message_keys }),

                Err(serialization_obstacles) => {
                    for obstacle in serialization_obstacles {
                        obstacles.push(match obstacle {
                            SerializationObstacle::TopicKeyMappingMissing {
                                input,
                                topic,
                                schema,
                            } => OrderingObstacle::TopicKeyMappingMissing {
                                input,
                                topic,
                                schema,
                            },

                            SerializationObstacle::KeyIdentityUnestablished {
                                input,
                                topic,
                                schema,
                                topic_key,
                            } => OrderingObstacle::KeyIdentityUnestablished {
                                input,
                                topic,
                                schema,
                                topic_key,
                            },

                            _ => OrderingObstacle::TopicOrderingProvidesNoPrecedence {
                                input: input_id.clone(),
                                topic: topic_id.clone(),
                                declared: ordering.clone(),
                            },
                        });
                    }

                    None
                }
            }
        }

        TopicOrdering::Global => Some(PrecedenceSource::GlobalTopic),

        TopicOrdering::Unspecified | TopicOrdering::Unordered => {
            obstacles.push(OrderingObstacle::TopicOrderingProvidesNoPrecedence {
                input: input_id.clone(),
                topic: topic_id.clone(),
                declared: ordering.clone(),
            });

            None
        }
    };

    // The mechanism: one routing domain per key, one owning member,
    // one invocation at a time on that member.
    let Some(runtime) = model.subscription_runtime(operation_id, input_id) else {
        obstacles.push(OrderingObstacle::NoSubscriptionRuntime {
            input: input_id.clone(),
        });

        return OrderingVerdict::Unproven { obstacles };
    };

    let pool_id = runtime.dispatch.pool.clone();

    let assignment = match &runtime.dispatch.routing {
        None => {
            obstacles.push(OrderingObstacle::RoutingAbsent {
                input: input_id.clone(),
                pool: pool_id.clone(),
            });

            None
        }

        // `topic_key` is the only routing key a subscription may
        // declare, and validation admits it only against a keyed topic
        // runtime — which is exactly the precedence source above, so
        // the routing fact and the precedence fact are about one
        // domain.
        Some(routing) => match routing.key {
            SubscriptionRoutingKey::TopicKey => Some((routing.key, routing.member_assignment)),
        },
    };

    let mut serialization_obstacles = Vec::new();
    let serial = pool_is_serial(model, input_id, &pool_id, &mut serialization_obstacles);

    for obstacle in serialization_obstacles {
        obstacles.push(match obstacle {
            SerializationObstacle::PoolUndeclared { input, pool } => {
                OrderingObstacle::PoolUndeclared { input, pool }
            }

            SerializationObstacle::MemberConcurrencyNotSerial {
                input,
                pool,
                declared,
            } => OrderingObstacle::MemberConcurrencyNotSerial {
                input,
                pool,
                declared,
            },

            _ => OrderingObstacle::PoolUndeclared {
                input: input_id.clone(),
                pool: pool_id.clone(),
            },
        });
    }

    // Redelivery: dispatch must preserve the established precedence,
    // and a duplicate of a completed delivery is idempotency's
    // concern. The proof records which requirement answers for it.
    let duplicates = match runtime.delivery {
        DeliverySemantics::AtMostOnce => DuplicateHandling::SingleDelivery,

        DeliverySemantics::AtLeastOnce | DeliverySemantics::Unspecified => {
            let coverage = idempotency
                .iter()
                .find(|check| {
                    &check.operation == operation_id
                        && !check.key.components.is_empty()
                        && check.key.components.iter().all(|component| {
                            component.source == ValueSource::Input(input_id.clone())
                        })
                })
                .map(|check| DuplicateCoverage {
                    requirement: check.requirement,
                    proven: matches!(check.verdict, IdempotencyVerdict::Proven { .. }),
                });

            DuplicateHandling::OrderPreservingRedelivery {
                idempotency: coverage,
            }
        }
    };

    match (precedence, assignment) {
        (Some(precedence), Some((routing_key, member_assignment)))
            if serial && obstacles.is_empty() =>
        {
            OrderingVerdict::proven(OrderingProof::RoutedOrder {
                input: input_id.clone(),
                topic: topic_id,
                pool: pool_id,
                precedence,
                routing_key,
                member_assignment,
                duplicates,
            })
        }

        _ => OrderingVerdict::Unproven { obstacles },
    }
}

impl OrderingCheck {
    /// The diagnostic for an unproven requirement; a proven one
    /// produces none.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let OrderingVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let requirement = self.requirement;
        let key = describe_value_ref(&self.key);

        Some(Diagnostic {
            code: DiagnosticCode::Verification(VerificationCode::OrderingUnproven),
            severity: Severity::Unknown,
            subject: Some(operation.clone()),
            message: format!(
                "Ordering requirement {requirement} of `{operation}` is not \
                 established: no declared facts prove that invocations sharing \
                 {key} take effect in their logical precedence."
            ),
            evidence: obstacles
                .iter()
                .map(|obstacle| obstacle.evidence(self))
                .collect(),
        })
    }
}

impl OrderingObstacle {
    fn evidence(&self, check: &OrderingCheck) -> Evidence {
        match self {
            Self::KeyNotFromInput { source } => Evidence {
                subject: Some(check.operation.clone()),
                message: format!(
                    "The ordering key is sourced from {}, not from an input of the \
                     operation, so no routing fact selects which invocations \
                     share it.",
                    super::describe::describe_value_source(source)
                ),
            },

            Self::RequestInputHasNoPrecedenceSource { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "Key-bearing invocations arrive through request input \
                     `{input}`; the DSL declares no precedence among requests, \
                     so there is no logical order to preserve. Request routing \
                     and serial member concurrency can establish \
                     serialization, but they do not invent an order."
                ),
            },

            Self::TopicOrderingProvidesNoPrecedence {
                input,
                topic,
                declared,
            } => Evidence {
                subject: Some(topic.clone()),
                message: match declared {
                    TopicOrdering::Unordered => format!(
                        "The runtime for `{topic}`, subscribed by `{input}`, is \
                         explicitly `unordered`: it provides no message order to \
                         serve as the precedence."
                    ),

                    TopicOrdering::Unspecified => format!(
                        "`{topic}`, subscribed by `{input}`, declares no runtime \
                         ordering fact to serve as the precedence."
                    ),

                    _ => format!(
                        "The runtime ordering of `{topic}`, subscribed by \
                         `{input}`, does not establish a precedence for this key."
                    ),
                },
            },

            Self::TopicKeyMappingMissing {
                input,
                topic,
                schema,
            } => Evidence {
                subject: Some(topic.clone()),
                message: format!(
                    "`{topic}` orders messages by key but declares no key \
                     mapping for `{schema}`, which `{input}` admits."
                ),
            },

            Self::KeyIdentityUnestablished {
                input,
                topic,
                schema,
                topic_key,
            } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "`{topic}` orders `{schema}` by `{topic_key}`, which is not \
                     established to carry the ordering key of `{input}`; the \
                     topic's order says nothing about same-key invocations."
                ),
            },

            Self::NoSubscriptionRuntime { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "Subscription input `{input}` declares no runtime, so nothing \
                     says where its deliveries execute or that the transport's \
                     order survives into execution."
                ),
            },

            Self::RoutingAbsent { input, pool } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` dispatches to `{pool}` without a routing \
                     declaration: same-key deliveries may be owned by different \
                     members and processed out of order."
                ),
            },

            Self::PoolUndeclared { input, pool } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` dispatches to execution pool `{pool}`, which the \
                     runtime model does not declare, so its member concurrency \
                     is unknown."
                ),
            },

            Self::MemberConcurrencyNotSerial { pool, declared, .. } => Evidence {
                subject: Some(pool.clone()),
                message: match declared {
                    MemberConcurrency::Bounded(bound) => format!(
                        "Execution pool `{pool}` admits {bound} simultaneous \
                         invocations per member: a later invocation may overtake \
                         an earlier one."
                    ),

                    MemberConcurrency::Unbounded => format!(
                        "Execution pool `{pool}` declares `unbounded` member \
                         concurrency: a later invocation may overtake an earlier \
                         one."
                    ),

                    MemberConcurrency::Unspecified => format!(
                        "Execution pool `{pool}` declares no member-concurrency \
                         fact; overtaking on one member cannot be excluded."
                    ),
                },
            },
        }
    }
}
