//! Verification of operation serialization requirements (§9).
//!
//! A serialization requirement keyed by a `ValueRef` means:
//!
//! > Invocations with the same logical key must not execute
//! > concurrently.
//!
//! Serialization establishes mutual exclusion of same-key invocations.
//! It does not establish which same-key invocation should come first;
//! that is an ordering requirement and is deliberately out of scope
//! here (§9 "Serialization versus ordering").
//!
//! ## The constrained population
//!
//! A concrete invocation is associated with the input that triggered
//! it, and a `ValueRef` sourcing an input refers to the payload of
//! that triggering input (§7). An invocation triggered by a different
//! input has no value for the key, so it is not "an invocation with
//! the same logical key" as any other and the requirement does not
//! constrain it. For a key sourced from input `i`, the population is
//! therefore the invocations triggered by `i`. A key sourced from
//! anything other than an input selects no population any runtime
//! fact can address, and nothing can serialize it.
//!
//! ## Accepted proof routes
//!
//! 1. **Vacuous population**: the key's subscription input admits no
//!    message schemas, so the population is empty by declaration.
//!    This is the only L0-only route.
//! 2. **Request-routed**: an L1 `Router` serves the key's request
//!    boundary; its semantic routing key is equivalent to the
//!    serialization key, so same-key invocations share one routing
//!    domain; its `MemberAssignment` gives that domain one active
//!    owning member, including through handoff; and the target
//!    `ExecutionPool` declares `member_concurrency = bounded(1)`, so
//!    that member runs one invocation at a time.
//! 3. **Subscription-routed**: the same argument on the delivery side.
//!    The dispatch routes by `grouping_key`, a keyed grouping is in
//!    effect at one of the two declaration scopes, and the
//!    serialization key is established to carry the same logical value
//!    as the grouping key for every admitted message schema.
//!
//! Both runtime routes have the same shape, and it is the shape the
//! whole model is built around:
//!
//! ```text
//! semantic key equivalence
//!     -> routing-domain equivalence
//!     -> member ownership
//!     -> member concurrency
//! ```
//!
//! Every step is a distinct declared fact and none is substituted for
//! another. A proof taking either runtime route is recorded as
//! `RuntimeDependent`: it holds of the declared realization, and is
//! invalidated when that realization changes.
//!
//! ## Routes deliberately not credited
//!
//! - **Shared pool identity alone**. Two boundaries assigned to one
//!   pool share an execution population, not a routing domain. Even
//!   equal-looking keys on two routers say nothing about common member
//!   ownership.
//! - **Routing absence**. A router or dispatch without a routing block
//!   gives the target population and no member-affinity fact at all —
//!   not round-robin, not random, not one member.
//! - **A partial key match**. If the routing key is a tuple wider than
//!   the serialization key, two same-key invocations differing in the
//!   remaining components fall into different routing domains, so
//!   equality of the serialization key implies nothing.
//! - **Locks** (§21). A lock protects the object instances its
//!   selector selects. Whether two same-key invocations conflict on a
//!   common instance depends on such an instance existing at lock
//!   time, which is runtime state the model cannot declare, and a
//!   lock serializes only the span from acquisition to transaction
//!   end, not the invocation's whole execution.
//! - **Serializable isolation** (§17). An equivalent serial commit
//!   order does not prevent concurrent execution.
//! - **Transport ordering**. Not merely uncredited — never consulted.
//!   Serialization is about non-overlap, and a grouping domain is the
//!   whole of what a transport supplies for it. This is the reason
//!   grouping is declared independently of ordering: an unordered
//!   transport that still groups by key serializes.
//! - **`bounded(n)` with `n > 1`**: it permits overlap.
//!
//! A requirement no route establishes is `Unproven`, never violated:
//! concurrency declarations are upper bounds, and nothing in the
//! model can prove that an overlapping same-key pair actually occurs
//! (§1.2).

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{
    FieldPath, Id, Input, MemberAssignment, MemberConcurrency, MessageSelector, Model, Operation,
    SerializationRequirement, SubscriptionInput, SubscriptionRoutingKey, ValueRef, ValueSource,
};

use super::ProofScope;
use super::describe::{describe_value_ref, describe_value_source, value_source_id};
use super::value_identity::canonical_value_path;

/// The verdict for one declared serialization requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerializationCheck {
    pub operation: Id,

    /// Index into `operation.requirements.serialization`.
    pub requirement: usize,

    /// The requirement's key, copied so the check is self-contained.
    pub key: ValueRef,

    pub verdict: SerializationVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializationVerdict {
    /// The requirement follows from the cited declared facts, subject
    /// to implementation conformance with those facts (§1.3), and —
    /// for a `RuntimeDependent` scope — to the realization those facts
    /// describe.
    Proven {
        proof: SerializationProof,
        scope: ProofScope,
    },

    /// The declared facts do not establish the requirement. This is
    /// epistemic: it records which facts are missing or insufficient,
    /// not that a violation occurs (§1.2).
    Unproven {
        obstacles: Vec<SerializationObstacle>,
    },
}

impl SerializationVerdict {
    fn proven(proof: SerializationProof) -> Self {
        Self::Proven {
            scope: proof.scope(),
            proof,
        }
    }
}

/// A successful serialization argument and the facts it consumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializationProof {
    /// The key's subscription input admits no message schemas, so no
    /// invocation can bear the key and the requirement constrains an
    /// empty population.
    NoAdmittedInvocations { input: Id },

    /// A router assigns same-key requests to one owning member of a
    /// pool whose members execute one invocation at a time.
    RequestRouted {
        input: Id,
        router: Id,
        pool: Id,

        /// The router's semantic routing key, and why each component
        /// carries the same logical value as the serialization key.
        routing_key: Vec<RoutingKeyFact>,

        member_assignment: MemberAssignment,
    },

    /// The delivery-side counterpart: deliveries sharing a runtime
    /// group share a routing domain, one member owns it, and that
    /// member executes one invocation at a time.
    ///
    /// No ordering fact participates. Serialization is about
    /// non-overlap, and a grouping domain is all the transport has to
    /// supply for it — which is why grouping is declared independently
    /// of ordering.
    SubscriptionRouted {
        input: Id,
        topic: Id,
        pool: Id,

        /// Which scope declared the grouping this proof consumed.
        grouping_scope: GroupingScope,

        /// Per admitted message schema, the fact identifying the
        /// grouping key with the serialization key.
        message_keys: Vec<MessageKeyFact>,

        member_assignment: MemberAssignment,
    },
}

impl SerializationProof {
    pub fn scope(&self) -> ProofScope {
        match self {
            Self::NoAdmittedInvocations { .. } => ProofScope::L0Only,

            Self::RequestRouted { .. } | Self::SubscriptionRouted { .. } => {
                ProofScope::RuntimeDependent
            }
        }
    }
}

/// Which declaration scope supplied the grouping a proof consumed.
///
/// Recorded because the two scopes are exclusive and a reader tracing
/// the proof needs to know which declaration to look at — and which one
/// changing would invalidate it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupingScope {
    /// The topic runtime declares transport semantics for every
    /// subscription of the topic.
    Topic { topic: Id },

    /// The subscription declares its own.
    Subscription { operation: Id, input: Id },
}

/// For one component of a request routing key, why it denotes the same
/// logical value as the serialization key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingKeyFact {
    pub path: FieldPath,
    pub identity: KeyIdentity,
}

/// For one admitted message schema, how the effective grouping key was
/// identified with the requirement key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageKeyFact {
    pub schema: Id,

    /// The declared grouping-key path for this schema.
    pub grouping_key: FieldPath,

    pub identity: KeyIdentity,
}

/// Why two field paths of one schema denote the same logical value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyIdentity {
    /// The paths are identical, so they name the same field.
    SamePath,

    /// The paths differ but expand to the same canonical value path
    /// through declared fragment mappings, which assert semantic
    /// identity across the fragment boundary (§4).
    SameCanonicalValue { schema: Id, path: FieldPath },
}

/// A fact that is missing or insufficient for one candidate proof
/// route.
///
/// Obstacles preserve the declared value where one exists, so an
/// explicitly negative declaration (`unbounded`) is distinguishable
/// from an absent one (`unspecified`, or an absent runtime block)
/// (§1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializationObstacle {
    /// The key is not sourced from an input declared by the
    /// operation, so no routing fact selects which invocations share
    /// it.
    KeyNotFromInput { source: ValueSource },

    /// Key-bearing invocations arrive through a request input that no
    /// router serves, so no runtime fact says where they execute.
    NoRouter { input: Id },

    /// More than one router serves the boundary. The declarations may
    /// contradict, so there is no single set of routing facts to reason
    /// from; validation rejects the shape and verification declines to
    /// pick one.
    AmbiguousRouter { input: Id, routers: Vec<Id> },

    /// Both the topic and the subscription declare transport
    /// semantics. The two scopes are exclusive, so the declarations may
    /// contradict and neither can be read as the effective one.
    TransportSemanticsAtBothScopes { input: Id, topic: Id },

    /// A declared routing key names no domain, because its tuple is
    /// empty.
    EmptyRoutingKey { input: Id, router: Id },

    /// A declared grouping key names no domain for a schema, because
    /// its tuple is empty.
    EmptyGroupingKey { input: Id, topic: Id, schema: Id },

    /// Key-bearing deliveries arrive through a subscription input with
    /// no declared runtime, so no runtime fact says where they
    /// execute.
    NoSubscriptionRuntime { input: Id },

    /// The boundary is assigned to a pool, but declares no routing:
    /// the target execution population is known and no member-affinity
    /// fact exists.
    RoutingAbsent { input: Id, pool: Id },

    /// A component of the routing key is not established to carry the
    /// same logical value as the serialization key, so same-key
    /// invocations may fall into different routing domains.
    RoutingKeyNotEquivalent {
        input: Id,
        router: Id,
        component: FieldPath,
    },

    /// The routing key is a wider tuple than the serialization key:
    /// same-key invocations differing in another component belong to
    /// different routing domains.
    RoutingKeyWiderThanRequirement {
        input: Id,
        router: Id,
        key: Vec<FieldPath>,
    },

    /// Dispatch routes by `grouping_key`, but no keyed grouping is
    /// declared at either scope, so there is no domain to route by.
    NoGroupingDomain { input: Id, topic: Id },

    /// The grouping declares no key mapping for an admitted schema.
    /// Validation rejects this shape; verification records it rather
    /// than assuming a mapping.
    GroupingKeyMappingMissing { input: Id, topic: Id, schema: Id },

    /// The grouping key for this schema is not established to carry
    /// the same logical value as the serialization key, so same-key
    /// deliveries may enter different runtime groups.
    KeyIdentityUnestablished {
        input: Id,
        topic: Id,
        schema: Id,
        grouping_key: FieldPath,
    },

    /// The target execution pool is not declared, so its member
    /// concurrency is unknown.
    PoolUndeclared { input: Id, pool: Id },

    /// The declared member assignment does not give a routing domain
    /// one active owning member.
    MemberAssignmentNotExclusive {
        input: Id,
        declared: MemberAssignment,
    },

    /// The pool's declared member concurrency does not bound one
    /// member to one simultaneously active invocation.
    MemberConcurrencyNotSerial {
        input: Id,
        pool: Id,
        declared: MemberConcurrency,
    },
}

/// Checks every serialization requirement declared by the model.
pub fn check(model: &Model) -> Vec<SerializationCheck> {
    let mut checks = Vec::new();

    for (operation_id, operation) in &model.operations {
        for (index, requirement) in operation.requirements.serialization.iter().enumerate() {
            checks.push(SerializationCheck {
                operation: operation_id.clone(),
                requirement: index,
                key: requirement.key.clone(),
                verdict: check_requirement(model, operation_id, operation, requirement),
            });
        }
    }

    checks
}

fn check_requirement(
    model: &Model,
    operation_id: &Id,
    operation: &Operation,
    requirement: &SerializationRequirement,
) -> SerializationVerdict {
    // Every route serializes the population of key-bearing
    // invocations, which the key's source input defines.
    let ValueSource::Input(input_id) = &requirement.key.source else {
        return SerializationVerdict::Unproven {
            obstacles: vec![SerializationObstacle::KeyNotFromInput {
                source: requirement.key.source.clone(),
            }],
        };
    };

    let Some(input) = operation.inputs.get(input_id) else {
        return SerializationVerdict::Unproven {
            obstacles: vec![SerializationObstacle::KeyNotFromInput {
                source: requirement.key.source.clone(),
            }],
        };
    };

    match input {
        Input::Request(request) => request_route(
            model,
            operation_id,
            input_id,
            &request.schema,
            &requirement.key.path,
        ),

        Input::Subscription(subscription) => subscription_route(
            model,
            operation_id,
            input_id,
            subscription,
            &requirement.key.path,
        ),
    }
}

/// The request-side route: router, routing-key equivalence, member
/// assignment, member concurrency.
fn request_route(
    model: &Model,
    operation_id: &Id,
    input_id: &Id,
    schema: &Id,
    key: &FieldPath,
) -> SerializationVerdict {
    let routers = model.routers_for(operation_id, input_id);

    let (router_id, router) = match routers.as_slice() {
        [single] => *single,

        [] => {
            return SerializationVerdict::Unproven {
                obstacles: vec![SerializationObstacle::NoRouter {
                    input: input_id.clone(),
                }],
            };
        }

        several => {
            return SerializationVerdict::Unproven {
                obstacles: vec![SerializationObstacle::AmbiguousRouter {
                    input: input_id.clone(),
                    routers: several.iter().map(|(id, _)| (*id).clone()).collect(),
                }],
            };
        }
    };

    let mut obstacles = Vec::new();

    let routing_key = match &router.routing {
        None => {
            obstacles.push(SerializationObstacle::RoutingAbsent {
                input: input_id.clone(),
                pool: router.pool.clone(),
            });

            None
        }

        Some(routing) => {
            match routing_key_facts(model, input_id, router_id, schema, &routing.key, key) {
                Ok(facts) => Some((facts, routing.member_assignment)),

                Err(routing_obstacles) => {
                    obstacles.extend(routing_obstacles);

                    None
                }
            }
        }
    };

    let serial = pool_is_serial(model, input_id, &router.pool, &mut obstacles);

    let exclusive = routing_key
        .as_ref()
        .is_none_or(|(_, assignment)| assignment_owns_one_member(*assignment));

    if !exclusive && let Some((_, declared)) = &routing_key {
        obstacles.push(SerializationObstacle::MemberAssignmentNotExclusive {
            input: input_id.clone(),
            declared: *declared,
        });
    }

    match routing_key {
        Some((routing_key, member_assignment)) if serial && exclusive => {
            SerializationVerdict::proven(SerializationProof::RequestRouted {
                input: input_id.clone(),
                router: router_id.clone(),
                pool: router.pool.clone(),
                routing_key,
                member_assignment,
            })
        }

        _ => SerializationVerdict::Unproven { obstacles },
    }
}

/// The delivery-side route: dispatch, grouping-key equivalence, member
/// assignment, member concurrency.
fn subscription_route(
    model: &Model,
    operation_id: &Id,
    input_id: &Id,
    subscription: &SubscriptionInput,
    key: &FieldPath,
) -> SerializationVerdict {
    // A population empty by declaration is serialized vacuously, and
    // needs no runtime fact at all.
    if admits_no_messages(model, subscription) {
        return SerializationVerdict::proven(SerializationProof::NoAdmittedInvocations {
            input: input_id.clone(),
        });
    }

    let Some(runtime) = model.subscription_runtime(operation_id, input_id) else {
        return SerializationVerdict::Unproven {
            obstacles: vec![SerializationObstacle::NoSubscriptionRuntime {
                input: input_id.clone(),
            }],
        };
    };

    let mut obstacles = Vec::new();

    let routed = match &runtime.dispatch.routing {
        None => {
            obstacles.push(SerializationObstacle::RoutingAbsent {
                input: input_id.clone(),
                pool: runtime.dispatch.pool.clone(),
            });

            None
        }

        Some(routing) => match routing.key {
            SubscriptionRoutingKey::GroupingKey => {
                match grouping_facts(model, operation_id, input_id, subscription, key) {
                    Ok(facts) => Some((facts, routing.member_assignment)),

                    Err(routing_obstacles) => {
                        obstacles.extend(routing_obstacles);

                        None
                    }
                }
            }
        },
    };

    let serial = pool_is_serial(model, input_id, &runtime.dispatch.pool, &mut obstacles);

    let exclusive = routed
        .as_ref()
        .is_none_or(|(_, assignment)| assignment_owns_one_member(*assignment));

    if !exclusive && let Some((_, declared)) = &routed {
        obstacles.push(SerializationObstacle::MemberAssignmentNotExclusive {
            input: input_id.clone(),
            declared: *declared,
        });
    }

    match routed {
        Some((facts, member_assignment)) if serial && exclusive => {
            SerializationVerdict::proven(SerializationProof::SubscriptionRouted {
                input: input_id.clone(),
                topic: facts.topic,
                pool: runtime.dispatch.pool.clone(),
                grouping_scope: facts.scope,
                message_keys: facts.message_keys,
                member_assignment,
            })
        }

        _ => SerializationVerdict::Unproven { obstacles },
    }
}

/// Whether a member assignment gives one routing domain one active
/// owning member, which is the ownership leg of every routed proof.
///
/// Matched exhaustively on purpose. The other three legs of the chain
/// are each guarded — routing keys by an exhaustive match, member
/// concurrency by `is_serial` being false for anything new — and this
/// one must be too, so a future assignment with weaker ownership
/// cannot be copied into a proof as though it were `consistent_hash`.
pub(super) fn assignment_owns_one_member(assignment: MemberAssignment) -> bool {
    match assignment {
        MemberAssignment::ConsistentHash => true,

        // Rotation ignores the routing domain, so two invocations of
        // one domain land on different members and may run at once,
        // whatever the pool's member concurrency.
        MemberAssignment::RoundRobin => false,
    }
}

/// Whether the pool bounds one member to one active invocation,
/// recording the obstacle when it does not.
pub(super) fn pool_is_serial(
    model: &Model,
    input_id: &Id,
    pool_id: &Id,
    obstacles: &mut Vec<SerializationObstacle>,
) -> bool {
    let Some(pool) = model.execution_pool(pool_id) else {
        obstacles.push(SerializationObstacle::PoolUndeclared {
            input: input_id.clone(),
            pool: pool_id.clone(),
        });

        return false;
    };

    if pool.member_concurrency.is_serial() {
        return true;
    }

    obstacles.push(SerializationObstacle::MemberConcurrencyNotSerial {
        input: input_id.clone(),
        pool: pool_id.clone(),
        declared: pool.member_concurrency,
    });

    false
}

/// Whether the subscription's admitted message set is empty by
/// declaration.
///
/// `only []` admits nothing regardless of the topic. `all` admits the
/// topic's declared messages, so it is empty only when the topic is
/// resolvable and declares none; an unresolvable topic leaves the
/// admitted set unknown, which must not become a vacuous proof.
pub(super) fn admits_no_messages(model: &Model, subscription: &SubscriptionInput) -> bool {
    match &subscription.messages {
        MessageSelector::Only(messages) => messages.is_empty(),

        MessageSelector::All => model
            .topics
            .get(&subscription.topic)
            .is_some_and(|topic| topic.messages.is_empty()),
    }
}

/// Establishes routing-domain equivalence for a request routing key:
/// every component must carry the same logical value as the
/// requirement key.
///
/// Requiring *every* component is what makes the argument sound. A
/// routing key of `(account_id, region)` partitions same-`account_id`
/// invocations across routing domains by `region`, so equality of the
/// serialization key would no longer imply a common domain. A
/// degenerate repetition — `(account_id, account_id)` — is admitted
/// because it partitions nothing.
fn routing_key_facts(
    model: &Model,
    input_id: &Id,
    router_id: &Id,
    schema: &Id,
    routing_key: &[FieldPath],
    requirement_key: &FieldPath,
) -> Result<Vec<RoutingKeyFact>, Vec<SerializationObstacle>> {
    // An empty tuple names no domain. Validation rejects the shape, and
    // verification must not turn it into a vacuous proof: with no
    // component to constrain them, all invocations would appear to
    // share a domain.
    if routing_key.is_empty() {
        return Err(vec![SerializationObstacle::EmptyRoutingKey {
            input: input_id.clone(),
            router: router_id.clone(),
        }]);
    }

    let mut facts = Vec::new();
    let mut obstacles = Vec::new();

    for component in routing_key {
        match key_identity(model, schema, component, requirement_key) {
            Some(identity) => facts.push(RoutingKeyFact {
                path: component.clone(),
                identity,
            }),

            None => obstacles.push(SerializationObstacle::RoutingKeyNotEquivalent {
                input: input_id.clone(),
                router: router_id.clone(),
                component: component.clone(),
            }),
        }
    }

    if obstacles.is_empty() {
        return Ok(facts);
    }

    // A wider tuple is the common shape of this failure and deserves
    // its own explanation, alongside the components themselves.
    if facts.len() < routing_key.len() && !facts.is_empty() {
        obstacles.push(SerializationObstacle::RoutingKeyWiderThanRequirement {
            input: input_id.clone(),
            router: router_id.clone(),
            key: routing_key.to_vec(),
        });
    }

    Err(obstacles)
}

/// The grouping facts a `grouping_key` routing declaration rests on.
pub(super) struct GroupingFacts {
    pub topic: Id,
    pub scope: GroupingScope,
    pub message_keys: Vec<MessageKeyFact>,
}

/// Establishes routing-domain equivalence for `key: grouping_key`:
/// for every admitted message schema, the effective grouping key must
/// carry the same logical value as the requirement key.
///
/// The grouping is read from whichever scope declares it (§12), and the
/// scope is recorded on the result so the proof can cite the
/// declaration it actually consumed. No ordering fact is touched: a
/// grouping domain is the whole of what the transport contributes here.
pub(super) fn grouping_facts(
    model: &Model,
    operation_id: &Id,
    input_id: &Id,
    subscription: &SubscriptionInput,
    requirement_key: &FieldPath,
) -> Result<GroupingFacts, Vec<SerializationObstacle>> {
    let topic_id = subscription.topic.clone();

    // The two declaration scopes are exclusive. When both declare, the
    // facts may contradict — one grouping by order_id and the other by
    // event_id put the same delivery in different groups — so there is
    // no effective grouping to read. Validation rejects the shape;
    // verification refuses it too rather than proving from whichever
    // half `effective_grouping` happens to return.
    let both_scopes = model.topic_scoped_transport(&topic_id)
        && model
            .subscription_runtime(operation_id, input_id)
            .is_some_and(|runtime| runtime.declares_transport_semantics());

    if both_scopes {
        return Err(vec![SerializationObstacle::TransportSemanticsAtBothScopes {
            input: input_id.clone(),
            topic: topic_id,
        }]);
    }

    let grouping = model.effective_grouping(operation_id, input_id, &topic_id);

    let (Some(grouping_key), Some(topic)) = (grouping.as_ref(), model.topics.get(&topic_id)) else {
        return Err(vec![SerializationObstacle::NoGroupingDomain {
            input: input_id.clone(),
            topic: topic_id,
        }]);
    };

    let scope = if model.topic_scoped_transport(&topic_id) {
        GroupingScope::Topic {
            topic: topic_id.clone(),
        }
    } else {
        GroupingScope::Subscription {
            operation: operation_id.clone(),
            input: input_id.clone(),
        }
    };

    let admitted: Vec<&Id> = match &subscription.messages {
        MessageSelector::All => topic.messages.iter().collect(),
        MessageSelector::Only(messages) => messages.iter().collect(),
    };

    let mut facts = Vec::new();
    let mut obstacles = Vec::new();

    for schema in admitted {
        let Some(mapped) = grouping_key.mapping.get(schema) else {
            obstacles.push(SerializationObstacle::GroupingKeyMappingMissing {
                input: input_id.clone(),
                topic: topic_id.clone(),
                schema: schema.clone(),
            });

            continue;
        };

        // A grouping key is a tuple. Every component must carry the
        // requirement key's value, for the same reason a request
        // routing key must: a wider tuple partitions same-key
        // deliveries across groups, so equality of the requirement key
        // would no longer imply a common group.
        if mapped.is_empty() {
            obstacles.push(SerializationObstacle::EmptyGroupingKey {
                input: input_id.clone(),
                topic: topic_id.clone(),
                schema: schema.clone(),
            });

            continue;
        }

        for component in mapped {
            match key_identity(model, schema, component, requirement_key) {
                Some(identity) => facts.push(MessageKeyFact {
                    schema: schema.clone(),
                    grouping_key: component.clone(),
                    identity,
                }),

                None => obstacles.push(SerializationObstacle::KeyIdentityUnestablished {
                    input: input_id.clone(),
                    topic: topic_id.clone(),
                    schema: schema.clone(),
                    grouping_key: component.clone(),
                }),
            }
        }
    }

    if obstacles.is_empty() {
        Ok(GroupingFacts {
            topic: topic_id,
            scope,
            message_keys: facts,
        })
    } else {
        Err(obstacles)
    }
}

/// Whether two paths denote the same logical value in any instance of
/// the schema.
fn key_identity(
    model: &Model,
    schema: &Id,
    declared: &FieldPath,
    requirement_key: &FieldPath,
) -> Option<KeyIdentity> {
    if declared == requirement_key {
        return Some(KeyIdentity::SamePath);
    }

    let declared_canonical = canonical_value_path(model, schema, declared)?;
    let requirement_canonical = canonical_value_path(model, schema, requirement_key)?;

    (declared_canonical == requirement_canonical).then_some(KeyIdentity::SameCanonicalValue {
        schema: requirement_canonical.schema,
        path: requirement_canonical.path,
    })
}

impl SerializationCheck {
    /// The diagnostic for an unproven requirement.
    ///
    /// A proven requirement produces no diagnostic; its argument
    /// lives in the structured verdict.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let SerializationVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let requirement = self.requirement;
        let key = describe_value_ref(&self.key);

        Some(Diagnostic {
            code: DiagnosticCode::Verification(VerificationCode::SerializationUnproven),
            severity: Severity::Unknown,
            subject: Some(operation.clone()),
            message: format!(
                "Serialization requirement {requirement} of `{operation}` is not \
                 established: no declared facts prove that invocations sharing \
                 {key} never execute concurrently."
            ),
            evidence: obstacles
                .iter()
                .map(|obstacle| obstacle.evidence(self))
                .collect(),
        })
    }
}

impl SerializationObstacle {
    fn evidence(&self, check: &SerializationCheck) -> Evidence {
        match self {
            Self::KeyNotFromInput { source } => Evidence {
                subject: value_source_id(source).cloned(),
                message: format!(
                    "The serialization key is sourced from {}, not from an \
                     input declared by the operation; no routing fact selects \
                     which invocations share such a key.",
                    describe_value_source(source)
                ),
            },

            Self::AmbiguousRouter { input, routers } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` is served by more than one router ({}). Their \
                     declarations may contradict, so there is no single routing \
                     fact to reason from.",
                    routers
                        .iter()
                        .map(Id::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
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

            Self::EmptyRoutingKey { router, .. } => Evidence {
                subject: Some(router.clone()),
                message: format!(
                    "`{router}` declares an empty routing key, which names no \
                     routing domain."
                ),
            },

            Self::MemberAssignmentNotExclusive { input, declared } => Evidence {
                subject: Some(input.clone()),
                message: match declared {
                    MemberAssignment::RoundRobin => format!(
                        "`{input}` declares `round_robin` member assignment, which \
                         rotates through members irrespective of routing domain: \
                         same-key invocations land on different members and may \
                         execute at once."
                    ),

                    _ => format!(
                        "The member assignment declared for `{input}` does not give \
                         a routing domain one active owning member, so same-key \
                         invocations may execute on different members."
                    ),
                },
            },

            Self::EmptyGroupingKey { topic, schema, .. } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "The grouping in effect for `{topic}` maps `{schema}` to an \
                     empty tuple, which names no group."
                ),
            },

            Self::NoRouter { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "Key-bearing invocations arrive through request input \
                     `{input}`, and no router declares where they execute; \
                     without an execution-pool assignment there is no member \
                     concurrency to reason from."
                ),
            },

            Self::NoSubscriptionRuntime { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "Subscription input `{input}` declares no runtime, so \
                     nothing says where its deliveries execute or how many may \
                     execute at once."
                ),
            },

            Self::RoutingAbsent { input, pool } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` is assigned to `{pool}` but declares no routing: \
                     the target execution population is known, and no fact \
                     relates same-key invocations to a common member."
                ),
            },

            Self::RoutingKeyNotEquivalent {
                router, component, ..
            } => Evidence {
                subject: Some(router.clone()),
                message: format!(
                    "Routing-key component `{component}` of `{router}` is not \
                     established to carry the same logical value as the \
                     serialization key `{}`, so same-key invocations may fall \
                     into different routing domains.",
                    check.key.path
                ),
            },

            Self::RoutingKeyWiderThanRequirement { router, key, .. } => Evidence {
                subject: Some(router.clone()),
                message: format!(
                    "`{router}` routes by the tuple ({}), which partitions \
                     same-`{}` invocations further; equal serialization keys \
                     therefore need not share a routing domain.",
                    key.iter()
                        .map(FieldPath::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    check.key.path
                ),
            },

            Self::NoGroupingDomain { input, topic } => Evidence {
                subject: Some(topic.clone()),
                message: format!(
                    "No keyed grouping is in effect for `{input}` on `{topic}`, so the \
                     `grouping_key` routing of `{input}` has no domain to route by."
                ),
            },

            Self::GroupingKeyMappingMissing { topic, schema, .. } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "The grouping in effect for `{topic}` declares no key mapping for \
                     admitted schema `{schema}`."
                ),
            },

            Self::KeyIdentityUnestablished {
                schema,
                grouping_key,
                ..
            } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "For messages of `{schema}`, the grouping key `{grouping_key}` is \
                     not established to carry the same logical value as the \
                     serialization key `{}`, so same-key deliveries may land in \
                     different runtime groups.",
                    check.key.path
                ),
            },

            Self::PoolUndeclared { input, pool } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "`{input}` is assigned to execution pool `{pool}`, which \
                     the runtime model does not declare, so its member \
                     concurrency is unknown."
                ),
            },

            Self::MemberConcurrencyNotSerial { pool, declared, .. } => Evidence {
                subject: Some(pool.clone()),
                message: match declared {
                    MemberConcurrency::Unspecified => format!(
                        "Execution pool `{pool}` declares no member-concurrency \
                         fact, so simultaneous execution on one member cannot \
                         be excluded."
                    ),

                    MemberConcurrency::Unbounded => format!(
                        "Execution pool `{pool}` declares `unbounded` member \
                         concurrency: one member may run any number of \
                         invocations at once."
                    ),

                    MemberConcurrency::Bounded(bound) => format!(
                        "Execution pool `{pool}` permits {bound} simultaneous \
                         invocations per member; serialization needs a bound \
                         of one."
                    ),
                },
            },
        }
    }
}
