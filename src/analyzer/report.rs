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
    self, ArtifactReplay, CommitArtifact, CommitOrderEvidence, ConsumerCollapse, DecisionReplay,
    DecisionRule, EffectSafety, IdempotencyProof, IdempotencyVerdict, InstanceStability,
    LineageFact, ModelNote, PathRef, ProofScope, RecoverabilityProof, RecoverabilityVerdict,
    RemedyLayer, Resolution, ResultReplayProof, ResultReplayVerdict, ResultStabilityRule,
    RetryDriver, RetryRoute, StableRoot, TransactionOrderingProof, TransactionOrderingVerdict,
    TransactionRef, TransactionSerializabilityProof, TransactionSerializabilityVerdict,
    VerificationReport,
};
use crate::spec::{
    CompletionRequirement, CursorAdvanceRule, Id, Model, ResultReplayRequirement, ValueRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProverReport {
    /// Version of this report format, not of the model.
    pub format: u32,

    /// The DSL contract version the verdicts are relative to: the
    /// same model text can prove differently across a contract bump,
    /// so an archived report without it is ambiguous.
    #[serde(default)]
    pub dsl: Option<crate::spec::DslVersion>,

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
/// on the L1 runtime model. Format 4 added `remedy` to unproven
/// serialization and ordering obligations. Format 5 added the `dsl`
/// contract version the verdicts are relative to, and rebuilt the
/// external-boundary evidence on the identity / idempotency /
/// result-replay decomposition. Format 6 split the ownership leg of
/// the topology serialization and ordering arguments and added the L0
/// invocation-lock serialization proof. Format 7 retires the
/// operation-level serialization and ordering families with every
/// topology and invocation-lock proof, and replaces them with the
/// transaction serializability and ordering families: obligations
/// anchored to a transaction, proven from the model-wide conflict
/// closure — serializable isolation, strict locks, version validation,
/// ordered cursors, fences — and never from L1.
pub const FORMAT: u32 = 7;

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

    /// Which semantic layer holds the facts an unproven obligation is
    /// waiting on. The dual of `scope`: that records the layers a
    /// proof consumed, this records the layer a missing proof needs.
    /// Absent for a proven obligation, and for families whose
    /// obstacles are not yet classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<RemedyLayer>,

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
/// The two transaction properties mirror `TransactionRequirements`;
/// idempotency and recoverability mirror `OperationRequirements`, and
/// `result_replay` splits out the result half of an idempotency
/// requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Property {
    TransactionSerializability,
    TransactionOrdering,
    Idempotency,
    Recoverability,
    ResultReplay,
    Custom { name: String },
}

/// The model entity an obligation is anchored to.
///
/// `requirement` indexes into the corresponding requirement list on
/// the operation or transaction, tying the obligation back to the
/// declaration that produced it.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requirement: Option<usize>,
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
            remedy: None,
            assumptions: Vec::new(),
            evidence: Vec::new(),
            counterexample: None,
        }
    }

    fn transaction_obligation(
        op_id: &Id,
        transaction: &Id,
        property: Property,
        index: usize,
        summary: String,
    ) -> Obligation {
        Obligation {
            id: transaction_obligation_id(op_id, transaction, property_slug(&property), index),
            property,
            subject: Subject::Transaction {
                operation: op_id.clone(),
                transaction: transaction.clone(),
                requirement: Some(index),
            },
            status: Status::Unknown,
            summary,
            scope: None,
            remedy: None,
            assumptions: Vec::new(),
            evidence: Vec::new(),
            counterexample: None,
        }
    }

    for (op_id, op) in &model.operations {
        // Transaction obligations first, in program order: they are
        // properties of committed state, which is what the operation's
        // own obligations build on.
        for (_, transaction) in op.program.transactions() {
            for (i, r) in transaction.requirements.serializability.iter().enumerate() {
                obligations.push(transaction_obligation(
                    op_id,
                    &transaction.id,
                    Property::TransactionSerializability,
                    i,
                    format!(
                        "Executions of {} sharing key {} commit in a history equivalent \
                         to some serial order, together with every transaction they may \
                         conflict with.",
                        transaction.id,
                        value_ref_label(&r.key)
                    ),
                ));
            }

            for (i, r) in transaction.requirements.ordering.iter().enumerate() {
                obligations.push(transaction_obligation(
                    op_id,
                    &transaction.id,
                    Property::TransactionOrdering,
                    i,
                    format!(
                        "Executions of {} sharing key {} take effect in the order of {}.",
                        transaction.id,
                        value_ref_label(&r.key),
                        value_ref_label(&r.position)
                    ),
                ));
            }
        }

        let push = |obligations: &mut Vec<Obligation>,
                    property: Property,
                    index: usize,
                    summary: String| {
            obligations.push(requirement_obligation(op_id, property, index, summary));
        };

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
                    remedy: None,
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
        dsl: Some(crate::spec::DSL_VERSION),
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

    for check in &verification.transaction_serializability {
        let id = transaction_obligation_id(
            &check.operation,
            &check.transaction,
            "transaction_serializability",
            check.requirement,
        );

        patch(&mut report, &id, || match &check.verdict {
            TransactionSerializabilityVerdict::Proven { proof, scope } => {
                let mut assumptions = transaction_serializability_assumptions(proof);

                assumptions.extend(commit_artifact_assumptions(&check.artifacts));

                Ok((*scope, assumptions))
            }
            TransactionSerializabilityVerdict::Unproven { .. } => Err(check.diagnostic()),
        });

        set_remedy(&mut report, &id, check.remedy());
    }

    for check in &verification.transaction_ordering {
        let id = transaction_obligation_id(
            &check.operation,
            &check.transaction,
            "transaction_ordering",
            check.requirement,
        );

        patch(&mut report, &id, || match &check.verdict {
            TransactionOrderingVerdict::Proven { proof, scope } => {
                let mut assumptions = transaction_ordering_assumptions(proof);

                assumptions.extend(commit_artifact_assumptions(&check.artifacts));

                Ok((*scope, assumptions))
            }
            TransactionOrderingVerdict::Unproven { .. } => Err(check.diagnostic()),
        });

        set_remedy(&mut report, &id, check.remedy());
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
            // The inert-continuation admission is derived proof
            // evidence: the analyzer proved it from program structure,
            // no implementation claim supplied it, and it lapses by
            // itself if an effectful step ever joins a continuation.
            if let IdempotencyVerdict::Proven {
                proof: IdempotencyProof::RetrySafePaths { paths },
                ..
            } = &check.verdict
            {
                for path in paths {
                    for decision in &path.decisions {
                        match &decision.rule {
                            DecisionRule::IdempotencyInertContinuation => {
                                obligation.evidence.push(EvidenceItem {
                                    subject: None,
                                    message: format!(
                                        "{} is not established to replay; every continuation \
                                         to a terminal is idempotency-inert, so divergence \
                                         cannot add modeled work and may affect only terminal \
                                         construction — which is the result-replay \
                                         obligation's concern, not this one's.",
                                        decision_label(&decision.decision),
                                    ),
                                });
                            }

                            DecisionRule::OutcomeDivergenceAddsNoWork { transaction } => {
                                obligation.evidence.push(EvidenceItem {
                                    subject: Some(transaction.clone()),
                                    message: format!(
                                        "{} is not established to replay; a rejection \
                                         commits nothing and performs only its rejection \
                                         block's work, judged duplicate-safe on its own \
                                         path, so divergence between rejection and commit \
                                         cannot duplicate modeled work — the returned \
                                         result is the result-replay obligation's concern.",
                                        decision_label(&decision.decision),
                                    ),
                                });
                            }

                            _ => {}
                        }
                    }
                }
            }

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

                let (source_label, source_kind) = match &lineage.source {
                    verification::LineageSource::Topic { topic } => (topic, "topic"),
                    verification::LineageSource::Outbox { outbox } => (outbox, "outbox"),
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
                                lineage.schema, source_label
                            ),
                            None => format!(
                                "the identity of {} on {} carries {key} by declared \
                                 propagation from {producer}",
                                lineage.schema, source_label
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
                            "{producer} produces {} into {} without a declared \
                             propagation onto its identity fields; the identity this \
                             population rests on is the {source_kind} declaration alone.",
                            lineage.schema, source_label
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

fn transaction_obligation_id(
    operation: &Id,
    transaction: &Id,
    slug: &str,
    requirement: usize,
) -> String {
    format!("oblig.{operation}.{transaction}.{slug}.{requirement}")
}

/// Records which layer an unproven obligation is waiting on.
///
/// Separate from `patch` because only the transaction families
/// classify their obstacles today; the rest leave it absent, which a
/// coordinator reads as the application layer.
fn set_remedy(report: &mut ProverReport, id: &str, remedy: Option<RemedyLayer>) {
    if let Some(obligation) = report
        .obligations
        .iter_mut()
        .find(|obligation| obligation.id == id)
    {
        obligation.remedy = remedy;
    }
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
        Property::TransactionSerializability => "transaction_serializability",
        Property::TransactionOrdering => "transaction_ordering",
        Property::Idempotency => "idempotency",
        Property::Recoverability => "recoverability",
        Property::ResultReplay => "result_replay",
        Property::Custom { name } => name,
    }
}

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

fn closure_label(closure: &[TransactionRef]) -> String {
    closure
        .iter()
        .map(|member| member.transaction.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn transaction_serializability_assumptions(proof: &TransactionSerializabilityProof) -> Vec<String> {
    match proof {
        TransactionSerializabilityProof::SerializableIsolationClosure { root, key, closure } => {
            vec![
                format!(
                    "the conflict closure of {root} under SerializableBy({}) is {{{}}}: every \
                     transaction that may touch overlapping state, transitively",
                    value_ref_label(key),
                    closure_label(closure)
                ),
                "every transaction in the closure declares isolation: serializable, so their \
                 committed history is equivalent to some serial order"
                    .to_string(),
            ]
        }

        TransactionSerializabilityProof::ConflictGraph {
            root,
            key,
            closure,
            dependencies,
        } => {
            let mut assumptions = vec![format!(
                "the conflict closure of {root} under SerializableBy({}) is {{{}}}: every \
                 transaction that may touch overlapping state, transitively",
                value_ref_label(key),
                closure_label(closure)
            )];

            let mut cited: Vec<String> = Vec::new();
            let mut unconstrained = 0usize;

            for dependency in dependencies {
                if dependency.evidence == CommitOrderEvidence::None {
                    unconstrained += 1;

                    continue;
                }

                let sentence =
                    verification::transaction_serializability::evidence_sentence(dependency);

                if !cited.contains(&sentence) {
                    cited.push(sentence);
                }
            }

            assumptions.extend(cited);

            if unconstrained > 0 {
                assumptions.push(format!(
                    "{unconstrained} potential {} on no cyclic conflict component and need no \
                     commit-order evidence",
                    if unconstrained == 1 {
                        "dependency lies"
                    } else {
                        "dependencies lie"
                    }
                ));
            }

            assumptions.push(
                "no cyclic conflict component contains an unconstrained dependency: an \
                 apparent cycle would imply a cycle in strict commit order and cannot occur \
                 in a committed history"
                    .to_string(),
            );

            assumptions
        }
    }
}

fn transaction_ordering_assumptions(proof: &TransactionOrderingProof) -> Vec<String> {
    let mut assumptions = vec!["transaction state history is serializable:".to_string()];

    assumptions.extend(
        transaction_serializability_assumptions(proof.serializability())
            .into_iter()
            .map(|assumption| format!("  {assumption}")),
    );

    match proof {
        TransactionOrderingProof::Cursor {
            key,
            position,
            cursor,
            rule,
            step,
            ..
        } => {
            assumptions.push(format!(
                "the ordering key {} identifies the cursor domain: every identity field \
                 of {} is pinned by it or by a literal",
                value_ref_label(key),
                cursor.object
            ));

            assumptions.push(format!(
                "the logical position {} is persisted as cursor {cursor} under the {rule} \
                 rule at step {}: the transaction commits only when the position is \
                 admissible after the stored one, so an older accepted position cannot \
                 commit after a newer one",
                value_ref_label(position),
                step + 1
            ));

            if *rule == CursorAdvanceRule::Successor {
                assumptions.push(
                    "the successor rule additionally makes accepted progression gap-free: \
                     a stale, duplicate, or skipped position rejects"
                        .to_string(),
                );
            }

            assumptions.push(format!(
                "no ordinary write touches {cursor}, and every advance of it uses the {rule} \
                 rule"
            ));
        }

        TransactionOrderingProof::Fence {
            key,
            position,
            fence,
            step,
            ..
        } => {
            assumptions.push(format!(
                "the ordering key {} identifies the fence domain: every identity field of \
                 {} is pinned by it or by a literal",
                value_ref_label(key),
                fence.object
            ));

            assumptions.push(format!(
                "the logical position {} is the fencing token of {fence} at step {}: a \
                 token older than the accepted fence rejects, so a lower generation \
                 cannot commit after a higher one has been accepted; equal tokens \
                 establish no relative order",
                value_ref_label(position),
                step + 1
            ));

            assumptions.push(format!("no ordinary write touches {fence}"));
        }
    }

    assumptions
}

/// The outbox admissions a transaction commits, rendered beside its
/// proof: they are part of the same ordered commit, and nothing here
/// claims anything about their later consumption.
fn commit_artifact_assumptions(artifacts: &[CommitArtifact]) -> Vec<String> {
    let mut assumptions = Vec::new();

    for artifact in artifacts {
        if let CommitArtifact::OutboxWrite {
            effect,
            outbox,
            schema,
            transition,
        } = artifact
        {
            assumptions.push(match transition {
                Some(transition) => format!(
                    "transition {transition} atomically admits {schema} to {outbox} through \
                     {effect}: the admission is part of the same ordered transaction \
                     commit, and no claim is made about later consumption order"
                ),
                None => format!(
                    "{effect} admits {schema} to {outbox} atomically with the commit; no \
                     claim is made about later consumption order"
                ),
            });
        }
    }

    assumptions
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
                        EffectSafety::ExternallyIdempotent { identity_key } => {
                            assumptions.push(format!(
                                "{prefix}duplicate applications of {} within one \
                                 interaction identity ({}) are declared externally \
                                 indistinguishable from a single application",
                                effect.effect,
                                root_labels(identity_key)
                            ))
                        }

                        EffectSafety::ExternallySideEffectFree => assumptions.push(format!(
                            "{prefix}the external boundary of {} is declared \
                             side-effect-free: any application causes no modeled \
                             externally observable state change",
                            effect.effect
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

                        EffectSafety::TransactionDeduplicated { transaction, key } => assumptions
                            .push(format!(
                                "{prefix}{} commits atomically with {transaction}, whose \
                                 commits are deduplicated by {}, stable across the \
                                 attempt class: at most one committed occurrence of the \
                                 outbox write exists",
                                effect.effect,
                                root_labels(key)
                            )),

                        EffectSafety::SameLogicalOutboxMessage {
                            outbox,
                            schema,
                            instance,
                            consumers,
                        } => {
                            assumptions.push(format!(
                                "{prefix}duplicate committed executions of {} admit the \
                                 same logical message under {outbox}'s message identity \
                                 ({})",
                                effect.effect,
                                instance_label(instance)
                            ));

                            if consumers.is_empty() {
                                assumptions.push(format!(
                                    "{prefix}no modeled outbox input on {outbox} admits \
                                     {schema}; the cascade ends at the outbox"
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

        ResultReplayProof::ClassFixedResult { returns, retryable } => {
            let mut assumptions = Vec::new();

            let path_count = returns.len() + retryable.len();

            for returned in returns {
                let prefix = path_prefix(path_count, &returned.path);

                assumptions.extend(decision_assumptions(&prefix, &returned.decisions));

                assumptions.push(format!(
                    "{prefix}the returned {} payload is derived deterministically from \
                     {}",
                    returned.arm,
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

            for exempt in retryable {
                let prefix = path_prefix(path_count, &exempt.path);

                assumptions.push(format!(
                    "{prefix}the returned error class {} is declared retryable: a \
                     nonterminal outcome by contract, so a later attempt may legitimately \
                     observe another result",
                    exempt.error
                ));
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

                RetryDriver::IntrinsicOutboxRedrive { input, outbox } => format!(
                    "outbox {outbox} intrinsically re-drives {input}: a committed \
                     message stays pending, admitting consumption attempts, until \
                     one succeeds"
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

/// One decision, named for evidence: the site, not the arm.
fn decision_label(decision: &verification::DecisionTaken) -> String {
    match decision {
        verification::DecisionTaken::Match { result, .. } => {
            format!("the match on {result}")
        }

        verification::DecisionTaken::Branch { location, .. } => {
            format!("the branch at step {location}")
        }

        verification::DecisionTaken::Transaction { transaction, .. } => {
            format!("the outcome of {transaction}")
        }
    }
}

/// The facts fixing each decision of a path: one line per decision.
fn decision_assumptions(prefix: &str, decisions: &[DecisionReplay]) -> Vec<String> {
    decisions
        .iter()
        .filter(|decision| {
            // The inert-continuation and outcome-divergence admissions
            // are derived structural facts, rendered as obligation
            // evidence — an assumption line would misfile them as
            // something an implementation must provide, and the arm
            // may legitimately differ per attempt.
            !matches!(
                decision.rule,
                DecisionRule::IdempotencyInertContinuation
                    | DecisionRule::OutcomeDivergenceAddsNoWork { .. }
            )
        })
        .map(|decision| {
            let taken = match &decision.decision {
                verification::DecisionTaken::Match { result, arm, .. } => {
                    format!("the match on {result} takes its {arm} arm on every attempt")
                }

                verification::DecisionTaken::Branch { location, arm } => {
                    format!("the branch at step {location} takes its {arm} arm on every attempt")
                }

                verification::DecisionTaken::Transaction {
                    transaction,
                    outcome,
                    ..
                } => format!("{transaction} is {outcome} on every attempt"),
            };

            let because = match &decision.rule {
                DecisionRule::StableResult { effect, rule, .. } => match rule {
                    ResultStabilityRule::ReplayConsistentTarget {
                        operation, input, ..
                    } => format!(
                        "{effect} sends a class-fixed request into {operation} via {input}, \
                         whose result replay is proven"
                    ),

                    ResultStabilityRule::ExternalTerminalResult { arm, .. } => format!(
                        "{effect} declares its terminal result replay-stable over a \
                         class-fixed interaction identity, and the observed {arm} is \
                         terminal"
                    ),
                },

                DecisionRule::StableCondition { roots } => {
                    format!("the condition is deterministic over {}", root_labels(roots))
                }

                DecisionRule::ResolvedCommit { transaction, key } => format!(
                    "{transaction} commits are deduplicated by {}, so every attempt \
                     resolves the one commit",
                    root_labels(key)
                ),

                DecisionRule::IdempotencyInertContinuation
                | DecisionRule::OutcomeDivergenceAddsNoWork { .. } => {
                    unreachable!("filtered above")
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

        verification::StabilityRule::ReplayStableExternalResult {
            result,
            effect,
            arm,
        } => Some(format!(
            "the {arm} of result {result} is observed equally by every attempt: \
             {effect} declares its terminal result replay-stable over a class-fixed \
             interaction identity"
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
