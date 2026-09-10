//! The replay engine: root stability, natural transaction
//! replayability, artifact replay availability, and — over the
//! operation program — effect-result and decision replay (§16, §17,
//! §18).
//!
//! Everything here is judged relative to a **governing key** — the
//! `IdempotencyKey` of the obligation under proof — and the attempt
//! population it defines (§12). The judgments form one simultaneous
//! induction, computed in a single forward pass over one path of the
//! program: every rule consumes either roots or facts established at
//! earlier steps, and transaction-read dependence, the only
//! backward-looking observation, is excluded outright.
//!
//! ## Root stability
//!
//! A `ValueRef` is replay-stable when any two attempts in the same
//! class that evaluate it obtain equal logical values. The §18 rules:
//! stability is definitional (governing-key components), declared (a
//! request or message identity pinned by the key), or derived
//! (recovered or reconstructed artifacts, replay-consistent effect
//! results, congruence). Everything else is a recorded gap, never an
//! assumption.
//!
//! ## Natural replayability
//!
//! V1 proves a transaction naturally replayable only when re-executing
//! its body for the same logical invocation reproduces the same
//! committed state: no `Transition` (§22), no `Insert`
//! (duplicate-identity insert outcomes are undefined — §27
//! question 4), no `Delete` (§20), and every `Write` with a stable
//! target and a replay-deterministic derivation. Reads and locks do
//! not mutate state and do not block the natural route; read-dependent
//! provenance is blocked where it is consumed, because a
//! transaction-read root is never stable.
//!
//! Natural replayability is judged from the transaction-entry artifact
//! context. Artifact derivations see artifacts established earlier in
//! the same transaction, in step order.
//!
//! ## Artifact availability
//!
//! For each artifact a retry may need, availability follows §17:
//! **recovery** (route B) when the establishing transaction declares
//! `deduplicated_by` over a stable key — the single successful commit
//! durably retains the exact artifact, so its derivation needs no
//! determinism at all; otherwise **reconstruction** (route A) when the
//! transaction is naturally replayable and the artifact's derivation
//! is replay-deterministic. Otherwise the artifact is unavailable,
//! with the gaps of both routes recorded.
//!
//! ## Effect results and decisions
//!
//! A bound effect result is judged per observed variant (§18 rule 6).
//! A request result is replay-stable — in both variants at once — when
//! the outgoing instance is class-fixed and the target proves its
//! result replay-consistent for the targeted input; stable outgoing
//! values do not by themselves prove a stable returned result (§13).
//! An external result is replay-stable when the boundary declares
//! `deduplicated_by` over a class-fixed key — equal keys identify one
//! logical external interaction whose terminal result is fixed — and
//! the observed variant is terminal: `Ok` by definition, `Err` only
//! under a declared `terminal` disposition. A retryable `Err` is an
//! attempt-level, nonterminal outcome, and an unspecified disposition
//! provides no usable fact; neither is promoted by idempotency alone.
//! A decision replays — every attempt in a class takes the same arm —
//! when the matched result is stable in the variant of the arm taken,
//! or the branch condition is deterministic over stable roots (§16).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::spec::{
    Derivation, ErrorDisposition, ExternalEffect, FieldPath, Id, IdempotencyGuarantee,
    IdempotencyKey, Input, MessageIdentity, MessageSelector, Model, Operation, RequestIdentity,
    ResultVariant, StepLocation, Transaction, TransactionStep, ValueRef, ValueSource, MessageIdentityKey, RequestIdentityKey,
};

use super::paths::{Decision, DecisionTaken, Path, PathStep, Terminal};
use super::trigger::{EffectContract, returns_consistently};
use super::value_identity::canonical_value_path;

/// Why a governing key cannot define a pre-execution equivalence
/// class (§12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoverningKeyDefect {
    /// An empty key places every attempt in one class; essentially
    /// nothing is replay-stable relative to it.
    Empty,

    /// A component is sourced from mutable state, from an artifact the
    /// invocation itself produces, or from an observation it makes.
    ComponentNotFromInput { source: ValueSource },

    /// Components name more than one input; no single triggering
    /// input defines the population.
    ComponentsFromMultipleInputs { first: Id, second: Id },

    /// The named input is not declared by the operation.
    InputNotDeclared { input: Id },
}

/// The rule that established a root as replay-stable. A proof carries
/// these so it states the facts it depends on (§25).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StabilityRule {
    /// Pinned by the governing key: class membership fixes it.
    KeyComponent,

    /// Covered by a declared request or message identity that the
    /// governing key pins; same-class attempts present one logical
    /// stimulus.
    IdentifiedPayload,

    /// A reference into an artifact recovered from the single keyed
    /// commit of the named transaction.
    RecoveredArtifact { transaction: Id },

    /// A reference into an artifact reconstructed by natural replay of
    /// the named transaction.
    ReconstructedArtifact { transaction: Id },

    /// A reference into a bound effect result that every attempt in
    /// the class observes equally: the instance is class-fixed and the
    /// target returns one result for it.
    ReplayConsistentResult { result: Id, effect: Id },

    /// A reference into a terminal external result that every attempt
    /// in the class observes equally: the boundary deduplicates by a
    /// class-fixed key, so equal keys identify one logical external
    /// interaction whose terminal result is fixed, and the referenced
    /// variant is terminal — `Ok` by definition, `Err` by declared
    /// disposition.
    DeduplicatedExternalResult {
        result: Id,
        effect: Id,
        variant: ResultVariant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StableRoot {
    pub root: ValueRef,
    pub rule: StabilityRule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnstableRoot {
    pub root: ValueRef,
    pub gap: StabilityGap,
}

/// Why no V1 rule establishes a root as replay-stable. Epistemic
/// (§1.1): instability is not proven.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StabilityGap {
    /// A non-key field of the triggering payload with no identity
    /// coverage.
    UnidentifiedPayloadField {
        input: Id,
        identity: PayloadIdentityGap,
    },

    /// The root belongs to an input that does not trigger the
    /// key-bearing invocations.
    NotTriggeringInput { input: Id },

    /// Mutable persistent state; V1 attempts no invariance analysis.
    MutableSubjectState { machine: Id },

    /// Effect payloads are not stable roots in V1.
    EffectPayloadRoot { effect: Id },

    /// A transaction-read result is never replay-stable (§18).
    TransactionReadRoot { read: Id },

    /// The referenced artifact is established, but replay-available
    /// through neither route; both routes' gaps are recorded.
    ArtifactUnavailable {
        artifact: Id,
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },

    /// The referenced artifact is not established before this point
    /// of the path.
    ArtifactNotInContext { artifact: Id },

    /// The referenced result binding is not bound before this point
    /// of the path.
    ResultNotInContext { result: Id },

    /// The referenced result is bound, but same-class attempts are not
    /// established to observe the same result.
    ResultUnstable {
        result: Id,
        effect: Id,
        gap: Box<ResultGap>,
    },
}

/// Why the declared identities do not make the triggering payload
/// stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadIdentityGap {
    /// No request identity, or no topic message identity, is declared
    /// for the triggering input.
    NotDeclared,

    /// The topic declares message identity, but not for this admitted
    /// schema.
    SchemaNotMapped { schema: Id },

    /// The identity is declared but the governing key does not pin
    /// this identity field in every admitted schema.
    NotPinnedByKey {
        schema: Option<Id>,
        field: FieldPath,
    },
}

/// A missing premise of a replay route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayGap {
    /// Route B: the transaction declares no keyed commit
    /// deduplication.
    NoKeyedCommit,

    /// Route B: a commit-key component is not replay-stable, so
    /// attempts may address different commits.
    CommitKeyRootUnstable { root: ValueRef, gap: StabilityGap },

    /// Route A: the transaction applies a state transition, which V1
    /// never replays naturally.
    ContainsTransition,

    /// Route A: the transaction inserts an object; duplicate-identity
    /// insert outcomes are not yet defined (§27 question 4).
    ContainsInsert,

    /// Route A: the transaction deletes objects; deletion replay
    /// outcomes are not defined (§20).
    ContainsDelete,

    /// Route A: a mutation target depends on an unstable root.
    MutationTargetRootUnstable { root: ValueRef, gap: StabilityGap },

    /// Route A: a mutation declares no value provenance.
    MutationDerivationUnspecified,

    /// Route A: a mutation value depends on an unstable root.
    MutationDerivationRootUnstable { root: ValueRef, gap: StabilityGap },

    /// Route A: the artifact's establishment declares no value
    /// provenance.
    ArtifactDerivationUnspecified,

    /// Route A: the artifact's values depend on an unstable root.
    ArtifactDerivationRootUnstable { root: ValueRef, gap: StabilityGap },
}

/// How an artifact reaches a retry, or why it does not (§17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactReplay {
    /// Route B: recovered from the single `Commit(T,K)` of the
    /// establishing transaction, whose key is stable through the
    /// cited rules.
    Recovered {
        transaction: Id,
        key: Vec<StableRoot>,
    },

    /// Route A: reconstructed by natural replay, with the artifact's
    /// derivation replay-deterministic over the cited roots.
    Reconstructed {
        transaction: Id,
        derivation: Vec<StableRoot>,
    },

    /// Neither route holds; both routes' gaps are recorded.
    Unavailable {
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },
}

impl ArtifactReplay {
    pub fn transaction(&self) -> &Id {
        match self {
            Self::Recovered { transaction, .. }
            | Self::Reconstructed { transaction, .. }
            | Self::Unavailable { transaction, .. } => transaction,
        }
    }

    pub fn is_available(&self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }
}

/// Why every attempt constructs the same logical effect instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstanceStability {
    /// A direct execution whose derivation is replay-deterministic
    /// over the cited roots.
    ReplayDeterministic { roots: Vec<StableRoot> },

    /// An intent whose values were fixed at establishment and are
    /// recovered or reconstructed on every attempt.
    EstablishedIntent { intent: Id, replay: ArtifactReplay },
}

/// Why an effect instance is not class-fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstanceGap {
    /// A direct execution declares no instance provenance.
    DerivationUnspecified,

    /// A direct execution's instance derivation depends on unstable
    /// roots.
    RootsUnstable { roots: Vec<UnstableRoot> },

    /// The executed intent is established by no earlier step of the
    /// path.
    IntentNotEstablished { intent: Id },

    /// The executed intent is replay-available through neither route.
    IntentNotReplayAvailable {
        intent: Id,
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },
}

/// Whether same-class attempts observe one result from a bound effect
/// execution, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultReplay {
    Stable {
        effect: Id,
        rule: ResultStabilityRule,
    },

    Unstable {
        effect: Id,
        gap: ResultGap,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultStabilityRule {
    /// The request instance is class-fixed, so every attempt sends a
    /// payload-equal request into one class of the target's
    /// replay-consistent result requirement, which is proven: the
    /// target returns the same variant and a replay-equivalent payload
    /// to each of them (§13.2).
    ReplayConsistentTarget {
        operation: Id,
        input: Id,
        requirement: usize,
        instance: InstanceStability,
    },

    /// The external boundary declares `deduplicated_by` over a key
    /// that is class-fixed through the cited roots, so same-class
    /// attempts address one logical external interaction, whose
    /// terminal result the guarantee fixes — and the observed variant
    /// is terminal: `Ok` by definition, `Err` by the contract's
    /// declared `terminal` disposition (§13.3).
    ExternalTerminalResult {
        variant: ResultVariant,
        key: Vec<StableRoot>,
    },
}

/// Why a bound result is not established to be observed equally by
/// every attempt in the class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultGap {
    /// The outgoing instance is not class-fixed, so attempts may ask
    /// different questions.
    InstanceNotClassFixed { gap: InstanceGap },

    /// The request effect's schema is not the targeted input's, so
    /// payload equality does not transfer.
    RequestSchemaMismatch { expected: Id, actual: Id },

    /// The target declares no replay-consistent result requirement
    /// keyed from the targeted input.
    TargetResultNotDeclared { operation: Id, input: Id },

    /// The target declares such a requirement, but it is not proven in
    /// this analysis.
    TargetResultUnproven { operation: Id, input: Id },

    /// The external boundary explicitly does not deduplicate, so the
    /// `deduplicated_by` terminal-result guarantee is unavailable.
    /// This does not say repeated executions return different results;
    /// only that no fact fixes them.
    ExternalNotDeduplicated,

    /// No deduplication fact is declared for the external boundary,
    /// so nothing identifies same-key executions as one logical
    /// interaction with one terminal result.
    ExternalDeduplicationUnknown,

    /// The declared external deduplication key is not replay-stable,
    /// so attempts may address different logical external
    /// interactions.
    ExternalDeduplicationKeyUnstable { roots: Vec<UnstableRoot> },

    /// The observed `Err` is declared retryable: an attempt-level,
    /// nonterminal outcome. It does not establish the logical
    /// interaction's terminal result, so a later attempt may observe a
    /// different result.
    ExternalErrorRetryable,

    /// The observed `Err` carries no declared disposition: no usable
    /// fact says whether it terminally resolves the logical
    /// interaction.
    ExternalErrorDispositionUnspecified,

    /// The effect contract yields no synchronous result.
    NoResultContract,

    /// The result was bound by a `race`: which candidate completes
    /// first is scheduling nondeterminism, so retries need not select
    /// the same winner. Conservative — a future verifier may prove
    /// stability when every possible winner yields replay-equivalent
    /// results; V1 does not attempt that equivalence proof.
    RaceWinnerNondeterministic { candidates: Vec<Id> },
}

/// The replay judgments of one bound result, per observed variant.
/// Stability is variant-sensitive (§18 rule 6): a deduplicated
/// external boundary's terminal `Ok` is stable while the same
/// binding's retryable `Err` is not. A request result carries one
/// judgment in both variants — the target's replay-consistent
/// requirement covers variant and payload together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundResult {
    pub ok: ResultReplay,
    pub err: ResultReplay,
}

impl BoundResult {
    /// One judgment in both variants.
    fn both(replay: ResultReplay) -> Self {
        Self {
            ok: replay.clone(),
            err: replay,
        }
    }

    /// The judgment for the observed variant.
    pub fn variant(&self, variant: ResultVariant) -> &ResultReplay {
        match variant {
            ResultVariant::Ok => &self.ok,
            ResultVariant::Err => &self.err,
        }
    }
}

/// What a path has made available so far: every established artifact
/// with its replay route, every bound result with its per-variant
/// replay judgments, and every launched async handle with the
/// judgment its result would carry if joined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathContext {
    pub artifacts: BTreeMap<Id, ArtifactReplay>,
    pub results: BTreeMap<Id, BoundResult>,
    pub handles: BTreeMap<Id, AsyncLaunch>,
}

/// The analyzer's bookkeeping for one asynchronous launch: the
/// underlying effect and the launch-time result judgment. A result
/// bound by joining the handle has the same logical result the effect
/// would have exposed synchronously (§43 of the async revision), so
/// the judgment is computed at the launch — where the instance and any
/// external deduplication key are evaluated — and surfaced at the
/// barrier that binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncLaunch {
    pub effect: Id,
    pub result: BoundResult,
}

/// Why every attempt in the class takes the same arm of a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionRule {
    /// The matched result is replay-stable, so the variant is fixed
    /// across the class.
    StableResult {
        result: Id,
        effect: Id,
        rule: ResultStabilityRule,
    },

    /// The condition is a deterministic function of replay-stable
    /// roots.
    StableCondition { roots: Vec<StableRoot> },
}

/// Why a retry is not established to take the same arm (§16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionGap {
    /// The condition declares no fact about how the decision is made.
    ConditionUnspecified,

    /// The condition depends on roots that are not replay-stable.
    ConditionRootsUnstable { roots: Vec<UnstableRoot> },

    /// The matched result is bound by no earlier step of the path.
    ResultNotInContext { result: Id },

    /// The matched result is not established to be observed equally
    /// by every attempt.
    ResultUnstable {
        result: Id,
        effect: Id,
        gap: ResultGap,
    },
}

/// A decision a proof cites: the arm taken and why every attempt
/// takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReplay {
    pub decision: DecisionTaken,
    pub rule: DecisionRule,
}

/// One effect-executing site on a path.
#[derive(Debug, Clone)]
pub enum EffectSite<'a> {
    Direct {
        effect: &'a Id,
        values: &'a Derivation,
    },

    Intent {
        intent: &'a Id,
        effect: &'a Id,
    },

    /// A transactional outbox write, executed as a step of the named
    /// transaction: its admission commits with that transaction, so
    /// the transaction's keyed-commit judgment is part of the site's
    /// duplicate-execution analysis (§50 of the outbox revision).
    OutboxWrite {
        effect: &'a Id,
        values: &'a Derivation,
        transaction: &'a Id,
    },
}

impl<'a> EffectSite<'a> {
    pub fn effect(&self) -> &'a Id {
        match self {
            Self::Direct { effect, .. }
            | Self::Intent { effect, .. }
            | Self::OutboxWrite { effect, .. } => effect,
        }
    }
}

/// One step of a path, judged by the replay engine in the context the
/// path had built before it.
#[derive(Debug, Clone)]
pub enum TracedStep<'a> {
    Transaction {
        location: StepLocation,
        transaction: &'a Transaction,
        before: PathContext,
        recovery: Result<Vec<StableRoot>, Vec<ReplayGap>>,
        natural: Result<(), Vec<ReplayGap>>,
    },

    Effect {
        location: StepLocation,
        site: EffectSite<'a>,
        contract: Option<EffectContract<'a>>,
        before: PathContext,
        instance: Result<InstanceStability, InstanceGap>,
        result: Option<(&'a Id, Box<BoundResult>)>,
    },

    Decision {
        location: StepLocation,
        taken: DecisionTaken,
        replay: Result<DecisionRule, DecisionGap>,
    },
}

/// A path walked to its end.
#[derive(Debug, Clone)]
pub struct PathTrace<'a> {
    pub steps: Vec<TracedStep<'a>>,
    pub terminal: Terminal<'a>,

    /// The context at the terminal.
    pub end: PathContext,
}

impl<'a> PathTrace<'a> {
    /// Every decision on the path a retry is not established to take
    /// again.
    pub fn unstable_decisions(&self) -> Vec<(&DecisionTaken, &DecisionGap)> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                TracedStep::Decision {
                    taken,
                    replay: Err(gap),
                    ..
                } => Some((taken, gap)),

                _ => None,
            })
            .collect()
    }

    /// Every decision on the path with the fact that fixes its arm.
    pub fn stable_decisions(&self) -> Vec<DecisionReplay> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                TracedStep::Decision {
                    taken,
                    replay: Ok(rule),
                    ..
                } => Some(DecisionReplay {
                    decision: taken.clone(),
                    rule: rule.clone(),
                }),

                _ => None,
            })
            .collect()
    }
}

/// The intent-binding producer facts the engine resolves an
/// `ExecuteEffectIntent` against: the captured effect occurrence and
/// its contract, walked out of the operation program — an explicit
/// `EstablishEffectIntent` site, or a transition application binding a
/// state-machine side effect.
#[derive(Debug, Clone, Copy)]
struct IntentSite<'a> {
    effect: &'a Id,
    contract: EffectContract<'a>,
}

/// One transactional outbox write of a traced transaction, with its
/// instance judged at its step position and the context in force
/// there — the transaction-entry context plus the artifacts earlier
/// steps of the same transaction established.
pub(crate) struct OutboxWriteTrace<'a> {
    pub step: &'a crate::spec::WriteOutboxEffect,
    pub before: PathContext,
    pub instance: Result<InstanceStability, InstanceGap>,
}

/// The replay engine for one operation and governing key.
pub struct ReplayAnalysis<'a> {
    model: &'a Model,
    operation: &'a Operation,
    input: &'a Id,
    key: &'a IdempotencyKey,

    /// The `(operation, input)` pairs whose replay-consistent result
    /// requirement is proven — or, during the result-replay fixpoint,
    /// assumed — so a request into one of them observes one result.
    consistent: &'a BTreeSet<(Id, Id)>,

    /// Payload schemas of the triggering input: the request schema, or
    /// the subscription's admitted message schemas. `None` when the
    /// subscribed topic is unresolvable, in which case only syntactic
    /// path equality is trusted.
    schemas: Option<Vec<&'a Id>>,

    /// Whether the whole triggering payload is stable under a declared
    /// identity pinned by the key (§18 rule 3).
    payload: Result<(), PayloadIdentityGap>,

    /// Each intent binding's producer facts, derived from the program.
    intents: BTreeMap<&'a Id, IntentSite<'a>>,
}

impl<'a> ReplayAnalysis<'a> {
    /// Resolves the governing key (§12): every component must name one
    /// declared input of the operation.
    pub fn new(
        model: &'a Model,
        operation: &'a Operation,
        key: &'a IdempotencyKey,
        consistent: &'a BTreeSet<(Id, Id)>,
    ) -> Result<Self, GoverningKeyDefect> {
        let mut input: Option<&Id> = None;

        for component in &key.components {
            let ValueSource::Input(id) = &component.source else {
                return Err(GoverningKeyDefect::ComponentNotFromInput {
                    source: component.source.clone(),
                });
            };

            match input {
                None => input = Some(id),

                Some(first) if first != id => {
                    return Err(GoverningKeyDefect::ComponentsFromMultipleInputs {
                        first: first.clone(),
                        second: id.clone(),
                    });
                }

                Some(_) => {}
            }
        }

        let Some(input) = input else {
            return Err(GoverningKeyDefect::Empty);
        };

        let Some(declaration) = operation.inputs.get(input) else {
            return Err(GoverningKeyDefect::InputNotDeclared {
                input: input.clone(),
            });
        };

        let schemas = match declaration {
            Input::Request(request) => Some(vec![&request.schema]),

            Input::Subscription(subscription) => match &subscription.messages {
                MessageSelector::Only(messages) => Some(messages.iter().collect()),

                MessageSelector::All => model
                    .topics
                    .get(&subscription.topic)
                    .map(|topic| topic.messages.iter().collect()),
            },

            Input::Outbox(outbox_input) => match &outbox_input.messages {
                MessageSelector::Only(messages) => Some(messages.iter().collect()),

                MessageSelector::All => model
                    .outbox(&outbox_input.outbox)
                    .map(|(_, outbox)| outbox.messages.iter().collect()),
            },
        };

        // Resolve each intent binding to its producer site: an
        // explicit establishment declares the captured contract
        // inline; a transition application captures the state-machine
        // side effect it names.
        let mut intents: BTreeMap<&Id, IntentSite<'_>> = BTreeMap::new();

        for (_, transaction) in operation.program.transactions() {
            for inner in &transaction.steps {
                match inner {
                    TransactionStep::EstablishEffectIntent(establish) => {
                        intents.insert(
                            &establish.bind,
                            IntentSite {
                                effect: &establish.effect_id,
                                contract: EffectContract::from(&establish.effect),
                            },
                        );
                    }

                    TransactionStep::Transition(transition) => {
                        let declared = model
                            .state_machines
                            .get(&transition.machine)
                            .and_then(|machine| machine.transitions.get(&transition.transition));

                        for (effect_id, intent) in &transition.effect_intents {
                            let Some(side_effect) = declared
                                .and_then(|transition| transition.side_effects.get(effect_id))
                            else {
                                continue;
                            };

                            intents.insert(
                                &intent.bind,
                                IntentSite {
                                    effect: effect_id,
                                    contract: EffectContract::from(side_effect),
                                },
                            );
                        }
                    }

                    _ => {}
                }
            }
        }

        let mut analysis = Self {
            model,
            operation,
            input,
            key,
            consistent,
            schemas,
            payload: Err(PayloadIdentityGap::NotDeclared),
            intents,
        };

        analysis.payload = analysis.payload_stability(declaration);

        Ok(analysis)
    }

    pub fn input(&self) -> &Id {
        self.input
    }

    /// The operation under analysis.
    pub fn operation(&self) -> &'a Operation {
        self.operation
    }

    /// Whether the whole triggering payload is identity-pinned by the
    /// governing key (§18 rule 3), making same-class stimuli one
    /// logical stimulus.
    pub fn payload_identified(&self) -> bool {
        self.payload.is_ok()
    }

    /// Whether the population is empty by declaration: the triggering
    /// subscription admits no message schemas. An unresolvable topic
    /// leaves the admitted set unknown and must not become a vacuous
    /// proof.
    pub fn admits_no_attempts(&self) -> bool {
        matches!(&self.schemas, Some(schemas) if schemas.is_empty())
    }

    /// §18 rule 3: is every field of the triggering payload stable
    /// under a declared identity pinned by the governing key?
    fn payload_stability(&self, declaration: &Input) -> Result<(), PayloadIdentityGap> {
        match declaration {
            Input::Request(request) => {
                let RequestIdentity::Keyed(RequestIdentityKey { fields }) = &request.identity else {
                    return Err(PayloadIdentityGap::NotDeclared);
                };

                if fields.is_empty() {
                    return Err(PayloadIdentityGap::NotDeclared);
                }

                for field in fields {
                    if !self.pinned_by_key(field) {
                        return Err(PayloadIdentityGap::NotPinnedByKey {
                            schema: None,
                            field: field.clone(),
                        });
                    }
                }

                Ok(())
            }

            Input::Subscription(subscription) => {
                let identity = self
                    .model
                    .topics
                    .get(&subscription.topic)
                    .map(|topic| &topic.message_identity);

                self.mapped_payload_stability(identity)
            }

            // An outbox declares message identity with the same
            // semantic concept a topic does, so the same rule pins the
            // triggering payload.
            Input::Outbox(outbox_input) => {
                let identity = self
                    .model
                    .outbox(&outbox_input.outbox)
                    .map(|(_, outbox)| &outbox.message_identity);

                self.mapped_payload_stability(identity)
            }
        }
    }

    /// The keyed message-identity half of §18 rule 3, shared by the
    /// two message-driven input kinds.
    fn mapped_payload_stability(
        &self,
        identity: Option<&MessageIdentity>,
    ) -> Result<(), PayloadIdentityGap> {
        let Some(MessageIdentity::Keyed(MessageIdentityKey { mapping })) = identity else {
            return Err(PayloadIdentityGap::NotDeclared);
        };

        let Some(schemas) = &self.schemas else {
            return Err(PayloadIdentityGap::NotDeclared);
        };

        let mut tuples = Vec::new();

        for schema in schemas {
            match mapping.get(*schema) {
                Some(tuple) if !tuple.is_empty() => tuples.push((*schema, tuple)),

                _ => {
                    return Err(PayloadIdentityGap::SchemaNotMapped {
                        schema: (*schema).clone(),
                    });
                }
            }
        }

        let Some((_, first)) = tuples.first() else {
            return Err(PayloadIdentityGap::NotDeclared);
        };

        // For each identity position, one key component must
        // pin that position's field in every admitted schema;
        // this is what carries key equality across schemas.
        for (position, field) in first.iter().enumerate() {
            let pinned = self.key.components.iter().any(|component| {
                tuples.iter().all(|(schema, tuple)| {
                    tuple.get(position).is_some_and(|identity_field| {
                        self.same_value(schema, &component.path, identity_field)
                    })
                })
            });

            if !pinned {
                let unpinned = tuples
                    .iter()
                    .find(|(_, tuple)| tuple.get(position).is_none());

                return Err(PayloadIdentityGap::NotPinnedByKey {
                    schema: unpinned.map(|(schema, _)| (*schema).clone()),
                    field: field.clone(),
                });
            }
        }

        Ok(())
    }

    /// Whether a payload path is pinned by the governing key in every
    /// admitted schema.
    fn pinned_by_key(&self, path: &FieldPath) -> bool {
        self.key.components.iter().any(|component| {
            match &self.schemas {
                Some(schemas) => schemas
                    .iter()
                    .all(|schema| self.same_value(schema, &component.path, path)),

                // Without resolvable schemas only syntactic equality
                // is trusted.
                None => &component.path == path,
            }
        })
    }

    /// Whether two paths denote the same logical value in any instance
    /// of the schema (§4 fragment identity).
    fn same_value(&self, schema: &Id, first: &FieldPath, second: &FieldPath) -> bool {
        if first == second {
            return true;
        }

        match (
            canonical_value_path(self.model, schema, first),
            canonical_value_path(self.model, schema, second),
        ) {
            (Some(first), Some(second)) => first == second,
            _ => false,
        }
    }

    /// The §18 root-stability judgment, resolved against the context
    /// the path has built so far.
    pub fn root_stability(
        &self,
        context: &PathContext,
        root: &ValueRef,
    ) -> Result<StableRoot, StabilityGap> {
        let rule = match &root.source {
            ValueSource::Input(input) if input == self.input => {
                if self.pinned_by_key(&root.path) {
                    Ok(StabilityRule::KeyComponent)
                } else {
                    match &self.payload {
                        Ok(()) => Ok(StabilityRule::IdentifiedPayload),

                        Err(identity) => Err(StabilityGap::UnidentifiedPayloadField {
                            input: input.clone(),
                            identity: identity.clone(),
                        }),
                    }
                }
            }

            ValueSource::Input(input) => Err(StabilityGap::NotTriggeringInput {
                input: input.clone(),
            }),

            ValueSource::StateMachineSubject(machine) => Err(StabilityGap::MutableSubjectState {
                machine: machine.clone(),
            }),

            ValueSource::Effect(effect) => Err(StabilityGap::EffectPayloadRoot {
                effect: effect.clone(),
            }),

            ValueSource::TransactionRead(read) => {
                Err(StabilityGap::TransactionReadRoot { read: read.clone() })
            }

            ValueSource::TransactionOutput(artifact) => match context.artifacts.get(artifact) {
                None => Err(StabilityGap::ArtifactNotInContext {
                    artifact: artifact.clone(),
                }),

                Some(ArtifactReplay::Unavailable {
                    transaction,
                    recovery,
                    reconstruction,
                }) => Err(StabilityGap::ArtifactUnavailable {
                    artifact: artifact.clone(),
                    transaction: transaction.clone(),
                    recovery: recovery.clone(),
                    reconstruction: reconstruction.clone(),
                }),

                Some(ArtifactReplay::Recovered { transaction, .. }) => {
                    Ok(StabilityRule::RecoveredArtifact {
                        transaction: transaction.clone(),
                    })
                }

                Some(ArtifactReplay::Reconstructed { transaction, .. }) => {
                    Ok(StabilityRule::ReconstructedArtifact {
                        transaction: transaction.clone(),
                    })
                }
            },

            ValueSource::EffectResultOk(result) | ValueSource::EffectResultErr(result) => {
                let variant = match &root.source {
                    ValueSource::EffectResultOk(_) => ResultVariant::Ok,
                    _ => ResultVariant::Err,
                };

                match context.results.get(result) {
                    None => Err(StabilityGap::ResultNotInContext {
                        result: result.clone(),
                    }),

                    Some(bound) => match bound.variant(variant) {
                        ResultReplay::Unstable { effect, gap } => {
                            Err(StabilityGap::ResultUnstable {
                                result: result.clone(),
                                effect: effect.clone(),
                                gap: Box::new(gap.clone()),
                            })
                        }

                        ResultReplay::Stable { effect, rule } => Ok(match rule {
                            ResultStabilityRule::ReplayConsistentTarget { .. } => {
                                StabilityRule::ReplayConsistentResult {
                                    result: result.clone(),
                                    effect: effect.clone(),
                                }
                            }

                            ResultStabilityRule::ExternalTerminalResult { .. } => {
                                StabilityRule::DeduplicatedExternalResult {
                                    result: result.clone(),
                                    effect: effect.clone(),
                                    variant,
                                }
                            }
                        }),
                    },
                }
            }
        }?;

        Ok(StableRoot {
            root: root.clone(),
            rule,
        })
    }

    /// Stability of every root of a derivation, split into the stable
    /// and the unstable.
    pub fn roots_stability(
        &self,
        context: &PathContext,
        roots: &[&ValueRef],
    ) -> (Vec<StableRoot>, Vec<UnstableRoot>) {
        let mut stable = Vec::new();
        let mut unstable = Vec::new();

        for root in roots {
            match self.root_stability(context, root) {
                Ok(root) => stable.push(root),

                Err(gap) => unstable.push(UnstableRoot {
                    root: (*root).clone(),
                    gap,
                }),
            }
        }

        (stable, unstable)
    }

    /// Walks a path in step order, judging every step in the context
    /// the path had built before it.
    pub fn trace(&self, path: &Path<'a>) -> PathTrace<'a> {
        let mut context = PathContext::default();
        let mut steps = Vec::new();

        for step in &path.steps {
            match step {
                PathStep::Transaction {
                    location,
                    transaction,
                } => {
                    let before = context.clone();

                    let (recovery, natural, writes) =
                        self.apply_transaction(&mut context, transaction);

                    steps.push(TracedStep::Transaction {
                        location: location.clone(),
                        transaction,
                        before: before.clone(),
                        recovery,
                        natural,
                    });

                    // Each contained outbox write is a real effect
                    // occurrence of the path, surfaced in step order
                    // after its transaction so a verifier sees the
                    // transaction's replay routes before judging the
                    // writes that commit with it. The write binds no
                    // result.
                    for write in writes {
                        steps.push(TracedStep::Effect {
                            location: location.clone(),
                            site: EffectSite::OutboxWrite {
                                effect: &write.step.effect_id,
                                values: &write.step.values,
                                transaction: &transaction.id,
                            },
                            contract: Some(EffectContract::OutboxWrite(&write.step.effect)),
                            before: write.before,
                            instance: write.instance,
                            result: None,
                        });
                    }
                }

                PathStep::ExecuteEffect {
                    location,
                    effect_id,
                    effect,
                    values,
                    bind,
                } => {
                    let contract = Some(EffectContract::from(*effect));
                    let instance = self.direct_instance(&context, values);

                    self.push_effect(
                        &mut context,
                        &mut steps,
                        location,
                        EffectSite::Direct {
                            effect: effect_id,
                            values,
                        },
                        contract,
                        instance,
                        *bind,
                    );
                }

                PathStep::ExecuteEffectIntent {
                    location,
                    intent,
                    bind,
                } => {
                    let Some(site) = self.intents.get(*intent).copied() else {
                        continue;
                    };

                    let instance = self.intent_instance(&context, intent);

                    self.push_effect(
                        &mut context,
                        &mut steps,
                        location,
                        EffectSite::Intent {
                            intent,
                            effect: site.effect,
                        },
                        Some(site.contract),
                        instance,
                        *bind,
                    );
                }

                PathStep::ExecuteEffectAsync {
                    location,
                    handle,
                    effect_id,
                    effect,
                    values,
                } => {
                    let contract = Some(EffectContract::from(*effect));
                    let instance = self.direct_instance(&context, values);

                    // The launch is a full effect attempt — the
                    // instance is judged here, where the derivation is
                    // evaluated — but binds no result. The judgment a
                    // joined result would carry is computed now and
                    // surfaced at the barrier that binds it.
                    let result = self.result_replay(&context, effect_id, contract, &instance);

                    self.push_effect(
                        &mut context,
                        &mut steps,
                        location,
                        EffectSite::Direct {
                            effect: effect_id,
                            values,
                        },
                        contract,
                        instance,
                        None,
                    );

                    context.handles.insert(
                        (*handle).clone(),
                        AsyncLaunch {
                            effect: (*effect_id).clone(),
                            result,
                        },
                    );
                }

                PathStep::ExecuteEffectIntentAsync {
                    location,
                    intent,
                    handle,
                } => {
                    let Some(site) = self.intents.get(*intent).copied() else {
                        continue;
                    };

                    let instance = self.intent_instance(&context, intent);

                    let result =
                        self.result_replay(&context, site.effect, Some(site.contract), &instance);

                    self.push_effect(
                        &mut context,
                        &mut steps,
                        location,
                        EffectSite::Intent {
                            intent,
                            effect: site.effect,
                        },
                        Some(site.contract),
                        instance,
                        None,
                    );

                    context.handles.insert(
                        (*handle).clone(),
                        AsyncLaunch {
                            effect: site.effect.clone(),
                            result,
                        },
                    );
                }

                PathStep::JoinAll { handles, .. } => {
                    // The barrier is not an effect attempt; it makes
                    // each joined result-bearing effect's result
                    // independently available, with exactly the
                    // judgment the launch computed (§43).
                    for entry in *handles {
                        let (Some(bind), Some(launch)) =
                            (&entry.bind, context.handles.get(&entry.handle))
                        else {
                            continue;
                        };

                        let result = launch.result.clone();

                        context.results.insert(bind.clone(), result);
                    }
                }

                PathStep::Race { handles, bind, .. } => {
                    // First completion is scheduling nondeterminism:
                    // a race-bound result is conservatively not
                    // replay-stable, whatever each candidate's own
                    // judgment (§44). The candidate set is recorded as
                    // the binding's possible producers.
                    let Some(bind) = bind else {
                        continue;
                    };

                    let candidates: Vec<Id> = handles
                        .iter()
                        .filter_map(|handle| {
                            context.handles.get(handle).map(|launch| launch.effect.clone())
                        })
                        .collect();

                    let effect = candidates.first().cloned().unwrap_or_else(|| (*bind).clone());

                    context.results.insert(
                        (*bind).clone(),
                        BoundResult::both(ResultReplay::Unstable {
                            effect,
                            gap: ResultGap::RaceWinnerNondeterministic { candidates },
                        }),
                    );
                }

                PathStep::Decision { location, decision } => {
                    let (taken, replay) = self.decision_replay(&context, location, decision);

                    steps.push(TracedStep::Decision {
                        location: location.clone(),
                        taken,
                        replay,
                    });
                }
            }
        }

        PathTrace {
            steps,
            terminal: path.terminal.clone(),
            end: context,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_effect(
        &self,
        context: &mut PathContext,
        steps: &mut Vec<TracedStep<'a>>,
        location: &StepLocation,
        site: EffectSite<'a>,
        contract: Option<EffectContract<'a>>,
        instance: Result<InstanceStability, InstanceGap>,
        result: Option<&'a Id>,
    ) {
        let before = context.clone();

        let result = result.map(|binding| {
            let replay = self.result_replay(&before, site.effect(), contract, &instance);

            context.results.insert(binding.clone(), replay.clone());

            (binding, Box::new(replay))
        });

        steps.push(TracedStep::Effect {
            location: location.clone(),
            site,
            contract,
            before,
            instance,
            result,
        });
    }

    /// A direct execution's instance: class-fixed iff its derivation is
    /// replay-deterministic.
    fn direct_instance(
        &self,
        context: &PathContext,
        values: &Derivation,
    ) -> Result<InstanceStability, InstanceGap> {
        match values {
            Derivation::Unspecified => Err(InstanceGap::DerivationUnspecified),

            Derivation::Deterministic { from } => {
                let roots: Vec<&ValueRef> = from.iter().collect();

                let (stable, unstable) = self.roots_stability(context, &roots);

                if unstable.is_empty() {
                    Ok(InstanceStability::ReplayDeterministic { roots: stable })
                } else {
                    Err(InstanceGap::RootsUnstable { roots: unstable })
                }
            }
        }
    }

    /// An intent execution's instance: class-fixed iff the intent is
    /// replay-available; its values were fixed at establishment.
    fn intent_instance(
        &self,
        context: &PathContext,
        intent: &Id,
    ) -> Result<InstanceStability, InstanceGap> {
        match context.artifacts.get(intent) {
            None => Err(InstanceGap::IntentNotEstablished {
                intent: intent.clone(),
            }),

            Some(ArtifactReplay::Unavailable {
                transaction,
                recovery,
                reconstruction,
            }) => Err(InstanceGap::IntentNotReplayAvailable {
                intent: intent.clone(),
                transaction: transaction.clone(),
                recovery: recovery.clone(),
                reconstruction: reconstruction.clone(),
            }),

            Some(replay) => Ok(InstanceStability::EstablishedIntent {
                intent: intent.clone(),
                replay: replay.clone(),
            }),
        }
    }

    /// Whether same-class attempts observe one result from an effect
    /// execution, judged per variant. A request result requires a
    /// class-fixed instance and a target proving its result
    /// replay-consistent (§13.2), and is then stable in both
    /// variants at once. An external result is judged by
    /// `external_result_replay`.
    fn result_replay(
        &self,
        context: &PathContext,
        effect: &Id,
        contract: Option<EffectContract<'_>>,
        instance: &Result<InstanceStability, InstanceGap>,
    ) -> BoundResult {
        let unstable = |gap| {
            BoundResult::both(ResultReplay::Unstable {
                effect: effect.clone(),
                gap,
            })
        };

        let request = match contract {
            None
            | Some(EffectContract::Publication(_))
            | Some(EffectContract::OutboxWrite(_)) => {
                return unstable(ResultGap::NoResultContract);
            }

            Some(EffectContract::External(external)) => {
                return self.external_result_replay(context, effect, external);
            }

            Some(EffectContract::Request(request)) => request,
        };

        let instance = match instance {
            Ok(instance) => instance.clone(),

            Err(gap) => {
                return unstable(ResultGap::InstanceNotClassFixed { gap: gap.clone() });
            }
        };

        let operation = &request.target.operation;
        let input = &request.target.input;

        let target = self
            .model
            .operations
            .get(operation)
            .and_then(|target| target.inputs.get(input).map(|declared| (target, declared)));

        let Some((target, Input::Request(declared))) = target else {
            return unstable(ResultGap::TargetResultNotDeclared {
                operation: operation.clone(),
                input: input.clone(),
            });
        };

        if declared.schema != request.schema {
            return unstable(ResultGap::RequestSchemaMismatch {
                expected: declared.schema.clone(),
                actual: request.schema.clone(),
            });
        }

        let Some(requirement) = returns_consistently(target, input) else {
            return unstable(ResultGap::TargetResultNotDeclared {
                operation: operation.clone(),
                input: input.clone(),
            });
        };

        if !self
            .consistent
            .contains(&(operation.clone(), input.clone()))
        {
            return unstable(ResultGap::TargetResultUnproven {
                operation: operation.clone(),
                input: input.clone(),
            });
        }

        BoundResult::both(ResultReplay::Stable {
            effect: effect.clone(),
            rule: ResultStabilityRule::ReplayConsistentTarget {
                operation: operation.clone(),
                input: input.clone(),
                requirement,
                instance,
            },
        })
    }

    /// The §13.3 judgment of an external result, per variant.
    ///
    /// `deduplicated_by` over a class-fixed key makes equal-key
    /// executions one logical external interaction whose terminal
    /// result is fixed, so a terminal variant is replay-stable: `Ok`
    /// by definition, `Err` under a declared `terminal` disposition.
    /// A retryable `Err` is attempt-level and nonterminal; an
    /// unspecified disposition provides no usable fact. The outgoing
    /// instance is not consulted: result identity follows the key
    /// alone, exactly as the boundary's duplicate-work collapse does.
    fn external_result_replay(
        &self,
        context: &PathContext,
        effect: &Id,
        external: &ExternalEffect,
    ) -> BoundResult {
        let unstable = |gap| {
            BoundResult::both(ResultReplay::Unstable {
                effect: effect.clone(),
                gap,
            })
        };

        let Some(result) = &external.result else {
            return unstable(ResultGap::NoResultContract);
        };

        let key = match &external.idempotency {
            IdempotencyGuarantee::DeduplicatedBy { key } => key,

            IdempotencyGuarantee::NotDeduplicated => {
                return unstable(ResultGap::ExternalNotDeduplicated);
            }

            IdempotencyGuarantee::Unspecified => {
                return unstable(ResultGap::ExternalDeduplicationUnknown);
            }
        };

        let roots: Vec<&ValueRef> = key.components.iter().collect();

        let (stable, unstable_roots) = self.roots_stability(context, &roots);

        if !unstable_roots.is_empty() {
            return unstable(ResultGap::ExternalDeduplicationKeyUnstable {
                roots: unstable_roots,
            });
        }

        let terminal = |variant| ResultReplay::Stable {
            effect: effect.clone(),
            rule: ResultStabilityRule::ExternalTerminalResult {
                variant,
                key: stable.clone(),
            },
        };

        BoundResult {
            ok: terminal(ResultVariant::Ok),

            err: match result.err.disposition {
                ErrorDisposition::Terminal => terminal(ResultVariant::Err),

                ErrorDisposition::Retryable => ResultReplay::Unstable {
                    effect: effect.clone(),
                    gap: ResultGap::ExternalErrorRetryable,
                },

                ErrorDisposition::Unspecified => ResultReplay::Unstable {
                    effect: effect.clone(),
                    gap: ResultGap::ExternalErrorDispositionUnspecified,
                },
            },
        }
    }

    /// Whether every attempt in the class takes the same arm (§16).
    fn decision_replay(
        &self,
        context: &PathContext,
        location: &StepLocation,
        decision: &Decision<'_>,
    ) -> (DecisionTaken, Result<DecisionRule, DecisionGap>) {
        match decision {
            Decision::Match { result, arm } => {
                let taken = DecisionTaken::Match {
                    location: location.clone(),
                    result: (*result).clone(),
                    arm: *arm,
                };

                // The arm taken on this path fixes the observed
                // variant, so the decision rests on that variant's
                // judgment: a stable terminal `Ok` re-selects the ok
                // arm even where the same binding's `Err` would not
                // replay (§18 rule 6).
                let replay = match context.results.get(*result) {
                    None => Err(DecisionGap::ResultNotInContext {
                        result: (*result).clone(),
                    }),

                    Some(bound) => match bound.variant(*arm) {
                        ResultReplay::Unstable { effect, gap } => {
                            Err(DecisionGap::ResultUnstable {
                                result: (*result).clone(),
                                effect: effect.clone(),
                                gap: gap.clone(),
                            })
                        }

                        ResultReplay::Stable { effect, rule } => Ok(DecisionRule::StableResult {
                            result: (*result).clone(),
                            effect: effect.clone(),
                            rule: rule.clone(),
                        }),
                    },
                };

                (taken, replay)
            }

            Decision::Branch { condition, arm } => {
                let taken = DecisionTaken::Branch {
                    location: location.clone(),
                    arm: *arm,
                };

                if !condition.is_deterministic() {
                    return (taken, Err(DecisionGap::ConditionUnspecified));
                }

                let (stable, unstable) = self.roots_stability(context, &condition.roots());

                let replay = if unstable.is_empty() {
                    Ok(DecisionRule::StableCondition { roots: stable })
                } else {
                    Err(DecisionGap::ConditionRootsUnstable { roots: unstable })
                };

                (taken, replay)
            }
        }
    }

    /// Judges one transaction's replay routes, folds the artifacts it
    /// establishes into the context, and judges each contained outbox
    /// write's instance, returning the routes and the writes.
    ///
    /// Both route legs are judged from the transaction-entry context;
    /// a commit key may not observe transaction state, and natural
    /// replay concerns the body as a whole. Artifact and outbox-write
    /// derivations see artifacts established earlier in the same
    /// transaction, in step order.
    #[allow(clippy::type_complexity)]
    pub(crate) fn apply_transaction(
        &self,
        context: &mut PathContext,
        body: &'a Transaction,
    ) -> (
        Result<Vec<StableRoot>, Vec<ReplayGap>>,
        Result<(), Vec<ReplayGap>>,
        Vec<OutboxWriteTrace<'a>>,
    ) {
        let recovery = self.recovery_route(context, body);
        let natural = self.natural_route(context, body);

        let mut writes = Vec::new();

        for inner in &body.steps {
            match inner {
                TransactionStep::EstablishTransactionOutput(establish) => {
                    let replay = self.artifact_replay(
                        &body.id,
                        &recovery,
                        &natural,
                        &establish.values,
                        context,
                    );

                    context.artifacts.insert(establish.bind.clone(), replay);
                }

                TransactionStep::EstablishEffectIntent(establish) => {
                    let replay = self.artifact_replay(
                        &body.id,
                        &recovery,
                        &natural,
                        &establish.values,
                        context,
                    );

                    context.artifacts.insert(establish.bind.clone(), replay);
                }

                TransactionStep::Transition(transition) => {
                    for intent in transition.effect_intents.values() {
                        let replay = self.artifact_replay(
                            &body.id,
                            &recovery,
                            &natural,
                            &intent.values,
                            context,
                        );

                        context.artifacts.insert(intent.bind.clone(), replay);
                    }
                }

                TransactionStep::WriteOutbox(write) => {
                    // The instance is judged where the derivation is
                    // evaluated — at this step, in the transaction
                    // context built so far, so it may consume
                    // artifacts established by earlier steps.
                    // A transaction-read root stays unstable here as
                    // everywhere.
                    writes.push(OutboxWriteTrace {
                        step: write,
                        before: context.clone(),
                        instance: self.direct_instance(context, &write.values),
                    });
                }

                _ => {}
            }
        }

        (recovery, natural, writes)
    }

    /// Route B: keyed commit deduplication over a stable key (§17).
    fn recovery_route(
        &self,
        context: &PathContext,
        transaction: &Transaction,
    ) -> Result<Vec<StableRoot>, Vec<ReplayGap>> {
        let IdempotencyGuarantee::DeduplicatedBy { key } = &transaction.idempotency else {
            return Err(vec![ReplayGap::NoKeyedCommit]);
        };

        let mut roots = Vec::new();
        let mut gaps = Vec::new();

        for component in &key.components {
            match self.root_stability(context, component) {
                Ok(root) => roots.push(root),

                Err(gap) => gaps.push(ReplayGap::CommitKeyRootUnstable {
                    root: component.clone(),
                    gap,
                }),
            }
        }

        if gaps.is_empty() {
            Ok(roots)
        } else {
            Err(gaps)
        }
    }

    /// Route A precondition: the V1 natural-replay judgment over the
    /// transaction body.
    fn natural_route(
        &self,
        context: &PathContext,
        transaction: &Transaction,
    ) -> Result<(), Vec<ReplayGap>> {
        let mut gaps = Vec::new();

        let push = |gaps: &mut Vec<ReplayGap>, gap: ReplayGap| {
            if !gaps.contains(&gap) {
                gaps.push(gap);
            }
        };

        for step in &transaction.steps {
            match step {
                TransactionStep::Transition(_) => push(&mut gaps, ReplayGap::ContainsTransition),

                TransactionStep::Insert(_) => push(&mut gaps, ReplayGap::ContainsInsert),

                TransactionStep::Delete(_) => push(&mut gaps, ReplayGap::ContainsDelete),

                TransactionStep::Write(write) => {
                    for root in write.target.predicate.roots() {
                        if let Err(gap) = self.root_stability(context, root) {
                            push(
                                &mut gaps,
                                ReplayGap::MutationTargetRootUnstable {
                                    root: root.clone(),
                                    gap,
                                },
                            );
                        }
                    }

                    match &write.values {
                        Derivation::Unspecified => {
                            push(&mut gaps, ReplayGap::MutationDerivationUnspecified);
                        }

                        Derivation::Deterministic { from } => {
                            for root in from {
                                if let Err(gap) = self.root_stability(context, root) {
                                    push(
                                        &mut gaps,
                                        ReplayGap::MutationDerivationRootUnstable {
                                            root: root.clone(),
                                            gap,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }

                // An outbox write does not block the natural route:
                // the state leg judges whether re-execution reproduces
                // the same DataObject state, and whether the repeated
                // admission it also commits is the *same logical
                // message* is exactly the effect leg's judgment (§51
                // of the outbox revision) — surfaced as an effect
                // occurrence of the same path, so a proof cannot rest
                // on natural replay while the duplicate admission goes
                // unjudged.
                TransactionStep::WriteOutbox(_) => {}

                TransactionStep::Read(_)
                | TransactionStep::Lock(_)
                | TransactionStep::EstablishEffectIntent(_)
                | TransactionStep::EstablishTransactionOutput(_) => {}
            }
        }

        if gaps.is_empty() { Ok(()) } else { Err(gaps) }
    }

    /// §17 route selection for one established artifact. Recovery
    /// needs no derivation determinism: the retained artifact is the
    /// exact original.
    fn artifact_replay(
        &self,
        transaction: &Id,
        recovery: &Result<Vec<StableRoot>, Vec<ReplayGap>>,
        natural: &Result<(), Vec<ReplayGap>>,
        derivation: &Derivation,
        context: &PathContext,
    ) -> ArtifactReplay {
        if let Ok(key) = recovery {
            return ArtifactReplay::Recovered {
                transaction: transaction.clone(),
                key: key.clone(),
            };
        }

        let mut reconstruction = natural.clone().err().unwrap_or_default();

        let mut roots = Vec::new();

        match derivation {
            Derivation::Unspecified => {
                reconstruction.push(ReplayGap::ArtifactDerivationUnspecified);
            }

            Derivation::Deterministic { from } => {
                for root in from {
                    match self.root_stability(context, root) {
                        Ok(root) => roots.push(root),

                        Err(gap) => {
                            reconstruction.push(ReplayGap::ArtifactDerivationRootUnstable {
                                root: root.clone(),
                                gap,
                            })
                        }
                    }
                }
            }
        }

        if reconstruction.is_empty() {
            ArtifactReplay::Reconstructed {
                transaction: transaction.clone(),
                derivation: roots,
            }
        } else {
            ArtifactReplay::Unavailable {
                transaction: transaction.clone(),
                recovery: recovery.clone().err().unwrap_or_default(),
                reconstruction,
            }
        }
    }
}
