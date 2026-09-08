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
//!    The dispatch routes by `topic_key`, the topic runtime declares
//!    the keyed domain that key names, and the serialization key is
//!    established to carry the same logical value as the topic key for
//!    every admitted message schema.
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
//! - **Topic ordering alone** (§6). Delivery order does not serialize
//!   consumer execution; the subscription route uses the keyed
//!   declaration only for the key domain that routing references.
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
    SerializationRequirement, SubscriptionInput, SubscriptionRoutingKey, TopicOrdering, ValueRef,
    ValueSource,
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

    /// The delivery-side counterpart: same-topic-key deliveries share
    /// a routing domain, one member owns it, and that member executes
    /// one invocation at a time.
    SubscriptionRouted {
        input: Id,
        topic: Id,
        pool: Id,

        /// Per admitted message schema, the fact identifying the
        /// topic key with the serialization key.
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

/// For one component of a request routing key, why it denotes the same
/// logical value as the serialization key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingKeyFact {
    pub path: FieldPath,
    pub identity: KeyIdentity,
}

/// For one admitted message schema, how the topic's ordering key was
/// identified with the serialization key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageKeyFact {
    pub schema: Id,

    /// The topic's declared ordering-key path for this schema.
    pub topic_key: FieldPath,

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
    /// invocations may fall into different routing domains. An empty
    /// routing key — which validation rejects — reports an empty
    /// component here rather than proving vacuously.
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

    /// Dispatch routes by `topic_key`, but the subscribed topic's
    /// runtime declares no keyed ordering domain to route by.
    TopicNotKeyed { input: Id, topic: Id },

    /// The keyed topic declares no ordering-key mapping for an
    /// admitted schema. Validation rejects this shape; verification
    /// records it rather than assuming a mapping.
    TopicKeyMappingMissing { input: Id, topic: Id, schema: Id },

    /// The topic's ordering key for this schema is not established to
    /// carry the same logical value as the serialization key, so
    /// same-key deliveries may enter different routing domains.
    KeyIdentityUnestablished {
        input: Id,
        topic: Id,
        schema: Id,
        topic_key: FieldPath,
    },

    /// The target execution pool is not declared, so its member
    /// concurrency is unknown.
    PoolUndeclared { input: Id, pool: Id },

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
    let Some((router_id, router)) = model.router_for(operation_id, input_id) else {
        return SerializationVerdict::Unproven {
            obstacles: vec![SerializationObstacle::NoRouter {
                input: input_id.clone(),
            }],
        };
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

    match routing_key {
        Some((routing_key, member_assignment)) if serial => {
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

/// The delivery-side route: dispatch, topic-key equivalence, member
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
            SubscriptionRoutingKey::TopicKey => {
                match topic_key_facts(model, input_id, subscription, key) {
                    Ok((topic, message_keys)) => {
                        Some((topic, message_keys, routing.member_assignment))
                    }

                    Err(routing_obstacles) => {
                        obstacles.extend(routing_obstacles);

                        None
                    }
                }
            }
        },
    };

    let serial = pool_is_serial(model, input_id, &runtime.dispatch.pool, &mut obstacles);

    match routed {
        Some((topic, message_keys, member_assignment)) if serial => {
            SerializationVerdict::proven(SerializationProof::SubscriptionRouted {
                input: input_id.clone(),
                topic,
                pool: runtime.dispatch.pool.clone(),
                message_keys,
                member_assignment,
            })
        }

        _ => SerializationVerdict::Unproven { obstacles },
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
        return Err(vec![SerializationObstacle::RoutingKeyNotEquivalent {
            input: input_id.clone(),
            router: router_id.clone(),
            component: FieldPath(Vec::new()),
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

/// Establishes routing-domain equivalence for `key: topic_key`: for
/// every admitted message schema, the topic's ordering key must carry
/// the same logical value as the requirement key.
pub(super) fn topic_key_facts(
    model: &Model,
    input_id: &Id,
    subscription: &SubscriptionInput,
    requirement_key: &FieldPath,
) -> Result<(Id, Vec<MessageKeyFact>), Vec<SerializationObstacle>> {
    let topic_id = subscription.topic.clone();

    let TopicOrdering::Keyed(topic_key) = model.topic_ordering(&topic_id) else {
        return Err(vec![SerializationObstacle::TopicNotKeyed {
            input: input_id.clone(),
            topic: topic_id,
        }]);
    };

    let Some(topic) = model.topics.get(&topic_id) else {
        return Err(vec![SerializationObstacle::TopicNotKeyed {
            input: input_id.clone(),
            topic: topic_id,
        }]);
    };

    let admitted: Vec<&Id> = match &subscription.messages {
        MessageSelector::All => topic.messages.iter().collect(),
        MessageSelector::Only(messages) => messages.iter().collect(),
    };

    let mut facts = Vec::new();
    let mut obstacles = Vec::new();

    for schema in admitted {
        let Some(mapped) = topic_key.mapping.get(schema) else {
            obstacles.push(SerializationObstacle::TopicKeyMappingMissing {
                input: input_id.clone(),
                topic: topic_id.clone(),
                schema: schema.clone(),
            });

            continue;
        };

        match key_identity(model, schema, mapped, requirement_key) {
            Some(identity) => facts.push(MessageKeyFact {
                schema: schema.clone(),
                topic_key: mapped.clone(),
                identity,
            }),

            None => obstacles.push(SerializationObstacle::KeyIdentityUnestablished {
                input: input_id.clone(),
                topic: topic_id.clone(),
                schema: schema.clone(),
                topic_key: mapped.clone(),
            }),
        }
    }

    if obstacles.is_empty() {
        Ok((topic_id, facts))
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
            } if component.0.is_empty() => Evidence {
                subject: Some(router.clone()),
                message: format!(
                    "`{router}` declares an empty routing key, which names no \
                     routing domain."
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

            Self::TopicNotKeyed { input, topic } => Evidence {
                subject: Some(topic.clone()),
                message: format!(
                    "The runtime for `{topic}` declares no keyed ordering, so \
                     the `topic_key` routing of `{input}` has no semantic key \
                     domain to route by."
                ),
            },

            Self::TopicKeyMappingMissing { topic, schema, .. } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "The keyed runtime ordering of `{topic}` declares no \
                     key mapping for admitted schema `{schema}`."
                ),
            },

            Self::KeyIdentityUnestablished {
                schema, topic_key, ..
            } => Evidence {
                subject: Some(schema.clone()),
                message: format!(
                    "For messages of `{schema}`, the topic's ordering key \
                     `{topic_key}` is not established to carry the same \
                     logical value as the serialization key `{}`, so same-key \
                     deliveries may enter different routing domains.",
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
