//! Verification of operation idempotency requirements (§9 of the
//! semantics contract).
//!
//! > Repeated attempts representing the same logical invocation must
//! > not cause externally distinguishable duplicate logical work
//! > beyond what the declared idempotency contract permits.
//!
//! The analysis composes the replay engine along each admitted path of
//! the operation program — a path ending at `complete` or at a
//! `return` for the triggering input — under the governing key's
//! population (§12):
//!
//! - **State leg**: every transaction step must be retry-safe — a
//!   keyed commit over a stable key commits once per class, and a
//!   naturally replayable body reproduces the same logical state on
//!   re-execution. There is no final-step exemption: a duplicate
//!   delivery re-drives the whole program even after terminal
//!   completion, so every committed transaction may be
//!   re-encountered.
//! - **Effect leg**: duplicate execution is possible at every effect
//!   site (§14 — even a recovered intent may re-execute when a crash
//!   hides a prior success), so each site must be duplicate-safe:
//!   an external effect through its declared `deduplicated_by` over a
//!   stable key; a request by targeting an input whose operation
//!   carries a proven idempotency requirement keyed from that input,
//!   fed by a class-fixed instance; a publication by being the *same
//!   logical message* — a class-fixed instance published to a topic
//!   whose message identity maps the schema — **and** by every modeled
//!   consumer of that message collapsing duplicate deliveries of it:
//!   a proven idempotency requirement keyed from the subscription, or
//!   `at_most_once` delivery of the one logical message.
//! - **Control leg**: every decision on the path must replay (§16) — a
//!   matched result replay-stable, or a branch condition deterministic
//!   over stable roots — so a retry traverses the same path. When a
//!   controlling observation may differ, the retry may do different
//!   work, and V1 has no compatibility argument for the two histories.
//!
//! The request and publication legs follow the trigger graph
//! (`trigger`): duplicate work an attempt causes downstream is still
//! work it caused, so a requirement is proven only when the cascade
//! it starts collapses everywhere the model can see. That makes
//! verdicts mutually dependent, so `check` computes a greatest
//! fixpoint: every requirement with an admissible governing key is
//! assumed, and whatever fails under that assumption is dropped until
//! nothing more fails. A cycle through request targets or message
//! consumers whose members each pass their local checks is therefore
//! proven, and the proof is marked coinductive
//! (§9).
//!
//! Result consistency is the separate result-replay obligation and is
//! not re-checked here; its verdicts feed in only where a decision
//! rests on a request effect's result. Vacuous routes: an empty
//! population, no admitted path (no modeled work to duplicate —
//! recoverability treats the same shape as an obstacle, and the
//! asymmetry is deliberate: progress is impossible, safety is
//! trivial), and single delivery (`at_most_once` with the payload
//! identity-pinned: same-class messages are one logical message
//! delivered at most once, so a class holds at most one attempt).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{
    DeliverySemantics, ExternalIdempotency, ExternalIdentity, FieldPath, Id, IdempotencyKey,
    Input, MessageIdentity, MessageIdentityKey, MessageSelector, Model, Operation, ValueSource,
};

use super::ProofScope;
use super::describe::{
    decision_gap_sentence, describe_decision, describe_path, gap_sentences, governing_key_evidence,
    unstable_roots,
};
use super::paths::{DecisionTaken, Path, PathRef, PathStep, paths};
use super::replay::{
    DecisionGap, DecisionReplay, DecisionRule, EffectSite, GoverningKeyDefect, InstanceGap,
    InstanceStability, PathContext, ReplayAnalysis, ReplayGap, StableRoot, TracedStep,
    UnstableRoot,
};
use super::trigger::{EffectContract, ProducerSite, TriggerGraph, collapses_duplicates, key_input};

/// The verdict for one declared idempotency requirement's side-effect
/// obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyCheck {
    pub operation: Id,

    /// Index into `operation.requirements.idempotency`.
    pub requirement: usize,

    /// The governing key, copied so the check is self-contained.
    pub key: IdempotencyKey,

    pub verdict: IdempotencyVerdict,

    /// Set when the proof holds only together with the proofs of the
    /// requirements it reaches through request targets or message
    /// consumers, and theirs hold only with it: the greatest fixpoint
    /// admits such a cycle by the minimal-counterexample argument of
    /// the minimal-counterexample argument of §9.
    #[serde(default)]
    pub coinductive: bool,

    /// For a governing key whose population is a subscription's
    /// messages on a topic with a keyed identity: per admitted schema
    /// and modeled producer, whether a declared propagation (§12)
    /// carries a key onto the identity fields the population rests on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<IdentityLineage>,
}

/// How a message's declared identity on its topic or outbox relates
/// to the key of the declaration that produces it — the propagation
/// lineage of §12, read from the consumer's side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityLineage {
    pub source: LineageSource,
    pub schema: Id,
    pub producer: ProducerRef,
    pub fact: LineageFact,
}

/// The message boundary whose declared identity anchors the lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineageSource {
    Topic { topic: Id },
    Outbox { outbox: Id },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProducerRef {
    Operation {
        operation: Id,
        effect: Id,
    },
    Transition {
        machine: Id,
        transition: Id,
        effect: Id,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineageFact {
    /// The producer declares a propagation whose targets cover the
    /// identity fields, so the identity carries `source`; when the
    /// source is the key of one of the producing operation's own
    /// idempotency requirements, `requirement` names it, and distinct
    /// logical invocations of the producer publish distinct messages.
    Propagated {
        source: IdempotencyKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requirement: Option<usize>,
    },

    /// No declared propagation carries a key onto the identity fields:
    /// the identity rests on the topic declaration alone.
    Undeclared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdempotencyVerdict {
    Proven {
        proof: IdempotencyProof,
        scope: ProofScope,
    },
    Unproven {
        obstacles: Vec<IdempotencyObstacle>,
    },
}

impl IdempotencyVerdict {
    fn proven(proof: IdempotencyProof) -> Self {
        Self::Proven {
            scope: proof.scope(),
            proof,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdempotencyProof {
    /// The triggering subscription admits no message schemas, so no
    /// attempt can bear the key.
    NoAdmittedInvocations { input: Id },

    /// No admitted path exists for the triggering input: an attempt
    /// performs no modeled work, so nothing can be duplicated.
    NoAdmittedPaths { input: Id },

    /// `at_most_once` delivery with the payload identity-pinned:
    /// same-class messages are one logical message, delivered no more
    /// than once, so repeated attempts cannot exist.
    SingleDelivery { input: Id, topic: Id },

    /// The outbox counterpart of `SingleDelivery`: `at_most_once`
    /// outbox delivery with the payload pinned by the outbox's keyed
    /// message identity.
    SingleOutboxDelivery { input: Id, outbox: Id },

    /// Every admitted path is retry-safe in all three legs.
    RetrySafePaths { paths: Vec<PathRetrySafety> },
}

impl IdempotencyProof {
    /// The layers this proof consumed *directly*, before propagation
    /// through the requirements it leans on.
    pub fn direct_scope(&self) -> ProofScope {
        match self {
            Self::NoAdmittedInvocations { .. } | Self::NoAdmittedPaths { .. } => {
                ProofScope::L0Only
            }

            // `at_most_once` is a delivery fact, and delivery is L1.
            Self::SingleDelivery { .. } | Self::SingleOutboxDelivery { .. } => {
                ProofScope::RuntimeDependent
            }

            Self::RetrySafePaths { paths } => ProofScope::joined(
                paths
                    .iter()
                    .flat_map(|path| path.effects.iter())
                    .map(|effect| effect.safety.direct_scope()),
            ),
        }
    }

    /// The scope this proof carries when nothing it leans on is
    /// runtime-dependent. Propagation refines it in [`check`].
    pub fn scope(&self) -> ProofScope {
        self.direct_scope()
    }

    /// The idempotency requirements of *other* boundaries this proof
    /// rests on, whose own scope propagates into this one.
    ///
    /// A duplicate collapsed by another operation's proven requirement
    /// is only as L0 as that requirement's proof: if the callee proved
    /// its idempotency from runtime topology, so did this caller.
    pub fn dependencies(&self) -> Vec<(Id, Id)> {
        let Self::RetrySafePaths { paths } = self else {
            return Vec::new();
        };

        paths
            .iter()
            .flat_map(|path| path.effects.iter())
            .flat_map(|effect| effect.safety.dependencies())
            .collect()
    }
}

impl EffectSafety {
    fn direct_scope(&self) -> ProofScope {
        let consumer_scopes = |consumers: &[ConsumerCollapse]| {
            ProofScope::joined(consumers.iter().map(|consumer| match consumer {
                // The consumer never sees a second delivery — a fact
                // about the realized transport.
                ConsumerCollapse::SingleDelivery { .. } => ProofScope::RuntimeDependent,
                ConsumerCollapse::ProvenRequirement { .. } => ProofScope::L0Only,
            }))
        };

        match self {
            Self::ExternallyIdempotent { .. }
            | Self::ExternallySideEffectFree
            | Self::DeduplicatedByTarget { .. }
            // Transaction commit deduplication is an L0 fact.
            | Self::TransactionDeduplicated { .. } => ProofScope::L0Only,

            Self::SameLogicalMessage { consumers, .. }
            | Self::SameLogicalOutboxMessage { consumers, .. } => consumer_scopes(consumers),
        }
    }

    fn dependencies(&self) -> Vec<(Id, Id)> {
        match self {
            Self::ExternallyIdempotent { .. }
            | Self::ExternallySideEffectFree
            | Self::TransactionDeduplicated { .. } => Vec::new(),

            Self::DeduplicatedByTarget {
                operation, input, ..
            } => vec![(operation.clone(), input.clone())],

            Self::SameLogicalMessage { consumers, .. }
            | Self::SameLogicalOutboxMessage { consumers, .. } => consumers
                .iter()
                .filter_map(|consumer| match consumer {
                    ConsumerCollapse::ProvenRequirement { operation, input } => {
                        Some((operation.clone(), input.clone()))
                    }
                    ConsumerCollapse::SingleDelivery { .. } => None,
                })
                .collect(),
        }
    }
}

/// The retry-safety argument for one admitted path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRetrySafety {
    pub path: PathRef,

    /// Why a retry takes the same arm at every decision on the path.
    pub decisions: Vec<DecisionReplay>,

    /// Per transaction step, in path order.
    pub transactions: Vec<TransactionRetrySafety>,

    /// Per effect-executing step, in path order.
    pub effects: Vec<EffectRetrySafety>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionRetrySafety {
    pub transaction: Id,
    pub route: RetryRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryRoute {
    /// At most one commit per class; re-encounters resolve it.
    KeyedCommit { key: Vec<StableRoot> },

    /// Re-execution reproduces the same logical state.
    NaturalReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectRetrySafety {
    /// The executed effect (for intent executions, the intent's
    /// effect; the intent itself is recorded in `safety`).
    pub effect: Id,

    pub safety: EffectSafety,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectSafety {
    /// The external boundary declares duplicate applications of one
    /// keyed interaction externally indistinguishable from a single
    /// application, and the interaction-identity key is stable across
    /// the class.
    ExternallyIdempotent { identity_key: Vec<StableRoot> },

    /// The external boundary declares itself side-effect-free: any
    /// application causes no modeled externally observable state
    /// change, so duplicates are harmless with no key condition.
    ExternallySideEffectFree,

    /// Every attempt publishes the same logical message — the instance
    /// is class-fixed and the topic's message identity maps the
    /// schema — and every modeled consumer of it collapses duplicate
    /// deliveries, so the cascade the publication starts performs the
    /// work of one logical invocation everywhere the model can see.
    SameLogicalMessage {
        topic: Id,
        schema: Id,
        instance: InstanceStability,
        consumers: Vec<ConsumerCollapse>,
    },

    /// Every attempt sends payload-equal requests into one class of
    /// the target's proven idempotency requirement.
    DeduplicatedByTarget {
        operation: Id,
        input: Id,
        instance: InstanceStability,
    },

    /// The producer-suppressed route for a transactional outbox write
    /// (§50 of the outbox revision): the containing transaction
    /// deduplicates its commit by a class-stable key, so at most one
    /// logical commit — and therefore at most one committed occurrence
    /// of the contained write — exists per class. No class-fixity of
    /// the message instance is needed: the single commit admitted
    /// whatever it admitted, once.
    TransactionDeduplicated {
        transaction: Id,
        key: Vec<StableRoot>,
    },

    /// The message/consumer route for a transactional outbox write
    /// (§51): every attempt admits the same logical outbox message —
    /// the instance is class-fixed and the outbox's message identity
    /// maps the schema — and every modeled consumer of it collapses
    /// duplicate deliveries, exactly as duplicate topic publication is
    /// discharged.
    SameLogicalOutboxMessage {
        outbox: Id,
        schema: Id,
        instance: InstanceStability,
        consumers: Vec<ConsumerCollapse>,
    },
}

/// How a modeled consumer of a published message collapses duplicate
/// deliveries of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsumerCollapse {
    /// The consumer's idempotency requirement keyed from the
    /// subscription is proven, so payload-equal deliveries fall into
    /// one of its classes.
    ProvenRequirement { operation: Id, input: Id },

    /// The subscription's `at_most_once` delivery bounds one logical
    /// message to at most one delivery, however often it is published.
    SingleDelivery { operation: Id, input: Id },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdempotencyObstacle {
    /// The governing key defines no pre-execution equivalence class
    /// (§12).
    GoverningKeyInadmissible { defect: GoverningKeyDefect },

    /// A decision on the path is not established to replay, so a retry
    /// may take a different path and do different work.
    PathDecisionUnstable {
        path: PathRef,
        decision: DecisionTaken,
        gap: DecisionGap,
    },

    /// A committed transaction may be re-encountered, and neither
    /// retry route holds.
    TransactionNotRetrySafe {
        path: PathRef,
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },

    /// The external boundary declares `distinguishable`: a duplicate
    /// application may produce distinguishable modeled external work
    /// (§13.3).
    ExternalApplicationsDistinguishable { path: PathRef, effect: Id },

    /// No duplicate-side-effect fact is declared for the external
    /// boundary.
    ExternalIdempotencyUnknown { path: PathRef, effect: Id },

    /// The declared external interaction-identity key is not
    /// replay-stable, so attempts may address different logical
    /// interactions.
    ExternalIdentityKeyUnstable {
        path: PathRef,
        effect: Id,
        roots: Vec<UnstableRoot>,
    },

    /// Duplicate publications are not established to be the same
    /// logical message: the topic declares no message identity for
    /// the published schema.
    PublicationNotIdentified {
        path: PathRef,
        effect: Id,
        topic: Id,
        schema: Id,
    },

    /// A modeled consumer of the published message declares no
    /// idempotency requirement keyed from its subscription, so nothing
    /// collapses the duplicate work a duplicate delivery causes there.
    PublicationConsumerNotKeyed {
        path: PathRef,
        effect: Id,
        topic: Id,
        schema: Id,
        operation: Id,
        input: Id,
    },

    /// The consumer declares such a requirement, but it is not proven
    /// in this analysis. Under the greatest fixpoint a requirement is
    /// dropped only by failing its own checks, so a cycle is not by
    /// itself a cause.
    PublicationConsumerRequirementUnproven {
        path: PathRef,
        effect: Id,
        topic: Id,
        schema: Id,
        operation: Id,
        input: Id,
    },

    /// Neither outbox-write route holds on the identity leg: the
    /// containing transaction is not established to suppress a second
    /// commit, and the destination outbox declares no message identity
    /// for the written schema, so repeated committed writes are not
    /// established to be the same logical message.
    OutboxWriteNotIdentified {
        path: PathRef,
        effect: Id,
        transaction: Id,
        outbox: Id,
        schema: Id,
    },

    /// A modeled consumer of the written outbox message declares no
    /// idempotency requirement keyed from its outbox input, so nothing
    /// collapses the duplicate work a duplicate delivery causes there.
    OutboxConsumerNotKeyed {
        path: PathRef,
        effect: Id,
        outbox: Id,
        schema: Id,
        operation: Id,
        input: Id,
    },

    /// The outbox consumer declares such a requirement, but it is not
    /// proven in this analysis.
    OutboxConsumerRequirementUnproven {
        path: PathRef,
        effect: Id,
        outbox: Id,
        schema: Id,
        operation: Id,
        input: Id,
    },

    /// A direct execution declares no instance provenance, so the
    /// instances attempts construct are not class-fixed.
    EffectInstanceUnspecified { path: PathRef, effect: Id },

    /// A direct execution's instance derivation depends on unstable
    /// roots.
    EffectInstanceRootUnstable {
        path: PathRef,
        effect: Id,
        roots: Vec<UnstableRoot>,
    },

    /// The executed intent is established by no earlier step, so no
    /// class-fixed instance exists to argue about.
    IntentNotEstablished { path: PathRef, intent: Id },

    /// The executed intent is replay-available through neither route,
    /// so a retry's instance may differ.
    IntentNotReplayAvailable {
        path: PathRef,
        intent: Id,
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },

    /// The request effect's schema is not the targeted input's
    /// schema, so payload equality does not transfer.
    RequestSchemaMismatch {
        path: PathRef,
        effect: Id,
        expected: Id,
        actual: Id,
    },

    /// The target operation declares no idempotency requirement keyed
    /// from the targeted input, so nothing collapses duplicate
    /// invocations.
    RequestTargetHasNoKeyedRequirement {
        path: PathRef,
        effect: Id,
        operation: Id,
        input: Id,
    },

    /// The target declares such a requirement, but it is not proven
    /// in this analysis. Under the greatest fixpoint a requirement is
    /// dropped only by failing its own checks, so a cycle is not by
    /// itself a cause.
    RequestTargetRequirementUnproven {
        path: PathRef,
        effect: Id,
        operation: Id,
        input: Id,
    },
}

/// What a check reads beyond the operation under analysis: the model,
/// its trigger graph, the requirements proven so far in the fixpoint,
/// keyed by operation and triggering input, and the result-replay
/// verdicts decisions may rest on.
struct Scope<'a> {
    model: &'a Model,
    graph: &'a TriggerGraph<'a>,
    proven: &'a BTreeSet<(Id, Id)>,
    consistent: &'a BTreeSet<(Id, Id)>,
}

/// Checks every idempotency requirement declared by the model, as a
/// fixpoint over cross-operation discharge through request targets and
/// message consumers. `consistent` names the `(operation, input)`
/// pairs whose result replay is proven, which a decision on a request
/// effect's result rests on.
///
/// The verdicts are the greatest fixpoint: every requirement with an
/// admissible governing key is assumed, and whatever fails under that
/// assumption is dropped until nothing more fails, so a cycle of
/// requirements that each collapse the others' duplicates proves
/// (§9). The least fixpoint is
/// computed alongside to mark which proofs rest on such a cycle.
pub fn check(model: &Model, consistent: &BTreeSet<(Id, Id)>) -> Vec<IdempotencyCheck> {
    let graph = TriggerGraph::new(model);

    let least = proven_set(&fixpoint(model, &graph, consistent, BTreeSet::new()));

    let every: BTreeSet<(Id, Id)> = model
        .operations
        .iter()
        .flat_map(|(operation, declaration)| {
            declaration
                .requirements
                .idempotency
                .iter()
                .filter_map(move |requirement| {
                    Some((operation.clone(), key_input(&requirement.key)?.clone()))
                })
        })
        .collect();

    let mut checks = fixpoint(model, &graph, consistent, every);

    for check in &mut checks {
        if matches!(check.verdict, IdempotencyVerdict::Proven { .. })
            && let Some(input) = key_input(&check.key)
            && !least.contains(&(check.operation.clone(), input.clone()))
        {
            check.coinductive = true;
        }
    }

    propagate_scopes(&mut checks);

    checks
}

/// Widens proof scopes until they are stable.
///
/// A proof that collapses duplicates through another boundary's proven
/// idempotency requirement inherits that requirement's dependence on
/// the runtime realization: if the callee proved its idempotency from
/// declared topology, the caller's proof is runtime-dependent too,
/// however L0 its own facts look. Scopes only ever widen from `L0Only`
/// to `RuntimeDependent`, so the iteration is monotone and terminates.
fn propagate_scopes(checks: &mut [IdempotencyCheck]) {
    loop {
        let runtime_dependent: BTreeSet<(Id, Id)> = checks
            .iter()
            .filter_map(|check| match &check.verdict {
                IdempotencyVerdict::Proven { scope, .. }
                    if *scope == ProofScope::RuntimeDependent =>
                {
                    Some((check.operation.clone(), key_input(&check.key)?.clone()))
                }

                _ => None,
            })
            .collect();

        let mut widened = false;

        for check in checks.iter_mut() {
            let IdempotencyVerdict::Proven { proof, scope } = &mut check.verdict else {
                continue;
            };

            if *scope == ProofScope::RuntimeDependent {
                continue;
            }

            if proof
                .dependencies()
                .iter()
                .any(|dependency| runtime_dependent.contains(dependency))
            {
                *scope = ProofScope::RuntimeDependent;
                widened = true;
            }
        }

        if !widened {
            return;
        }
    }
}

/// Iterates `run` from `assumed` until the proven set is stable. From
/// the empty set the chain ascends to the least fixpoint; from every
/// admissible requirement it descends to the greatest, since fewer
/// assumptions never prove more.
fn fixpoint(
    model: &Model,
    graph: &TriggerGraph<'_>,
    consistent: &BTreeSet<(Id, Id)>,
    mut assumed: BTreeSet<(Id, Id)>,
) -> Vec<IdempotencyCheck> {
    loop {
        let checks = run(&Scope {
            model,
            graph,
            proven: &assumed,
            consistent,
        });

        let next = proven_set(&checks);

        if next == assumed {
            return checks;
        }

        assumed = next;
    }
}

fn proven_set(checks: &[IdempotencyCheck]) -> BTreeSet<(Id, Id)> {
    checks
        .iter()
        .filter(|check| matches!(check.verdict, IdempotencyVerdict::Proven { .. }))
        .filter_map(|check| Some((check.operation.clone(), key_input(&check.key)?.clone())))
        .collect()
}

/// The propagation lineage behind a message-triggered governing key:
/// for every admitted schema on the input's topic or outbox and every
/// modeled producer of it, whether a declared propagation carries a
/// key onto the identity fields the population rests on. An outbox
/// boundary continues the same lineage a topic does — no
/// outbox-specific key type exists.
fn lineage(scope: &Scope<'_>, operation: &Operation, key: &IdempotencyKey) -> Vec<IdentityLineage> {
    let Some(input_id) = key_input(key) else {
        return Vec::new();
    };

    match operation.inputs.get(input_id) {
        Some(Input::Subscription(subscription)) => {
            subscription_lineage(scope, subscription)
        }

        Some(Input::Outbox(outbox_input)) => outbox_lineage(scope, outbox_input),

        _ => Vec::new(),
    }
}

/// The identity fields the population rests on, per admitted schema.
fn identified_schemas<'a>(
    mapping: &'a std::collections::BTreeMap<Id, Vec<FieldPath>>,
    admitted: Vec<&'a Id>,
) -> Vec<(&'a Id, &'a Vec<FieldPath>)> {
    admitted
        .into_iter()
        .filter_map(|schema| mapping.get(schema).map(|identity| (schema, identity)))
        .collect()
}

/// Whether one of the producer's declared propagations covers the
/// identity fields at its own effect site, and from which source key.
fn propagation_fact(
    scope: &Scope<'_>,
    producer_operation: Option<&Id>,
    effect: &Id,
    propagations: &[crate::spec::IdempotencyKeyPropagation],
    identity: &[FieldPath],
) -> LineageFact {
    propagations
        .iter()
        .find_map(|propagation| {
            let targets: Vec<&FieldPath> = propagation
                .target
                .components
                .iter()
                .filter(|component| component.source == ValueSource::Effect(effect.clone()))
                .map(|component| &component.path)
                .collect();

            identity
                .iter()
                .all(|field| targets.contains(&field))
                .then(|| {
                    let requirement = producer_operation.and_then(|operation| {
                        scope.model.operations.get(operation).and_then(|declaration| {
                            declaration
                                .requirements
                                .idempotency
                                .iter()
                                .position(|requirement| requirement.key == propagation.source)
                        })
                    });

                    LineageFact::Propagated {
                        source: propagation.source.clone(),
                        requirement,
                    }
                })
        })
        .unwrap_or(LineageFact::Undeclared)
}

fn subscription_lineage(
    scope: &Scope<'_>,
    subscription: &crate::spec::SubscriptionInput,
) -> Vec<IdentityLineage> {
    let Some(topic) = scope.model.topics.get(&subscription.topic) else {
        return Vec::new();
    };

    let MessageIdentity::Keyed(MessageIdentityKey { mapping }) = &topic.message_identity else {
        return Vec::new();
    };

    let admitted: Vec<&Id> = match &subscription.messages {
        MessageSelector::All => topic.messages.iter().collect(),
        MessageSelector::Only(schemas) => schemas.iter().collect(),
    };

    let mut out = Vec::new();

    for (schema, identity) in identified_schemas(mapping, admitted) {
        for producer in scope.graph.producers(&subscription.topic, schema) {
            let producer_operation = match producer.site {
                ProducerSite::Operation { operation } => Some(operation),
                ProducerSite::Transition { .. } => None,
            };

            let fact = propagation_fact(
                scope,
                producer_operation,
                producer.effect,
                &producer.publication.idempotency_key_propagation,
                identity,
            );

            out.push(IdentityLineage {
                source: LineageSource::Topic {
                    topic: subscription.topic.clone(),
                },
                schema: schema.clone(),
                producer: match producer.site {
                    ProducerSite::Operation { operation } => ProducerRef::Operation {
                        operation: operation.clone(),
                        effect: producer.effect.clone(),
                    },

                    ProducerSite::Transition {
                        machine,
                        transition,
                    } => ProducerRef::Transition {
                        machine: machine.clone(),
                        transition: transition.clone(),
                        effect: producer.effect.clone(),
                    },
                },
                fact,
            });
        }
    }

    out
}

fn outbox_lineage(
    scope: &Scope<'_>,
    input: &crate::spec::OutboxInput,
) -> Vec<IdentityLineage> {
    let Some((_, outbox)) = scope.model.outbox(&input.outbox) else {
        return Vec::new();
    };

    let MessageIdentity::Keyed(MessageIdentityKey { mapping }) = &outbox.message_identity else {
        return Vec::new();
    };

    let admitted: Vec<&Id> = match &input.messages {
        MessageSelector::All => outbox.messages.iter().collect(),
        MessageSelector::Only(schemas) => schemas.iter().collect(),
    };

    let mut out = Vec::new();

    for (schema, identity) in identified_schemas(mapping, admitted) {
        for producer in scope.graph.outbox_producers(&input.outbox, schema) {
            let fact = propagation_fact(
                scope,
                Some(producer.operation),
                producer.effect,
                &producer.write.idempotency_key_propagation,
                identity,
            );

            out.push(IdentityLineage {
                source: LineageSource::Outbox {
                    outbox: input.outbox.clone(),
                },
                schema: schema.clone(),
                producer: ProducerRef::Operation {
                    operation: producer.operation.clone(),
                    effect: producer.effect.clone(),
                },
                fact,
            });
        }
    }

    out
}

fn run(scope: &Scope<'_>) -> Vec<IdempotencyCheck> {
    let mut checks = Vec::new();

    for (operation_id, operation) in &scope.model.operations {
        for (index, requirement) in operation.requirements.idempotency.iter().enumerate() {
            checks.push(IdempotencyCheck {
                operation: operation_id.clone(),
                requirement: index,
                key: requirement.key.clone(),
                verdict: check_requirement(scope, operation_id, operation, &requirement.key),
                coinductive: false,
                lineage: lineage(scope, operation, &requirement.key),
            });
        }
    }

    checks
}

fn check_requirement(
    scope: &Scope<'_>,
    operation_id: &Id,
    operation: &Operation,
    key: &IdempotencyKey,
) -> IdempotencyVerdict {
    let analysis = match ReplayAnalysis::new(scope.model, operation, key, scope.consistent) {
        Ok(analysis) => analysis,

        Err(defect) => {
            return IdempotencyVerdict::Unproven {
                obstacles: vec![IdempotencyObstacle::GoverningKeyInadmissible { defect }],
            };
        }
    };

    if analysis.admits_no_attempts() {
        return IdempotencyVerdict::proven(IdempotencyProof::NoAdmittedInvocations {
                input: analysis.input().clone(),
            },
        );
    }

    // The single-delivery vacuous route: at most one attempt per
    // class can exist.
    if let Some(Input::Subscription(subscription)) = operation.inputs.get(analysis.input())
        && scope.model.delivery(operation_id, analysis.input()) == DeliverySemantics::AtMostOnce
        && analysis.payload_identified()
    {
        return IdempotencyVerdict::proven(IdempotencyProof::SingleDelivery {
                input: analysis.input().clone(),
                topic: subscription.topic.clone(),
            },
        );
    }

    // The same route for an outbox-triggered population, from the
    // outbox runtime's delivery fact and the outbox's keyed message
    // identity.
    if let Some(Input::Outbox(outbox_input)) = operation.inputs.get(analysis.input())
        && scope.model.outbox_delivery(operation_id, analysis.input())
            == DeliverySemantics::AtMostOnce
        && analysis.payload_identified()
    {
        return IdempotencyVerdict::proven(IdempotencyProof::SingleOutboxDelivery {
            input: analysis.input().clone(),
            outbox: outbox_input.outbox.clone(),
        });
    }

    let all = paths(&operation.program);

    let admitted: Vec<&Path<'_>> = all
        .iter()
        .filter(|path| path.admitted_for(analysis.input()))
        .collect();

    if admitted.is_empty() {
        return IdempotencyVerdict::proven(IdempotencyProof::NoAdmittedPaths {
                input: analysis.input().clone(),
            },
        );
    }

    let mut obstacles = Vec::new();
    let mut safe = Vec::new();

    let inert = idempotency_inert_decisions(&admitted);

    for path in admitted {
        if let Some(safety) = analyze_path(scope, &analysis, path, &inert, &mut obstacles) {
            safe.push(safety);
        }
    }

    if obstacles.is_empty() {
        IdempotencyVerdict::proven(IdempotencyProof::RetrySafePaths { paths: safe })
    } else {
        IdempotencyVerdict::Unproven {
            obstacles: dedupe(obstacles, IdempotencyObstacle::site),
        }
    }
}

/// Keeps the first obstacle per site: paths sharing a prefix reach its
/// steps with the same context and report the same facts about them.
pub(crate) fn dedupe<T, K: PartialEq>(obstacles: Vec<T>, site: impl Fn(&T) -> K) -> Vec<T> {
    let mut kept: Vec<T> = Vec::new();
    let mut sites: Vec<K> = Vec::new();

    for obstacle in obstacles {
        let key = site(&obstacle);

        if !sites.contains(&key) {
            sites.push(key);
            kept.push(obstacle);
        }
    }

    kept
}

impl IdempotencyObstacle {
    /// The obstacle with its path forgotten — and, for a decision, the
    /// arm — so the same fact on two paths compares equal.
    fn site(&self) -> Self {
        let mut site = self.clone();

        match &mut site {
            Self::GoverningKeyInadmissible { .. } => {}

            Self::PathDecisionUnstable { path, decision, .. } => {
                *path = PathRef::default();

                match decision {
                    DecisionTaken::Match { arm, .. } => *arm = crate::spec::ResultVariant::Ok,
                    DecisionTaken::Branch { arm, .. } => *arm = crate::spec::Arm::Then,
                }
            }

            Self::TransactionNotRetrySafe { path, .. }
            | Self::ExternalApplicationsDistinguishable { path, .. }
            | Self::ExternalIdempotencyUnknown { path, .. }
            | Self::ExternalIdentityKeyUnstable { path, .. }
            | Self::PublicationNotIdentified { path, .. }
            | Self::PublicationConsumerNotKeyed { path, .. }
            | Self::PublicationConsumerRequirementUnproven { path, .. }
            | Self::OutboxWriteNotIdentified { path, .. }
            | Self::OutboxConsumerNotKeyed { path, .. }
            | Self::OutboxConsumerRequirementUnproven { path, .. }
            | Self::EffectInstanceUnspecified { path, .. }
            | Self::EffectInstanceRootUnstable { path, .. }
            | Self::IntentNotEstablished { path, .. }
            | Self::IntentNotReplayAvailable { path, .. }
            | Self::RequestSchemaMismatch { path, .. }
            | Self::RequestTargetHasNoKeyedRequirement { path, .. }
            | Self::RequestTargetRequirementUnproven { path, .. } => *path = PathRef::default(),
        }

        site
    }
}

/// The §9 idempotency-inert continuation admission: the locations of
/// decisions after which, on every admitted path, only further
/// decisions occur before the terminal. Divergence at such a decision
/// cannot add modeled work — no transaction, no effect execution or
/// launch, no intent execution, no `join_all` or `race` follows on
/// any continuation — so a non-replaying decision there need not
/// block an idempotency proof: the class's complete modeled work is
/// the shared prefix's, and terminal divergence is the result-replay
/// obligation's separate concern.
///
/// The quantification is over complete continuations, never immediate
/// arm bodies: branch fall-through and enclosing-block suffixes are
/// already unrolled into the admitted paths, so "every step after the
/// decision on every path is a decision" is exactly the predicate. A
/// location effectful on any one continuation is disqualified on all.
fn idempotency_inert_decisions(paths: &[&Path<'_>]) -> BTreeSet<crate::spec::StepLocation> {
    let mut inert = BTreeSet::new();
    let mut disqualified = BTreeSet::new();

    for path in paths {
        for (index, step) in path.steps.iter().enumerate() {
            let PathStep::Decision { location, .. } = step else {
                continue;
            };

            let effectful_suffix = path.steps[index + 1..]
                .iter()
                .any(|later| !matches!(later, PathStep::Decision { .. }));

            if effectful_suffix {
                disqualified.insert(location.clone());
            } else {
                inert.insert(location.clone());
            }
        }
    }

    inert.retain(|location| !disqualified.contains(location));

    inert
}

fn analyze_path(
    scope: &Scope<'_>,
    analysis: &ReplayAnalysis<'_>,
    path: &Path<'_>,
    inert: &BTreeSet<crate::spec::StepLocation>,
    obstacles: &mut Vec<IdempotencyObstacle>,
) -> Option<PathRetrySafety> {
    let before = obstacles.len();

    let reference = path.reference();
    let trace = analysis.trace(path);

    let mut transactions = Vec::new();
    let mut effects = Vec::new();
    let mut decisions = Vec::new();

    // The keyed-commit judgment of each transaction on the path, for
    // the outbox writes that commit with it: the producer-suppressed
    // route (§50 of the outbox revision) is the transaction's route B.
    let mut commits: std::collections::BTreeMap<&Id, &Vec<StableRoot>> =
        std::collections::BTreeMap::new();

    for step in &trace.steps {
        match step {
            TracedStep::Transaction {
                transaction,
                recovery,
                natural,
                ..
            } => {
                if let Ok(key) = recovery {
                    commits.insert(&transaction.id, key);
                }

                match (recovery, natural) {
                    (Ok(key), _) => transactions.push(TransactionRetrySafety {
                        transaction: transaction.id.clone(),
                        route: RetryRoute::KeyedCommit { key: key.clone() },
                    }),

                    (Err(_), Ok(())) => transactions.push(TransactionRetrySafety {
                        transaction: transaction.id.clone(),
                        route: RetryRoute::NaturalReplay,
                    }),

                    (Err(recovery), Err(reconstruction)) => {
                        obstacles.push(IdempotencyObstacle::TransactionNotRetrySafe {
                            path: reference.clone(),
                            transaction: transaction.id.clone(),
                            recovery: recovery.clone(),
                            reconstruction: reconstruction.clone(),
                        });
                    }
                }
            }

            TracedStep::Effect {
                site,
                contract,
                before: context,
                instance,
                ..
            } => {
                let Some(contract) = contract else {
                    continue;
                };

                if let Some(safety) = contract_safety(
                    scope, analysis, context, &reference, site, contract, instance, &commits,
                    obstacles,
                ) {
                    effects.push(EffectRetrySafety {
                        effect: site.effect().clone(),
                        safety,
                    });
                }
            }

            TracedStep::Decision {
                location,
                taken,
                replay,
            } => match replay {
                Ok(rule) => decisions.push(DecisionReplay {
                    decision: taken.clone(),
                    rule: rule.clone(),
                }),

                // The idempotency-inert continuation admission (§9):
                // when nothing but decisions and terminals can follow
                // this decision on any admitted path, divergence adds
                // no modeled work — recorded as a derived structural
                // fact on the proof, never silently and never as an
                // implementation assumption.
                Err(_) if inert.contains(location) => decisions.push(DecisionReplay {
                    decision: taken.clone(),
                    rule: DecisionRule::IdempotencyInertContinuation,
                }),

                Err(gap) => {
                    obstacles.push(IdempotencyObstacle::PathDecisionUnstable {
                        path: reference.clone(),
                        decision: taken.clone(),
                        gap: gap.clone(),
                    });
                }
            },
        }
    }

    (obstacles.len() == before).then_some(PathRetrySafety {
        path: reference,
        decisions,
        transactions,
        effects,
    })
}

/// The obstacle a non-class-fixed instance is, at the site that needs
/// one.
fn instance_obstacle(
    path: &PathRef,
    site: &EffectSite<'_>,
    gap: &InstanceGap,
) -> IdempotencyObstacle {
    match gap {
        InstanceGap::DerivationUnspecified => IdempotencyObstacle::EffectInstanceUnspecified {
            path: path.clone(),
            effect: site.effect().clone(),
        },

        InstanceGap::RootsUnstable { roots } => IdempotencyObstacle::EffectInstanceRootUnstable {
            path: path.clone(),
            effect: site.effect().clone(),
            roots: roots.clone(),
        },

        InstanceGap::IntentNotEstablished { intent } => IdempotencyObstacle::IntentNotEstablished {
            path: path.clone(),
            intent: intent.clone(),
        },

        InstanceGap::IntentNotReplayAvailable {
            intent,
            transaction,
            recovery,
            reconstruction,
        } => IdempotencyObstacle::IntentNotReplayAvailable {
            path: path.clone(),
            intent: intent.clone(),
            transaction: transaction.clone(),
            recovery: recovery.clone(),
            reconstruction: reconstruction.clone(),
        },
    }
}

/// The per-kind duplicate-execution judgment. The instance is consulted
/// only for kinds whose discharge needs a class-fixed one; an external
/// boundary deduplicates by key alone, and a transactionally
/// suppressed outbox write commits at most once whatever its instance.
#[allow(clippy::too_many_arguments)]
fn contract_safety(
    scope: &Scope<'_>,
    analysis: &ReplayAnalysis<'_>,
    context: &PathContext,
    path: &PathRef,
    site: &EffectSite<'_>,
    contract: &EffectContract<'_>,
    instance: &Result<InstanceStability, InstanceGap>,
    commits: &std::collections::BTreeMap<&Id, &Vec<StableRoot>>,
    obstacles: &mut Vec<IdempotencyObstacle>,
) -> Option<EffectSafety> {
    let effect = site.effect();

    match contract {
        EffectContract::OutboxWrite(write) => {
            let EffectSite::OutboxWrite { transaction, .. } = site else {
                // An outbox write reaches the trace only from its one
                // legal site; any other shape is an invalid model,
                // judged conservatively as an unidentified duplicate
                // write below.
                obstacles.push(IdempotencyObstacle::OutboxWriteNotIdentified {
                    path: path.clone(),
                    effect: effect.clone(),
                    transaction: Id(String::new()),
                    outbox: write.outbox.clone(),
                    schema: write.schema.clone(),
                });

                return None;
            };

            // Route one — producer-suppressed (§50): the containing
            // transaction commits at most once per class, so at most
            // one committed occurrence of this write exists. The
            // transaction's key stability was judged where the
            // transaction was traced.
            if let Some(key) = commits.get(*transaction) {
                return Some(EffectSafety::TransactionDeduplicated {
                    transaction: (*transaction).clone(),
                    key: (*key).clone(),
                });
            }

            // Route two — message/consumer (§51), the publication
            // pattern: repeated committed writes must denote one
            // logical message, and every modeled consumer must
            // collapse the duplicate work an extra delivery causes.
            let outbox = &write.outbox;
            let schema = &write.schema;

            let identified = scope
                .model
                .outbox(outbox)
                .is_some_and(|(_, outbox)| match &outbox.message_identity {
                    MessageIdentity::Keyed(MessageIdentityKey { mapping }) => {
                        mapping.contains_key(schema)
                    }
                    MessageIdentity::Unspecified => false,
                });

            if !identified {
                obstacles.push(IdempotencyObstacle::OutboxWriteNotIdentified {
                    path: path.clone(),
                    effect: effect.clone(),
                    transaction: (*transaction).clone(),
                    outbox: outbox.clone(),
                    schema: schema.clone(),
                });
            }

            let mut consumers = Vec::new();
            let mut collapsed = true;

            for consumer in scope.graph.outbox_consumers(outbox, schema) {
                let operation = consumer.operation.clone();
                let input = consumer.input.clone();

                if identified
                    && scope
                        .model
                        .outbox_delivery(consumer.operation, consumer.input)
                        == DeliverySemantics::AtMostOnce
                {
                    consumers.push(ConsumerCollapse::SingleDelivery { operation, input });
                } else if !scope
                    .model
                    .operations
                    .get(consumer.operation)
                    .is_some_and(|target| collapses_duplicates(target, consumer.input))
                {
                    collapsed = false;

                    obstacles.push(IdempotencyObstacle::OutboxConsumerNotKeyed {
                        path: path.clone(),
                        effect: effect.clone(),
                        outbox: outbox.clone(),
                        schema: schema.clone(),
                        operation,
                        input,
                    });
                } else if !scope.proven.contains(&(operation.clone(), input.clone())) {
                    collapsed = false;

                    obstacles.push(IdempotencyObstacle::OutboxConsumerRequirementUnproven {
                        path: path.clone(),
                        effect: effect.clone(),
                        outbox: outbox.clone(),
                        schema: schema.clone(),
                        operation,
                        input,
                    });
                } else {
                    consumers.push(ConsumerCollapse::ProvenRequirement { operation, input });
                }
            }

            let instance = match instance {
                Ok(instance) => instance.clone(),

                Err(gap) => {
                    obstacles.push(instance_obstacle(path, site, gap));

                    return None;
                }
            };

            (identified && collapsed).then_some(EffectSafety::SameLogicalOutboxMessage {
                outbox: outbox.clone(),
                schema: schema.clone(),
                instance,
                consumers,
            })
        }
        // The external effect leg consumes only the declared
        // duplicate-side-effect behaviour; `result_replay` plays no
        // role here (§13.3) — result stability is the replay
        // analysis's separate concern.
        EffectContract::External(external) => match &external.idempotency {
            ExternalIdempotency::SideEffectFree => Some(EffectSafety::ExternallySideEffectFree),

            ExternalIdempotency::IdenticalPerIdentity => {
                // Validation guarantees a keyed identity accompanies
                // the declaration; stay total regardless.
                let ExternalIdentity::Keyed { key } = &external.identity else {
                    obstacles.push(IdempotencyObstacle::ExternalIdempotencyUnknown {
                        path: path.clone(),
                        effect: effect.clone(),
                    });

                    return None;
                };

                let roots: Vec<_> = key.components.iter().collect();

                let (stable, unstable) = analysis.roots_stability(context, &roots);

                if unstable.is_empty() {
                    Some(EffectSafety::ExternallyIdempotent {
                        identity_key: stable,
                    })
                } else {
                    obstacles.push(IdempotencyObstacle::ExternalIdentityKeyUnstable {
                        path: path.clone(),
                        effect: effect.clone(),
                        roots: unstable,
                    });

                    None
                }
            }

            ExternalIdempotency::Distinguishable => {
                obstacles.push(IdempotencyObstacle::ExternalApplicationsDistinguishable {
                    path: path.clone(),
                    effect: effect.clone(),
                });

                None
            }

            ExternalIdempotency::Unspecified => {
                obstacles.push(IdempotencyObstacle::ExternalIdempotencyUnknown {
                    path: path.clone(),
                    effect: effect.clone(),
                });

                None
            }
        },

        EffectContract::Publication(publication) => {
            let topic = &publication.topic;
            let schema = &publication.schema;

            let identified =
                scope
                    .model
                    .topics
                    .get(topic)
                    .is_some_and(|topic| match &topic.message_identity {
                        MessageIdentity::Keyed(MessageIdentityKey { mapping }) => mapping.contains_key(schema),
                        MessageIdentity::Unspecified => false,
                    });

            if !identified {
                obstacles.push(IdempotencyObstacle::PublicationNotIdentified {
                    path: path.clone(),
                    effect: effect.clone(),
                    topic: topic.clone(),
                    schema: schema.clone(),
                });
            }

            // The cascade: one logical message still reaches every
            // modeled consumer, and the duplicate work it would do
            // there is work this attempt caused. Each consumer must
            // collapse duplicate deliveries — by a proven requirement
            // keyed from the subscription, or by never seeing a second
            // delivery of the one message.
            let mut consumers = Vec::new();
            let mut collapsed = true;

            for consumer in scope.graph.consumers(topic, schema) {
                let operation = consumer.operation.clone();
                let input = consumer.input.clone();

                if identified
                    && scope.model.delivery(consumer.operation, consumer.input)
                        == DeliverySemantics::AtMostOnce
                {
                    consumers.push(ConsumerCollapse::SingleDelivery { operation, input });
                } else if !scope
                    .model
                    .operations
                    .get(consumer.operation)
                    .is_some_and(|target| collapses_duplicates(target, consumer.input))
                {
                    collapsed = false;

                    obstacles.push(IdempotencyObstacle::PublicationConsumerNotKeyed {
                        path: path.clone(),
                        effect: effect.clone(),
                        topic: topic.clone(),
                        schema: schema.clone(),
                        operation,
                        input,
                    });
                } else if !scope.proven.contains(&(operation.clone(), input.clone())) {
                    collapsed = false;

                    obstacles.push(
                        IdempotencyObstacle::PublicationConsumerRequirementUnproven {
                            path: path.clone(),
                            effect: effect.clone(),
                            topic: topic.clone(),
                            schema: schema.clone(),
                            operation,
                            input,
                        },
                    );
                } else {
                    consumers.push(ConsumerCollapse::ProvenRequirement { operation, input });
                }
            }

            let instance = match instance {
                Ok(instance) => instance.clone(),

                Err(gap) => {
                    obstacles.push(instance_obstacle(path, site, gap));

                    return None;
                }
            };

            (identified && collapsed).then_some(EffectSafety::SameLogicalMessage {
                topic: topic.clone(),
                schema: schema.clone(),
                instance,
                consumers,
            })
        }

        EffectContract::Request(request) => {
            let target_operation = &request.target.operation;
            let target_input = &request.target.input;

            let mut target_ok = false;

            match scope
                .model
                .operations
                .get(target_operation)
                .and_then(|target| target.inputs.get(target_input).map(|input| (target, input)))
            {
                Some((target, Input::Request(declared))) => {
                    if declared.schema != request.schema {
                        obstacles.push(IdempotencyObstacle::RequestSchemaMismatch {
                            path: path.clone(),
                            effect: effect.clone(),
                            expected: declared.schema.clone(),
                            actual: request.schema.clone(),
                        });
                    } else if !collapses_duplicates(target, target_input) {
                        obstacles.push(IdempotencyObstacle::RequestTargetHasNoKeyedRequirement {
                            path: path.clone(),
                            effect: effect.clone(),
                            operation: target_operation.clone(),
                            input: target_input.clone(),
                        });
                    } else if !scope
                        .proven
                        .contains(&(target_operation.clone(), target_input.clone()))
                    {
                        obstacles.push(IdempotencyObstacle::RequestTargetRequirementUnproven {
                            path: path.clone(),
                            effect: effect.clone(),
                            operation: target_operation.clone(),
                            input: target_input.clone(),
                        });
                    } else {
                        target_ok = true;
                    }
                }

                _ => {
                    obstacles.push(IdempotencyObstacle::RequestTargetHasNoKeyedRequirement {
                        path: path.clone(),
                        effect: effect.clone(),
                        operation: target_operation.clone(),
                        input: target_input.clone(),
                    });
                }
            }

            let instance = match instance {
                Ok(instance) => instance.clone(),

                Err(gap) => {
                    obstacles.push(instance_obstacle(path, site, gap));

                    return None;
                }
            };

            target_ok.then_some(EffectSafety::DeduplicatedByTarget {
                operation: target_operation.clone(),
                input: target_input.clone(),
                instance,
            })
        }
    }
}

impl IdempotencyCheck {
    /// The diagnostic for an unproven requirement; a proven one
    /// produces none.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let IdempotencyVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let requirement = self.requirement;

        Some(Diagnostic {
            code: DiagnosticCode::Verification(VerificationCode::IdempotencyUnproven),
            severity: Severity::Unknown,
            subject: Some(operation.clone()),
            message: format!(
                "Idempotency requirement {requirement} of `{operation}` is not \
                 established: repeated attempts sharing the declared key are not \
                 proven to avoid externally distinguishable duplicate logical \
                 work."
            ),
            evidence: obstacles
                .iter()
                .map(IdempotencyObstacle::evidence)
                .collect(),
        })
    }
}

impl IdempotencyObstacle {
    fn evidence(&self) -> Evidence {
        match self {
            Self::GoverningKeyInadmissible { defect } => governing_key_evidence(defect),

            Self::PathDecisionUnstable {
                path,
                decision,
                gap,
            } => Evidence {
                subject: None,
                message: format!(
                    "On {}, {} is not established to replay, so a retry may do \
                     different work: {}.",
                    describe_path(path),
                    describe_decision(decision),
                    decision_gap_sentence(gap)
                ),
            },

            Self::TransactionNotRetrySafe {
                path,
                transaction,
                recovery,
                reconstruction,
            } => Evidence {
                subject: Some(transaction.clone()),
                message: format!(
                    "A duplicate attempt re-drives {} and re-encounters \
                     `{transaction}`, which is retry-safe by neither route. \
                     Recovery: {}. Reconstruction: {}.",
                    describe_path(path),
                    gap_sentences(recovery),
                    gap_sentences(reconstruction),
                ),
            },

            Self::ExternalApplicationsDistinguishable { path, effect } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "{} applies external effect `{effect}`, declared \
                     `distinguishable`: a duplicate application may produce \
                     distinguishable modeled external work at that boundary.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::ExternalIdempotencyUnknown { path, effect } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "{} applies external effect `{effect}`, and no \
                     duplicate-side-effect fact is declared for that boundary.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::ExternalIdentityKeyUnstable {
                path,
                effect,
                roots,
            } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "External effect `{effect}` on {} declares an interaction \
                     identity that is not replay-stable, so attempts may address \
                     different logical interactions: {}.",
                    describe_path(path),
                    unstable_roots(roots)
                ),
            },

            Self::PublicationNotIdentified {
                path,
                effect,
                topic,
                schema,
            } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "{} publishes `{schema}` to `{topic}` through `{effect}`, but the \
                     topic declares no message identity for that schema, so \
                     duplicate publications are not established to be the same \
                     logical message.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::PublicationConsumerNotKeyed {
                path,
                effect,
                topic,
                schema,
                operation,
                input,
            } => Evidence {
                subject: Some(operation.clone()),
                message: format!(
                    "{} publishes `{schema}` to `{topic}` through `{effect}`, and \
                     `{operation}` consumes it through `{input}` with no idempotency \
                     requirement keyed from that input; nothing collapses the \
                     duplicate work a duplicate delivery causes there.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::PublicationConsumerRequirementUnproven {
                path,
                effect,
                topic,
                schema,
                operation,
                input,
            } => Evidence {
                subject: Some(operation.clone()),
                message: format!(
                    "{} publishes `{schema}` to `{topic}` through `{effect}`, and \
                     `{operation}` consumes it through `{input}`, whose idempotency \
                     requirement is not proven in this analysis, so the duplicate \
                     work a duplicate delivery causes there is not established to \
                     collapse.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::OutboxWriteNotIdentified {
                path,
                effect,
                transaction,
                outbox,
                schema,
            } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "{} admits `{schema}` to outbox `{outbox}` through `{effect}` in \
                     `{transaction}`; the transaction is not established to suppress \
                     a second commit, and the outbox declares no message identity for \
                     that schema, so repeated committed writes are not established to \
                     be the same logical message.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::OutboxConsumerNotKeyed {
                path,
                effect,
                outbox,
                schema,
                operation,
                input,
            } => Evidence {
                subject: Some(operation.clone()),
                message: format!(
                    "{} admits `{schema}` to outbox `{outbox}` through `{effect}`, and \
                     `{operation}` consumes it through `{input}` with no idempotency \
                     requirement keyed from that input; nothing collapses the \
                     duplicate work a duplicate delivery causes there.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::OutboxConsumerRequirementUnproven {
                path,
                effect,
                outbox,
                schema,
                operation,
                input,
            } => Evidence {
                subject: Some(operation.clone()),
                message: format!(
                    "{} admits `{schema}` to outbox `{outbox}` through `{effect}`, and \
                     `{operation}` consumes it through `{input}`, whose idempotency \
                     requirement is not proven in this analysis, so the duplicate \
                     work a duplicate delivery causes there is not established to \
                     collapse.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::EffectInstanceUnspecified { path, effect } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "{} executes `{effect}` with unspecified instance provenance, so \
                     the instances attempts construct are not class-fixed.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::EffectInstanceRootUnstable {
                path,
                effect,
                roots,
            } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "The instance `{effect}` constructs on {} depends on roots that \
                     are not replay-stable: {}.",
                    describe_path(path),
                    unstable_roots(roots)
                ),
            },

            Self::IntentNotEstablished { path, intent } => Evidence {
                subject: Some(intent.clone()),
                message: format!(
                    "{} executes intent `{intent}`, but no earlier step establishes \
                     it, so no class-fixed instance exists.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::IntentNotReplayAvailable {
                path,
                intent,
                transaction,
                recovery,
                reconstruction,
            } => Evidence {
                subject: Some(transaction.clone()),
                message: format!(
                    "Intent `{intent}` on {} is established by `{transaction}` but \
                     replay-available through neither route, so a retry's instance \
                     may differ. Recovery: {}. Reconstruction: {}.",
                    describe_path(path),
                    gap_sentences(recovery),
                    gap_sentences(reconstruction),
                ),
            },

            Self::RequestSchemaMismatch {
                path,
                effect,
                expected,
                actual,
            } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "Request effect `{effect}` on {} declares schema `{actual}`, but \
                     the targeted input declares `{expected}`, so payload equality \
                     does not transfer to the target's key.",
                    describe_path(path)
                ),
            },

            Self::RequestTargetHasNoKeyedRequirement {
                path,
                effect,
                operation,
                input,
            } => Evidence {
                subject: Some(effect.clone()),
                message: format!(
                    "Request effect `{effect}` on {} targets `{operation}` through \
                     `{input}`, which carries no idempotency requirement keyed from \
                     that input; nothing collapses duplicate invocations.",
                    describe_path(path)
                ),
            },

            Self::RequestTargetRequirementUnproven {
                path,
                effect,
                operation,
                input,
            } => Evidence {
                subject: Some(operation.clone()),
                message: format!(
                    "Request effect `{effect}` on {} targets `{operation}` through \
                     `{input}`, whose idempotency requirement is not proven in this \
                     analysis, so duplicate invocations are not established to \
                     collapse.",
                    describe_path(path)
                ),
            },
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}
