//! The obligation report: the flattened, presentation-oriented
//! projection of verification results.
//!
//! Obligations are enumerated per declared requirement. A proven
//! obligation carries the declared facts its proof relies on — per
//! §25, a proof is conditional on the implementation conforming to
//! them. An unproven obligation carries the checker's evidence:
//! exactly which facts are missing or insufficient. `unknown` is
//! epistemic (§1.2), never a violation; V1 produces no `disproven`
//! verdicts, though the format admits them for future checkers.
//!
//! `scaffold` enumerates every obligation the declared requirements
//! imply, all `unknown` — executable documentation of the shape.
//! `obligations` fills the same enumeration in from a real
//! `VerificationReport`.

use serde::{Deserialize, Serialize};

use crate::analyzer::verification::{
    self, ArtifactReplay, ConsumerCollapse, DecisionReplay, DecisionRule, EffectSafety,
    IdempotencyProof, IdempotencyVerdict, InstanceStability, KeyIdentity, PathRef,
    RecoverabilityProof, RecoverabilityVerdict, Resolution, ResultReplayProof, ResultReplayVerdict,
    ResultStabilityRule, RetryDriver, RetryRoute, SerializationProof, SerializationVerdict,
    StableRoot, VerificationReport,
};
use crate::analyzer::verification::{
    DuplicateHandling, LineageFact, ModelNote, OrderingProof, OrderingVerdict, PrecedenceSource,
    ProofScope,
};
use crate::spec::{
    CompletionRequirement, Id, MemberAssignment, Model, ResultReplayRequirement,
    SubscriptionRoutingKey, ValueRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProverReport {
    /// Version of this report format, not of the model.
    pub format: u32,

    /// Revision of the model the report was produced against.
    ///
    /// The visualization warns when this disagrees with the rendered
    /// model's revision.
    pub model_revision: Option<u64>,

    pub obligations: Vec<Obligation>,

    /// Model-wide notes that belong to no single obligation: warnings
    /// the checker raises about gaps no declaration covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<EvidenceItem>,
}

/// The current report format. Format 2 replaced the response-replay
/// property with result replay, dropped object-history obligations and
/// the flow subject, and made proofs cite program paths. Format 3 added
/// proof `scope` and rebuilt the serialization and ordering arguments
/// on the L1 runtime model — routing domains, member assignment, and
/// execution-pool member concurrency in place of dispatch lanes and
/// operation-global concurrency.
pub const FORMAT: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
    /// Stable identity of the obligation within the report.
    pub id: String,

    pub property: Property,
    pub subject: Subject,
    pub status: Status,

    /// One-line human-readable statement of the obligation.
    pub summary: String,

    /// Which semantic layers a proof consumed. `runtime_dependent`
    /// means the argument rests on at least one declared L1 fact, so
    /// it must be re-examined whenever the runtime realization
    /// changes. Absent for an obligation that is not proven.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ProofScope>,

    /// Declared model facts the verdict relies on. A proof is
    /// conditional on the implementation conforming to these.
    #[serde(default)]
    pub assumptions: Vec<String>,

    /// Model facts explaining how the verdict was reached.
    #[serde(default)]
    pub evidence: Vec<EvidenceItem>,

    /// Present only when `status` is `disproven`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterexample: Option<Counterexample>,
}

/// The correctness property an obligation discharges.
///
/// The first four mirror `OperationRequirements`; `result_replay`
/// splits out the result half of an idempotency requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Property {
    Serialization,
    Ordering,
    Idempotency,
    Recoverability,
    ResultReplay,
    Custom { name: String },
}

/// The model entity an obligation is anchored to.
///
/// `requirement` indexes into the corresponding requirement list on
/// the operation, tying the obligation back to the declaration that
/// produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    Operation {
        operation: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requirement: Option<usize>,
    },
    Transaction {
        operation: Id,
        transaction: Id,
    },
    Object {
        data_model: Id,
        object: Id,
    },
    StateMachine {
        machine: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition: Option<Id>,
    },
    Topic {
        topic: Id,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The property follows for all executions admitted by the model.
    Proven,

    /// The solver found an admitted execution violating the property.
    Disproven,

    /// The solver could not decide, typically because a required fact
    /// is `unspecified`. Not evidence of a violation.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceItem {
    /// Model entity the fact concerns, when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Id>,

    pub message: String,
}

/// A concrete admitted execution that violates the property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counterexample {
    pub trace: Vec<TraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceStep {
    /// Entity performing the step (an operation, topic, or the
    /// environment), when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Id>,

    pub description: String,
}

/// Builds a scaffold report enumerating every obligation the declared
/// requirements imply, all with status `unknown`.
///
/// Doubles as executable documentation of the format and as the shape
/// `obligations` fills in from real verification results.
pub fn scaffold(model: &Model) -> ProverReport {
    let mut obligations = Vec::new();

    fn requirement_obligation(
        op_id: &Id,
        property: Property,
        index: usize,
        summary: String,
    ) -> Obligation {
        Obligation {
            id: format!("oblig.{}.{}.{}", op_id, property_slug(&property), index),
            property,
            subject: Subject::Operation {
                operation: op_id.clone(),
                requirement: Some(index),
            },
            status: Status::Unknown,
            summary,
            scope: None,
            assumptions: Vec::new(),
            evidence: Vec::new(),
            counterexample: None,
        }
    }

    for (op_id, op) in &model.operations {
        let push = |obligations: &mut Vec<Obligation>,
                    property: Property,
                    index: usize,
                    summary: String| {
            obligations.push(requirement_obligation(op_id, property, index, summary));
        };

        for (i, r) in op.requirements.serialization.iter().enumerate() {
            push(
                &mut obligations,
                Property::Serialization,
                i,
                format!(
                    "Invocations of {op_id} sharing key {} never overlap.",
                    value_ref_label(&r.key)
                ),
            );
        }

        for (i, r) in op.requirements.ordering.iter().enumerate() {
            push(
                &mut obligations,
                Property::Ordering,
                i,
                format!(
                    "Invocations of {op_id} sharing key {} take effect in \
                     their semantic order.",
                    value_ref_label(&r.key)
                ),
            );
        }

        for (i, r) in op.requirements.idempotency.iter().enumerate() {
            push(
                &mut obligations,
                Property::Idempotency,
                i,
                format!(
                    "Repeated attempts at {op_id} sharing the declared key \
                     produce the effects of a single invocation.",
                ),
            );

            if r.result == ResultReplayRequirement::ReplayConsistent {
                obligations.push(Obligation {
                    id: format!("oblig.{op_id}.result_replay.{i}"),
                    property: Property::ResultReplay,
                    subject: Subject::Operation {
                        operation: op_id.clone(),
                        requirement: Some(i),
                    },
                    status: Status::Unknown,
                    summary: format!(
                        "Every attempt at {op_id} sharing the declared key \
                         returns an equivalent result."
                    ),
                    scope: None,
                    assumptions: Vec::new(),
                    evidence: Vec::new(),
                    counterexample: None,
                });
            }
        }

        for (i, r) in op.requirements.recoverability.iter().enumerate() {
            push(
                &mut obligations,
                Property::Recoverability,
                i,
                format!(
                    "An interrupted invocation of {op_id} {} a terminal of \
                     its program.",
                    match r.completion {
                        CompletionRequirement::Resumable => "can be resumed to reach",
                        CompletionRequirement::Guaranteed => "is re-driven until it reaches",
                    }
                ),
            );
        }
    }

    ProverReport {
        format: FORMAT,
        model_revision: Some(model.revision.0),
        obligations,
        notes: Vec::new(),
    }
}

/// Builds the obligation report from real verification results.
///
/// Every declared obligation appears, carrying its verdict — proofs
/// rendered as assumptions, obstacles as evidence.
pub fn obligations(model: &Model, verification: &VerificationReport) -> ProverReport {
    let mut report = scaffold(model);

    for check in &verification.serialization {
        let id = obligation_id(&check.operation, "serialization", check.requirement);

        patch(&mut report, &id, || match &check.verdict {
            SerializationVerdict::Proven { proof, scope } => {
                Ok((*scope, serialization_assumptions(proof)))
            }
            SerializationVerdict::Unproven { .. } => Err(check.diagnostic()),
        });
    }

    for check in &verification.ordering {
        let id = obligation_id(&check.operation, "ordering", check.requirement);

        patch(&mut report, &id, || match &check.verdict {
            OrderingVerdict::Proven { proof, scope } => Ok((*scope, ordering_assumptions(proof))),
            OrderingVerdict::Unproven { .. } => Err(check.diagnostic()),
        });
    }

    for check in &verification.idempotency {
        let id = obligation_id(&check.operation, "idempotency", check.requirement);

        patch(&mut report, &id, || match &check.verdict {
            IdempotencyVerdict::Proven { proof, scope } => {
                Ok((*scope, idempotency_assumptions(proof)))
            }
            IdempotencyVerdict::Unproven { .. } => Err(check.diagnostic()),
        });

        if let Some(obligation) = report
            .obligations
            .iter_mut()
            .find(|obligation| obligation.id == id)
        {
            if check.coinductive {
                obligation
                    .assumptions
                    .insert(0, coinductive_note("request targets or message consumers"));
            }

            // Lineage facts ride along: declared propagations become
            // assumptions the identity-based population rests on, and
            // their absence is evidence a reader wants next to it.
            for lineage in &check.lineage {
                let producer = match &lineage.producer {
                    verification::ProducerRef::Operation { operation, effect } => {
                        format!("{operation} through {effect}")
                    }
                    verification::ProducerRef::Transition {
                        machine,
                        transition,
                        effect,
                    } => format!("{machine}'s {transition} through {effect}"),
                };

                match &lineage.fact {
                    LineageFact::Propagated {
                        source,
                        requirement,
                    } => {
                        let key = source
                            .components
                            .iter()
                            .map(value_ref_label)
                            .collect::<Vec<_>>()
                            .join(" + ");

                        obligation.assumptions.push(match requirement {
                            Some(index) => format!(
                                "the identity of {} on {} is carried by its producer's \
                                 idempotency key ({key}, requirement #{index}): declared \
                                 propagation from {producer}",
                                lineage.schema, lineage.topic
                            ),
                            None => format!(
                                "the identity of {} on {} carries {key} by declared \
                                 propagation from {producer}",
                                lineage.schema, lineage.topic
                            ),
                        });
                    }

                    LineageFact::Undeclared => obligation.evidence.push(EvidenceItem {
                        subject: match &lineage.producer {
                            verification::ProducerRef::Operation { effect, .. }
                            | verification::ProducerRef::Transition { effect, .. } => {
                                Some(effect.clone())
                            }
                        },
                        message: format!(
                            "{producer} publishes {} to {} without a declared \
                             propagation onto its identity fields; the identity this \
                             population rests on is the topic declaration alone.",
                            lineage.schema, lineage.topic
                        ),
                    }),
                }
            }
        }
    }

    for check in &verification.result_replay {
        let id = obligation_id(&check.operation, "result_replay", check.requirement);

        patch(&mut report, &id, || match &check.verdict {
            ResultReplayVerdict::Proven { proof, scope } => {
                Ok((*scope, result_replay_assumptions(proof)))
            }
            ResultReplayVerdict::Unproven { .. } => Err(check.diagnostic()),
        });

        if check.coinductive
            && let Some(obligation) = report
                .obligations
                .iter_mut()
                .find(|obligation| obligation.id == id)
        {
            obligation
                .assumptions
                .insert(0, coinductive_note("request effects"));
        }
    }

    for check in &verification.recoverability {
        let id = obligation_id(&check.operation, "recoverability", check.requirement);

        patch(&mut report, &id, || match &check.verdict {
            RecoverabilityVerdict::Proven { proof, scope } => {
                Ok((*scope, recoverability_assumptions(proof)))
            }
            RecoverabilityVerdict::Unproven { .. } => Err(check.diagnostic()),
        });

        // Notes ride along as evidence: facts a reader wants next to
        // the verdict, which they do not change.
        if let Some(obligation) = report
            .obligations
            .iter_mut()
            .find(|obligation| obligation.id == id)
        {
            obligation.evidence.extend(check.notes.iter().map(|note| {
                let evidence = note.evidence();

                EvidenceItem {
                    subject: evidence.subject,
                    message: evidence.message,
                }
            }));
        }
    }

    report.notes = verification
        .notes
        .iter()
        .map(|note: &ModelNote| EvidenceItem {
            subject: note.subject(),
            message: note.message(),
        })
        .collect();

    report
}

fn coinductive_note(through: &str) -> String {
    format!(
        "proven coinductively: this requirement and the ones it reaches through \
         {through} each rest on the others, and the greatest fixpoint admits the \
         cycle (effect-safety draft §4.1)"
    )
}

fn obligation_id(operation: &Id, slug: &str, requirement: usize) -> String {
    format!("oblig.{operation}.{slug}.{requirement}")
}

/// Applies one check's verdict to its scaffolded obligation: proven
/// verdicts contribute assumptions, unproven ones contribute the
/// diagnostic's evidence.
fn patch(
    report: &mut ProverReport,
    id: &str,
    verdict: impl FnOnce() -> Result<(ProofScope, Vec<String>), Option<crate::analyzer::Diagnostic>>,
) {
    let Some(obligation) = report
        .obligations
        .iter_mut()
        .find(|obligation| obligation.id == id)
    else {
        return;
    };

    match verdict() {
        Ok((scope, assumptions)) => {
            obligation.status = Status::Proven;
            obligation.scope = Some(scope);
            obligation.assumptions = assumptions;
        }

        Err(diagnostic) => {
            obligation.status = Status::Unknown;

            if let Some(diagnostic) = diagnostic {
                obligation.evidence = diagnostic
                    .evidence
                    .into_iter()
                    .map(|evidence| EvidenceItem {
                        subject: evidence.subject,
                        message: evidence.message,
                    })
                    .collect();
            }
        }
    }
}

fn property_slug(property: &Property) -> &str {
    match property {
        Property::Serialization => "serialization",
        Property::Ordering => "ordering",
        Property::Idempotency => "idempotency",
        Property::Recoverability => "recoverability",
        Property::ResultReplay => "result_replay",
        Property::Custom { name } => name,
    }
}

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

fn serialization_assumptions(proof: &SerializationProof) -> Vec<String> {
    match proof {
        SerializationProof::NoAdmittedInvocations { input } => vec![format!(
            "{input} admits no message schemas; the requirement constrains no \
             invocations"
        )],

        SerializationProof::RequestRouted {
            input,
            router,
            pool,
            routing_key,
            member_assignment,
        } => {
            let mut assumptions = vec![format!(
                "{router} routes invocations of {input} into {pool} by a semantic \
                 routing key"
            )];

            for component in routing_key {
                assumptions.push(match &component.identity {
                    KeyIdentity::SamePath => format!(
                        "the routing-key component {} is the serialization key field, \
                         so same-key invocations share one routing domain",
                        component.path
                    ),

                    KeyIdentity::SameCanonicalValue { schema, path } => format!(
                        "the routing-key component {} carries the serialization key's \
                         value ({schema}.{path} via fragment aliasing), so same-key \
                         invocations share one routing domain",
                        component.path
                    ),
                });
            }

            assumptions.push(member_assignment_assumption(member_assignment));

            assumptions.push(format!(
                "{pool} bounds each member to one simultaneously active invocation, so \
                 the owning member runs same-key invocations one at a time"
            ));

            assumptions
        }

        SerializationProof::SubscriptionRouted {
            input,
            topic,
            pool,
            message_keys,
            member_assignment,
        } => {
            let mut assumptions = vec![format!(
                "{topic} declares a keyed transport domain, and {input} dispatches \
                 into {pool} by topic_key, so same-key deliveries share one routing \
                 domain"
            )];

            for key in message_keys {
                assumptions.push(match &key.identity {
                    KeyIdentity::SamePath => format!(
                        "for {}, the topic key {} is the serialization key field",
                        key.schema, key.topic_key
                    ),

                    KeyIdentity::SameCanonicalValue { schema, path } => format!(
                        "for {}, the topic key {} carries the serialization key's \
                         value ({schema}.{path} via fragment aliasing)",
                        key.schema, key.topic_key
                    ),
                });
            }

            assumptions.push(member_assignment_assumption(member_assignment));

            assumptions.push(format!(
                "{pool} bounds each member to one simultaneously active invocation, so \
                 the owning member runs same-key invocations one at a time"
            ));

            assumptions
        }
    }
}

/// What a member assignment guarantees about domain ownership — the
/// step that turns routing-domain equality into a single executing
/// member.
fn member_assignment_assumption(assignment: &MemberAssignment) -> String {
    match assignment {
        MemberAssignment::ConsistentHash => "consistent_hash assignment gives each routing \
             domain one owning pool member at a time, and transfers that ownership safely \
             when membership changes"
            .to_string(),
    }
}

fn ordering_assumptions(proof: &OrderingProof) -> Vec<String> {
    match proof {
        OrderingProof::NoAdmittedInvocations { input } => vec![format!(
            "{input} admits no message schemas; no invocation bears the key and \
             no precedence exists to preserve"
        )],

        OrderingProof::RoutedOrder {
            input,
            topic,
            pool,
            precedence,
            routing_key,
            member_assignment,
            duplicates,
        } => {
            let mut assumptions = Vec::new();

            match precedence {
                PrecedenceSource::KeyedTopic { message_keys } => {
                    assumptions.push(format!(
                        "{topic} orders same-key messages (keyed transport ordering); \
                         that order is the precedence"
                    ));

                    for key in message_keys {
                        assumptions.push(match &key.identity {
                            KeyIdentity::SamePath => format!(
                                "for {}, the topic key {} is the ordering key field",
                                key.schema, key.topic_key
                            ),

                            KeyIdentity::SameCanonicalValue { schema, path } => format!(
                                "for {}, the topic key {} and the ordering key both \
                                 denote {schema}.{path} through declared fragment mappings",
                                key.schema, key.topic_key
                            ),
                        });
                    }
                }

                PrecedenceSource::GlobalTopic => assumptions.push(format!(
                    "{topic} orders every message (global transport ordering); that \
                     order is the precedence for any key"
                )),
            }

            assumptions.push(match routing_key {
                SubscriptionRoutingKey::TopicKey => format!(
                    "{input} dispatches by topic_key, so same-key deliveries belong to \
                     one semantic routing domain"
                ),
            });

            assumptions.push(member_assignment_assumption(member_assignment));

            assumptions.push(format!(
                "{pool} bounds each member to one simultaneously active invocation, so a \
                 later invocation cannot overtake an earlier one"
            ));

            match duplicates {
                DuplicateHandling::SingleDelivery => assumptions.push(format!(
                    "{input} receives each logical message at most once, so neither \
                     redelivery nor a duplicate exists"
                )),

                DuplicateHandling::OrderPreservingRedelivery { idempotency } => {
                    assumptions.push(
                        "dispatch preserves the transport's established same-key \
                         precedence when admitting invocations, including across \
                         failure-driven redelivery and ownership reassignment"
                            .to_string(),
                    );

                    assumptions.push(match idempotency {
                        Some(coverage) => format!(
                            "a duplicate of a completed delivery repeats an invocation \
                             that already took effect in order; its work is idempotency \
                             requirement #{}'s obligation ({})",
                            coverage.requirement,
                            if coverage.proven {
                                "proven"
                            } else {
                                "unproven"
                            }
                        ),

                        None => format!(
                            "a duplicate of a completed delivery repeats an invocation \
                             that already took effect in order; no idempotency \
                             requirement keyed from {input} answers for its work"
                        ),
                    });
                }
            }

            assumptions
        }
    }
}

fn idempotency_assumptions(proof: &IdempotencyProof) -> Vec<String> {
    match proof {
        IdempotencyProof::NoAdmittedInvocations { input } => vec![format!(
            "{input} admits no message schemas; no attempt can bear the key"
        )],

        IdempotencyProof::NoAdmittedPaths { input } => vec![format!(
            "no path of the program is admitted for {input}; an attempt performs no \
             modeled work"
        )],

        IdempotencyProof::SingleDelivery { input, topic } => vec![format!(
            "{input} receives at-most-once delivery from {topic}, whose message \
             identity is pinned by the key: a class holds at most one attempt"
        )],

        IdempotencyProof::RetrySafePaths { paths } => {
            let mut assumptions = Vec::new();

            for path in paths {
                let prefix = path_prefix(paths.len(), &path.path);

                assumptions.extend(decision_assumptions(&prefix, &path.decisions));

                for transaction in &path.transactions {
                    assumptions.push(match &transaction.route {
                        RetryRoute::KeyedCommit { key } => format!(
                            "{prefix}{} commits are deduplicated by {}, stable \
                             across the attempt class",
                            transaction.transaction,
                            root_labels(key)
                        ),

                        RetryRoute::NaturalReplay => format!(
                            "{prefix}{} is naturally replayable: re-execution \
                             reproduces the same logical state",
                            transaction.transaction
                        ),
                    });
                }

                for effect in &path.effects {
                    match &effect.safety {
                        EffectSafety::ExternallyDeduplicated { key } => assumptions.push(format!(
                            "{prefix}the external boundary of {} deduplicates \
                             executions sharing {}",
                            effect.effect,
                            root_labels(key)
                        )),

                        EffectSafety::SameLogicalMessage {
                            topic,
                            schema,
                            instance,
                            consumers,
                        } => {
                            assumptions.push(format!(
                                "{prefix}duplicate executions of {} publish the same \
                                 logical message under {topic}'s message identity \
                                 ({})",
                                effect.effect,
                                instance_label(instance)
                            ));

                            if consumers.is_empty() {
                                assumptions.push(format!(
                                    "{prefix}no modeled subscription on {topic} admits \
                                     {schema}; the cascade ends at the topic"
                                ));
                            }

                            for consumer in consumers {
                                assumptions.push(match consumer {
                                    ConsumerCollapse::ProvenRequirement { operation, input } => {
                                        format!(
                                            "{prefix}duplicate deliveries of {schema} to \
                                             {operation} via {input} fall into one proven \
                                             idempotency class"
                                        )
                                    }

                                    ConsumerCollapse::SingleDelivery { operation, input } => {
                                        format!(
                                            "{prefix}{operation} via {input} receives \
                                             {schema} at most once: one logical message \
                                             under at-most-once delivery"
                                        )
                                    }
                                });
                            }
                        }

                        EffectSafety::DeduplicatedByTarget {
                            operation,
                            input,
                            instance,
                        } => assumptions.push(format!(
                            "{prefix}duplicate requests of {} fall into one \
                             proven idempotency class of {operation} via {input} \
                             ({})",
                            effect.effect,
                            instance_label(instance)
                        )),
                    }
                }
            }

            assumptions
        }
    }
}

fn result_replay_assumptions(proof: &ResultReplayProof) -> Vec<String> {
    match proof {
        ResultReplayProof::NoAdmittedInvocations { input } => vec![format!(
            "{input} admits no message schemas; no attempt can bear the key"
        )],

        ResultReplayProof::NoReturnedResult { input } => vec![format!(
            "no admitted path returns a result for {input}; there is nothing \
             to stabilize"
        )],

        ResultReplayProof::ClassFixedResult { returns } => {
            let mut assumptions = Vec::new();

            for returned in returns {
                let prefix = path_prefix(returns.len(), &returned.path);

                assumptions.extend(decision_assumptions(&prefix, &returned.decisions));

                assumptions.push(format!(
                    "{prefix}the returned {} payload is derived deterministically from \
                     {}",
                    returned.variant,
                    root_labels(&returned.derivation)
                ));

                // Several roots may rest on one fact; state it once.
                let mut cited = Vec::new();

                for root in &returned.derivation {
                    if let Some(label) = root_rule_label(root)
                        && !cited.contains(&label)
                    {
                        assumptions.push(format!("{prefix}{label}"));
                        cited.push(label);
                    }
                }
            }

            assumptions
        }
    }
}

fn recoverability_assumptions(proof: &RecoverabilityProof) -> Vec<String> {
    match proof {
        RecoverabilityProof::NoAdmittedInvocations { input } => vec![format!(
            "{input} admits no message schemas; no attempt can bear the key"
        )],

        RecoverabilityProof::Resumable { paths } => resumption_assumptions(paths),

        RecoverabilityProof::Guaranteed { driver, paths } => {
            let mut assumptions = vec![match driver {
                RetryDriver::AtLeastOnceDelivery { input, topic } => format!(
                    "{input} redelivers via {topic} at least once, re-driving \
                     interrupted invocations"
                ),

                RetryDriver::InboundRepeatableRequest { operation, effect } => format!(
                    "{operation} may repeat its request through {effect}, \
                     re-driving interrupted invocations"
                ),

                RetryDriver::InboundRepeatableTransitionEffect {
                    machine,
                    transition,
                    effect,
                } => format!(
                    "transition {transition} of {machine} may repeat its request \
                     through {effect}, re-driving interrupted invocations"
                ),
            }];

            assumptions.extend(resumption_assumptions(paths));

            assumptions
        }
    }
}

fn resumption_assumptions(paths: &[verification::PathResumption]) -> Vec<String> {
    let mut assumptions = Vec::new();

    for path in paths {
        let prefix = path_prefix(paths.len(), &path.path);

        for transaction in &path.transactions {
            assumptions.push(match &transaction.resolution {
                Resolution::KeyedCommit { key } => format!(
                    "{prefix}{} resolves on re-encounter through its keyed \
                     commit ({})",
                    transaction.transaction,
                    root_labels(key)
                ),

                Resolution::NaturalReplay => format!(
                    "{prefix}{} re-executes safely by natural replay",
                    transaction.transaction
                ),

                Resolution::TerminalStep => format!(
                    "{prefix}{} is the path's final step before completion; no \
                     failing prefix follows its commit",
                    transaction.transaction
                ),
            });
        }

        for artifact in &path.artifacts {
            assumptions.push(match &artifact.replay {
                ArtifactReplay::Recovered { transaction, .. } => format!(
                    "{prefix}artifact {} is recovered from {transaction}'s keyed \
                     commit on resumption",
                    artifact.artifact
                ),

                ArtifactReplay::Reconstructed { transaction, .. } => format!(
                    "{prefix}artifact {} is reconstructed by naturally replaying \
                     {transaction}",
                    artifact.artifact
                ),

                ArtifactReplay::Unavailable { transaction, .. } => format!(
                    "{prefix}artifact {} is supplied by {transaction}",
                    artifact.artifact
                ),
            });
        }
    }

    assumptions
}

/// The facts fixing each decision of a path: one line per decision.
fn decision_assumptions(prefix: &str, decisions: &[DecisionReplay]) -> Vec<String> {
    decisions
        .iter()
        .map(|decision| {
            let taken = match &decision.decision {
                verification::DecisionTaken::Match { result, arm, .. } => {
                    format!("the match on {result} takes its {arm} arm on every attempt")
                }

                verification::DecisionTaken::Branch { location, arm } => {
                    format!("the branch at step {location} takes its {arm} arm on every attempt")
                }
            };

            let because = match &decision.rule {
                DecisionRule::StableResult { effect, rule, .. } => match rule {
                    ResultStabilityRule::ReplayConsistentTarget {
                        operation, input, ..
                    } => format!(
                        "{effect} sends a class-fixed request into {operation} via {input}, \
                         whose result replay is proven"
                    ),

                    ResultStabilityRule::ExternalTerminalResult { variant, .. } => format!(
                        "{effect} deduplicates by a class-fixed external key, which fixes \
                         the interaction's terminal result, and the observed {variant} is \
                         terminal"
                    ),
                },

                DecisionRule::StableCondition { roots } => {
                    format!("the condition is deterministic over {}", root_labels(roots))
                }
            };

            format!("{prefix}{taken}: {because}")
        })
        .collect()
}

/// The prefix naming a path when the proof spans more than one.
fn path_prefix(path_count: usize, path: &PathRef) -> String {
    if path_count > 1 {
        format!("on {}: ", verification::path_label(path))
    } else {
        String::new()
    }
}

fn root_labels(roots: &[StableRoot]) -> String {
    if roots.is_empty() {
        return "its declared key".to_string();
    }

    roots
        .iter()
        .map(|root| value_ref_label(&root.root))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A root whose stability rests on a fact worth stating on its own
/// line: an artifact or an effect result.
fn root_rule_label(root: &StableRoot) -> Option<String> {
    match &root.rule {
        verification::StabilityRule::RecoveredArtifact { transaction } => Some(format!(
            "{} is recovered from {transaction}'s keyed commit on every retry",
            root.root.source.id()
        )),

        verification::StabilityRule::ReconstructedArtifact { transaction } => Some(format!(
            "{} is reconstructed deterministically by naturally replaying {transaction}",
            root.root.source.id()
        )),

        verification::StabilityRule::ReplayConsistentResult { result, effect } => Some(format!(
            "result {result} of {effect} is observed equally by every attempt: the \
             target proves its result replay-consistent"
        )),

        verification::StabilityRule::DeduplicatedExternalResult {
            result,
            effect,
            variant,
        } => Some(format!(
            "the {variant} of result {result} is observed equally by every attempt: \
             {effect} deduplicates by a class-fixed external key, which fixes the \
             interaction's terminal result"
        )),

        _ => None,
    }
}

fn instance_label(instance: &InstanceStability) -> String {
    match instance {
        InstanceStability::ReplayDeterministic { .. } => {
            "the instance is replay-deterministic".to_string()
        }

        InstanceStability::EstablishedIntent { intent, replay } => match replay {
            ArtifactReplay::Recovered { transaction, .. } => {
                format!("intent {intent} is recovered from {transaction}'s keyed commit")
            }

            ArtifactReplay::Reconstructed { transaction, .. } => {
                format!("intent {intent} is reconstructed by naturally replaying {transaction}")
            }

            ArtifactReplay::Unavailable { .. } => format!("intent {intent}"),
        },
    }
}
