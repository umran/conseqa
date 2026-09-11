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
//! The load-bearing step is that the precedence and the routing domain
//! are established to be *the same domain*. A precedence over one
//! domain composed with routing over another proves nothing: the
//! transport would order two messages that routing then sends to
//! different members. `check_requirement` therefore decides the
//! pairing explicitly and exhaustively over both facts, rather than
//! reading the presence of each as though it implied the other.
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
    BatchOrderingPreservation, DeliverySemantics, FieldPath, Id, Input, MemberAssignment,
    MemberConcurrency, Model, Operation, OrderingRequirement, OrderingSemantics, OutboxInput,
    OutboxOrdering, SubscriptionRoutingKey, ValueRef, ValueSource,
};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};

use super::{ProofScope, RemedyLayer};
use super::describe::describe_value_ref;
use super::idempotency::{IdempotencyCheck, IdempotencyVerdict};
use super::serialization::{
    GroupingScope, MessageKeyFact, OutboxPartitionKeyFact, SerializationObstacle,
    admits_no_messages, admits_no_outbox_messages, assignment_owns_one_member, grouping_facts,
    outbox_partition_facts, pool_is_serial,
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

    /// The transport's declared precedence, preserved by one owning
    /// member at concurrency one; redelivery cannot reorder it.
    RoutedOrder {
        input: Id,
        topic: Id,
        pool: Id,

        /// The transport precedence, and which scope declared it.
        precedence: PrecedenceSource,
        scope: GroupingScope,

        /// Why same-key deliveries share one runtime group — the leg
        /// serialization proves on its own, and which ordering needs
        /// before a precedence is worth anything.
        message_keys: Vec<MessageKeyFact>,

        routing_key: SubscriptionRoutingKey,
        member_assignment: MemberAssignment,
        duplicates: DuplicateHandling,
    },

    /// The outbox runtime's declared precedence, preserved by one
    /// owning member at concurrency one, with any declared batching
    /// stage explicitly order-preserving.
    OutboxRoutedOrder {
        input: Id,
        outbox: Id,
        pool: Id,

        /// The outbox runtime precedence consumed.
        precedence: OutboxPrecedence,

        /// Why same-key deliveries share one outbox partition.
        partition_keys: Vec<OutboxPartitionKeyFact>,

        member_assignment: MemberAssignment,

        /// The declared batch ordering-preservation fact, when a
        /// batching stage exists. `None` records that no batching
        /// stage is declared, so no batch obstacle exists to clear.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batching: Option<BatchOrderingPreservation>,

        duplicates: DuplicateHandling,
    },
}

impl OrderingProof {
    pub fn scope(&self) -> ProofScope {
        match self {
            Self::NoAdmittedInvocations { .. } => ProofScope::L0Only,
            Self::RoutedOrder { .. } | Self::OutboxRoutedOrder { .. } => {
                ProofScope::RuntimeDependent
            }
        }
    }
}

/// Where the preserved precedence comes from.
///
/// Both variants require the same grouping identity to be useful — the
/// mechanism has to keep same-key deliveries together either way — so
/// the grouping evidence lives on the proof rather than inside one
/// variant. What differs is only the reach of the precedence itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrecedenceSource {
    /// The transport orders messages within a runtime group, and the
    /// ordering key is established to be that group's key.
    WithinGroup,

    /// The transport orders every message in scope, so same-key
    /// messages are ordered whatever the key. Still needs the
    /// mechanism to keep them on one member.
    Global,
}

/// Where an outbox-preserved precedence comes from — deliberately the
/// outbox's own vocabulary, not the subscription's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxPrecedence {
    /// The runtime orders messages within each keyed partition, and
    /// the ordering key is established to be the partition key.
    Partition,

    /// The runtime orders every message it consumes, so same-key
    /// messages are ordered whatever the key. Still needs the keyed
    /// partition mechanism to keep them on one member.
    Global,
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

    /// Neither scope declares a transport precedence, so there is no
    /// order for the execution topology to preserve.
    NoTransportPrecedence { input: Id, topic: Id },

    /// No keyed grouping is declared at either scope, so same-key
    /// deliveries are not established to share a runtime group — and
    /// a precedence that execution cannot keep together is no proof.
    NoGroupingDomain { input: Id, topic: Id },

    /// Both the topic and the subscription declare transport
    /// semantics, so neither can be read as the effective one.
    TransportSemanticsAtBothScopes { input: Id, topic: Id },

    /// A declared grouping key maps a schema to an empty tuple, which
    /// names no group.
    EmptyGroupingKey { input: Id, topic: Id, schema: Id },

    /// The declared member assignment does not give a routing domain
    /// one active owning member, so the precedence cannot survive into
    /// execution.
    MemberAssignmentNotExclusive {
        input: Id,
        declared: MemberAssignment,
    },

    /// The grouping declares no key mapping for an admitted schema.
    GroupingKeyMappingMissing { input: Id, topic: Id, schema: Id },

    /// The grouping key is not established to carry the ordering key
    /// for this schema, so same-key deliveries may land in different
    /// runtime groups and the declared precedence says nothing about
    /// them.
    KeyIdentityUnestablished {
        input: Id,
        topic: Id,
        schema: Id,
        grouping_key: FieldPath,
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

    /// The outbox input declares no runtime, so nothing says what
    /// precedence exists or where invocations execute.
    NoOutboxRuntime { input: Id },

    /// The outbox runtime declares `ordering: none`: no usable
    /// precedence exists for the mechanism to preserve.
    NoOutboxPrecedence { input: Id, outbox: Id },

    /// No keyed partitioning is declared, so same-key deliveries are
    /// not established to share a keyed partition that a precedence
    /// could reach execution through.
    NoPartitionDomain { input: Id, outbox: Id },

    /// The keyed partitioning declares no mapping for an admitted
    /// schema.
    OutboxPartitionMappingMissing { input: Id, outbox: Id, schema: Id },

    /// The partition mapping maps a schema to an empty tuple.
    EmptyOutboxPartitionKey { input: Id, outbox: Id, schema: Id },

    /// The partition key is not established to carry the ordering key
    /// for this schema.
    OutboxPartitionKeyNotEquivalent {
        input: Id,
        outbox: Id,
        schema: Id,
        partition_key: FieldPath,
    },

    /// The dispatch declares a batching stage whose ordering
    /// preservation is unspecified, so no evidence says the
    /// established order survives batch processing.
    BatchOrderingUnspecified { input: Id, pool: Id },
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

        Some(Input::Outbox(outbox_input)) => {
            return outbox_order_route(
                model,
                operation_id,
                input_id,
                outbox_input,
                requirement,
                idempotency,
            );
        }

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

    // The precedence: what the transport guarantees about the order in
    // which these messages are observed, read from whichever scope
    // declares it.
    let precedence = match model.effective_ordering(operation_id, input_id, &topic_id) {
        OrderingSemantics::WithinGroup => Some(PrecedenceSource::WithinGroup),
        OrderingSemantics::Global => Some(PrecedenceSource::Global),

        OrderingSemantics::None => {
            obstacles.push(OrderingObstacle::NoTransportPrecedence {
                input: input_id.clone(),
                topic: topic_id.clone(),
            });

            None
        }
    };

    // The grouping: why same-key deliveries stay together. Both
    // precedence sources need it — `within_group` because its
    // guarantee is *about* the group, `global` because a precedence
    // over everything still has to survive into execution, and it only
    // does when same-key deliveries reach one member. So the grouping
    // identity is established once, for either.
    let grouping = match grouping_facts(
        model,
        operation_id,
        input_id,
        subscription,
        &requirement.key.path,
    ) {
        Ok(facts) => Some(facts),

        Err(serialization_obstacles) => {
            for obstacle in serialization_obstacles {
                obstacles.push(match obstacle {
                    SerializationObstacle::NoGroupingDomain { input, topic } => {
                        OrderingObstacle::NoGroupingDomain { input, topic }
                    }

                    SerializationObstacle::GroupingKeyMappingMissing {
                        input,
                        topic,
                        schema,
                    } => OrderingObstacle::GroupingKeyMappingMissing {
                        input,
                        topic,
                        schema,
                    },

                    SerializationObstacle::TransportSemanticsAtBothScopes { input, topic } => {
                        OrderingObstacle::TransportSemanticsAtBothScopes { input, topic }
                    }

                    SerializationObstacle::EmptyGroupingKey {
                        input,
                        topic,
                        schema,
                    } => OrderingObstacle::EmptyGroupingKey {
                        input,
                        topic,
                        schema,
                    },

                    SerializationObstacle::KeyIdentityUnestablished {
                        input,
                        topic,
                        schema,
                        grouping_key,
                    } => OrderingObstacle::KeyIdentityUnestablished {
                        input,
                        topic,
                        schema,
                        grouping_key,
                    },

                    _ => OrderingObstacle::NoGroupingDomain {
                        input: input_id.clone(),
                        topic: topic_id.clone(),
                    },
                });
            }

            None
        }
    };

    // The mechanism: the routing domain the grouping establishes, one
    // owning member, one invocation at a time on that member.
    let Some(runtime) = model.subscription_runtime(operation_id, input_id) else {
        obstacles.push(OrderingObstacle::NoSubscriptionRuntime {
            input: input_id.clone(),
        });

        return OrderingVerdict::Unproven { obstacles };
    };

    let pool_id = runtime.dispatch.pool.clone();

    // Dispatch does not create precedence; it preserves the precedence
    // the transport established, by admitting one runtime group to one
    // member. `grouping_key` routing is what ties the two together —
    // it names the very domain the grouping evidence is about, so the
    // precedence and the routing domain cannot drift apart.
    let assignment = match &runtime.dispatch.routing {
        None => {
            obstacles.push(OrderingObstacle::RoutingAbsent {
                input: input_id.clone(),
                pool: pool_id.clone(),
            });

            None
        }

        Some(routing) => match routing.key {
            SubscriptionRoutingKey::GroupingKey => Some((routing.key, routing.member_assignment)),
        },
    };

    // The ownership leg, interrogated rather than copied — same rule as
    // the serialization side.
    if let Some((_, declared)) = &assignment
        && !assignment_owns_one_member(*declared)
    {
        obstacles.push(OrderingObstacle::MemberAssignmentNotExclusive {
            input: input_id.clone(),
            declared: *declared,
        });
    }

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
    let duplicates = duplicate_handling(operation_id, input_id, runtime.delivery, idempotency);

    match (precedence, grouping, assignment) {
        (Some(precedence), Some(grouping), Some((routing_key, member_assignment)))
            if serial && obstacles.is_empty() =>
        {
            OrderingVerdict::proven(OrderingProof::RoutedOrder {
                input: input_id.clone(),
                topic: grouping.topic,
                pool: pool_id,
                precedence,
                scope: grouping.scope,
                message_keys: grouping.message_keys,
                routing_key,
                member_assignment,
                duplicates,
            })
        }

        _ => OrderingVerdict::Unproven { obstacles },
    }
}

/// Who answers for the work a duplicate delivery does, from the
/// delivery fact and the idempotency verdicts — shared by the
/// subscription and outbox routes.
fn duplicate_handling(
    operation_id: &Id,
    input_id: &Id,
    delivery: DeliverySemantics,
    idempotency: &[IdempotencyCheck],
) -> DuplicateHandling {
    match delivery {
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
    }
}

/// The outbox route: the runtime's declared precedence, the keyed
/// partition domain identifying the ordering key, dispatch routing by
/// that domain with one owning member at concurrency one, and — where
/// a batching stage exists — its declared order preservation (§60–§64
/// of the outbox revision).
///
/// A batching stage differs from the serialization side: `preserved`
/// lets the established precedence pass through the stage, while
/// `unspecified` stops the proof — the stage then provides no evidence
/// that established order survives execution. Overlap within the
/// batch is not this proof's concern; ordering constrains precedence
/// of effect, not simultaneity.
fn outbox_order_route(
    model: &Model,
    operation_id: &Id,
    input_id: &Id,
    input: &OutboxInput,
    requirement: &OrderingRequirement,
    idempotency: &[IdempotencyCheck],
) -> OrderingVerdict {
    if admits_no_outbox_messages(model, input) {
        return OrderingVerdict::proven(OrderingProof::NoAdmittedInvocations {
            input: input_id.clone(),
        });
    }

    let Some(runtime) = model.outbox_runtime(operation_id, input_id) else {
        return OrderingVerdict::Unproven {
            obstacles: vec![OrderingObstacle::NoOutboxRuntime {
                input: input_id.clone(),
            }],
        };
    };

    let mut obstacles = Vec::new();

    // The precedence the runtime establishes among consumed messages.
    let precedence = match runtime.ordering {
        OutboxOrdering::Partition => Some(OutboxPrecedence::Partition),
        OutboxOrdering::Global => Some(OutboxPrecedence::Global),

        OutboxOrdering::None => {
            obstacles.push(OrderingObstacle::NoOutboxPrecedence {
                input: input_id.clone(),
                outbox: input.outbox.clone(),
            });

            None
        }
    };

    // The partition domain: why same-key deliveries stay together.
    // Both precedence sources need it — `partition` because its
    // guarantee is *about* the partition, `global` because a
    // precedence over everything still has to survive into execution,
    // and it only does when same-key deliveries reach one member.
    let partitioned = match outbox_partition_facts(
        model,
        input_id,
        input,
        runtime,
        &requirement.key.path,
    ) {
        Ok(facts) => Some(facts),

        Err(partition_obstacles) => {
            for obstacle in partition_obstacles {
                obstacles.push(match obstacle {
                    SerializationObstacle::NoPartitionDomain { input, outbox } => {
                        OrderingObstacle::NoPartitionDomain { input, outbox }
                    }

                    SerializationObstacle::OutboxPartitionMappingMissing {
                        input,
                        outbox,
                        schema,
                    } => OrderingObstacle::OutboxPartitionMappingMissing {
                        input,
                        outbox,
                        schema,
                    },

                    SerializationObstacle::EmptyOutboxPartitionKey {
                        input,
                        outbox,
                        schema,
                    } => OrderingObstacle::EmptyOutboxPartitionKey {
                        input,
                        outbox,
                        schema,
                    },

                    SerializationObstacle::OutboxPartitionKeyNotEquivalent {
                        input,
                        outbox,
                        schema,
                        partition_key,
                    } => OrderingObstacle::OutboxPartitionKeyNotEquivalent {
                        input,
                        outbox,
                        schema,
                        partition_key,
                    },

                    _ => OrderingObstacle::NoPartitionDomain {
                        input: input_id.clone(),
                        outbox: input.outbox.clone(),
                    },
                });
            }

            None
        }
    };

    let pool_id = runtime.dispatch.pool.clone();

    // The ownership leg: the dispatch must route by the partition
    // domain, and the assignment must give each partition one active
    // owning member.
    let assignment = match &runtime.dispatch.routing {
        None => {
            obstacles.push(OrderingObstacle::RoutingAbsent {
                input: input_id.clone(),
                pool: pool_id.clone(),
            });

            None
        }

        Some(routing) => match routing.key {
            crate::spec::OutboxRoutingKey::PartitionKey => {
                if !assignment_owns_one_member(routing.member_assignment) {
                    obstacles.push(OrderingObstacle::MemberAssignmentNotExclusive {
                        input: input_id.clone(),
                        declared: routing.member_assignment,
                    });
                }

                Some(routing.member_assignment)
            }
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

    // The batching stage, judged explicitly: absent means no obstacle
    // exists; `preserved` lets the precedence pass; `unspecified`
    // provides no evidence that it survives.
    let batching = runtime.dispatch.batching.as_ref().map(|batching| batching.ordering);

    let batch_ok = match batching {
        None | Some(BatchOrderingPreservation::Preserved) => true,

        Some(BatchOrderingPreservation::Unspecified) => {
            obstacles.push(OrderingObstacle::BatchOrderingUnspecified {
                input: input_id.clone(),
                pool: pool_id.clone(),
            });

            false
        }
    };

    // Duplicate consumption attempts are intrinsic to the outbox
    // abstraction — a pending message is re-driven until successfully
    // consumed, and attempts may overlap — so redelivery is judged as
    // at-least-once; no delivery fact exists to bound it.
    let duplicates = duplicate_handling(
        operation_id,
        input_id,
        DeliverySemantics::AtLeastOnce,
        idempotency,
    );

    match (precedence, partitioned, assignment) {
        (Some(precedence), Some(partition_keys), Some(member_assignment))
            if serial && batch_ok && obstacles.is_empty() =>
        {
            OrderingVerdict::proven(OrderingProof::OutboxRoutedOrder {
                input: input_id.clone(),
                outbox: input.outbox.clone(),
                pool: pool_id,
                precedence,
                partition_keys,
                member_assignment,
                batching,
                duplicates,
            })
        }

        _ => OrderingVerdict::Unproven { obstacles },
    }
}

impl OrderingCheck {
    /// Which layer holds the facts this check is waiting on, or
    /// `None` when it is proven.
    pub fn remedy(&self) -> Option<RemedyLayer> {
        let OrderingVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        RemedyLayer::joined(obstacles.iter().map(OrderingObstacle::layer))
    }

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
    /// The semantic layer this obstacle's fix belongs to.
    ///
    /// Two obstacles are application-layer. A key no input carries
    /// selects no population, and a request boundary has no precedence
    /// source at all — no runtime declaration invents one, so the fix
    /// is the operation's interface, not its topology.
    pub fn layer(&self) -> RemedyLayer {
        match self {
            Self::KeyNotFromInput { .. } | Self::RequestInputHasNoPrecedenceSource { .. } => {
                RemedyLayer::Application
            }

            Self::NoTransportPrecedence { .. }
            | Self::NoGroupingDomain { .. }
            | Self::TransportSemanticsAtBothScopes { .. }
            | Self::EmptyGroupingKey { .. }
            | Self::MemberAssignmentNotExclusive { .. }
            | Self::GroupingKeyMappingMissing { .. }
            | Self::KeyIdentityUnestablished { .. }
            | Self::NoSubscriptionRuntime { .. }
            | Self::RoutingAbsent { .. }
            | Self::PoolUndeclared { .. }
            | Self::MemberConcurrencyNotSerial { .. }
            | Self::NoOutboxRuntime { .. }
            | Self::NoOutboxPrecedence { .. }
            | Self::NoPartitionDomain { .. }
            | Self::OutboxPartitionMappingMissing { .. }
            | Self::EmptyOutboxPartitionKey { .. }
            | Self::OutboxPartitionKeyNotEquivalent { .. }
            | Self::BatchOrderingUnspecified { .. } => RemedyLayer::Runtime,
        }
    }

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

            Self::NoTransportPrecedence { input, topic } => Evidence {
                subject: Some(topic.clone()),
                message: format!(
                    "Neither `{topic}` nor `{input}` declares a transport precedence, \
                     so there is no order for the execution topology to preserve."
                ),
            },

            Self::TransportSemanticsAtBothScopes { input, topic } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{topic}` declares transport semantics for all its \
                     subscriptions and `{input}` declares its own. The two scopes \
                     are exclusive, so neither can be read as the effective one."
                ),
            },

            Self::EmptyGroupingKey { topic, schema, .. } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "The grouping in effect for `{topic}` maps `{schema}` to an \
                     empty tuple, which names no group."
                ),
            },

            Self::MemberAssignmentNotExclusive { input, declared } => Evidence {
                subject: Some(input.clone()),
                message: match declared {
                    MemberAssignment::RoundRobin => format!(
                        "`{input}` declares `round_robin` member assignment, which \
                         rotates through members irrespective of routing domain, so \
                         the transport's order does not survive into execution: a \
                         later delivery may run on another member and overtake an \
                         earlier one."
                    ),

                    _ => format!(
                        "The member assignment declared for `{input}` does not give \
                         a routing domain one active owning member, so a later \
                         invocation may execute on a different member and overtake \
                         an earlier one."
                    ),
                },
            },

            Self::NoGroupingDomain { input, topic } => Evidence {
                subject: Some(topic.clone()),
                message: format!(
                    "No keyed grouping is in effect for `{input}` on `{topic}`. A \
                     transport precedence only reaches execution when same-key \
                     deliveries stay in one group, and nothing establishes that here."
                ),
            },

            Self::GroupingKeyMappingMissing {
                input,
                topic,
                schema,
            } => Evidence {
                subject: Some(topic.clone()),
                message: format!(
                    "The grouping in effect for `{input}` declares no key mapping for \
                     `{schema}`, which `{topic}` carries and the subscription admits."
                ),
            },

            Self::KeyIdentityUnestablished {
                input,
                topic,
                schema,
                grouping_key,
            } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "For messages of `{schema}` on `{topic}`, the grouping key \
                     `{grouping_key}` is not established to carry the ordering key of \
                     `{input}`, so same-key deliveries may land in different runtime \
                     groups."
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

            Self::NoOutboxRuntime { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "Outbox input `{input}` declares no runtime, so nothing says \
                     what precedence its deliveries carry or that any order \
                     survives into execution."
                ),
            },

            Self::NoOutboxPrecedence { input, outbox } => Evidence {
                subject: Some(outbox.clone()),
                message: format!(
                    "The outbox runtime of `{input}` on `{outbox}` declares \
                     `ordering: none`, so there is no order for the execution \
                     topology to preserve."
                ),
            },

            Self::NoPartitionDomain { input, outbox } => Evidence {
                subject: Some(outbox.clone()),
                message: format!(
                    "The outbox runtime of `{input}` on `{outbox}` declares no \
                     keyed partitioning. A precedence only reaches execution when \
                     same-key deliveries stay in one partition, and nothing \
                     establishes that here."
                ),
            },

            Self::OutboxPartitionMappingMissing {
                input,
                outbox,
                schema,
            } => Evidence {
                subject: Some(outbox.clone()),
                message: format!(
                    "The partitioning in effect for `{input}` declares no partition \
                     key for `{schema}`, which the input admits from `{outbox}`."
                ),
            },

            Self::EmptyOutboxPartitionKey { outbox, schema, .. } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "The partitioning declared for `{outbox}` maps `{schema}` to an \
                     empty tuple, which names no partition."
                ),
            },

            Self::OutboxPartitionKeyNotEquivalent {
                input,
                outbox,
                schema,
                partition_key,
            } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "For messages of `{schema}` on `{outbox}`, the partition key \
                     `{partition_key}` is not established to carry the ordering key \
                     of `{input}`, so same-key deliveries may land in different \
                     partitions."
                ),
            },

            Self::BatchOrderingUnspecified { input, pool } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` dispatches to `{pool}` through a declared batching \
                     stage whose ordering preservation is unspecified: no evidence \
                     says the established order survives batch processing."
                ),
            },
        }
    }
}
