//! Verification: discharging declared requirements from declared
//! facts.
//!
//! Validation (`analyzer::validation`) establishes that a model is
//! structurally coherent. Verification establishes whether the
//! requirements the model declares actually follow from its facts and
//! structure — the distinction drawn in §1 of the semantics contract.
//! A requirement is a proof obligation, not a guarantee: declaring it
//! does not assert that the operation already satisfies it (§9).
//!
//! This module is the model checker. It grew one requirement family
//! at a time and now discharges all five of §9: operation
//! serialization (`serialization`), ordering (`ordering`), result
//! replay consistency (`result_replay`), recoverability
//! (`recoverability`), and operation idempotency (`idempotency`). The
//! replay-based three share the replay engine (`replay`) — root
//! stability, natural transaction replayability, artifact replay
//! availability, effect-result and decision replay — applied path by
//! path over the operation program (`paths`). Verifiers that follow
//! effects into other operations share the trigger graph (`trigger`);
//! ordering rests on the serialization verifier's key identity and on
//! idempotency's verdicts for redelivery; idempotency and
//! recoverability rest on result replay's verdicts wherever a decision
//! or a value observes a request effect's result. Beyond §9, a
//! model-wide deadlock checker is earmarked (§27 question 9),
//! gated on the locking facts the DSL cannot yet state (§27
//! question 8); no verifier here reasons about locks.
//!
//! Two rules govern every verdict:
//!
//! 1. A verdict is proven only when the argument rests entirely on
//!    declared facts. An unknown fact cannot be used as evidence
//!    (§1.1).
//! 2. A requirement that cannot be proven is unproven, never
//!    "violated": absence of a guarantee is not evidence of a
//!    violation (§1.2). An unproven verdict records exactly which
//!    facts are missing or insufficient, preserving the distinction
//!    between an explicitly negative declaration (`unbounded`,
//!    `unordered`) and an absent one (`unspecified`, or an absent
//!    runtime declaration).
//!
//! Every proof is conditional (§1.3, §25): it holds only if the
//! concrete implementation conforms to the declarations it cites.
//! Proofs therefore carry the facts they consumed.
//!
//! Requirements are L0 obligations, but the facts that discharge them
//! may come from either layer, and most serialization and ordering
//! proofs now rest on the L1 runtime model. Every proven verdict
//! therefore carries a [`ProofScope`]: `RuntimeDependent` marks an
//! argument that holds of the declared realization and must be
//! re-examined when that realization changes. Removing L1 from a valid
//! model makes such requirements unproven — never violated, and never
//! a structural error.
//!
//! `verify` expects a model that `validation::validate` accepts. On a
//! model that fails validation it stays total and conservative:
//! lookups that fail produce unproven verdicts, never panics and
//! never unsound proofs.

mod describe;
pub mod idempotency;
pub mod ordering;
pub mod paths;
pub mod recoverability;
pub mod replay;
pub mod result_replay;
pub mod serialization;
pub mod trigger;
pub mod value_identity;

pub use describe::path_label;
pub use idempotency::{
    ConsumerCollapse, EffectRetrySafety, EffectSafety, IdempotencyCheck, IdempotencyObstacle,
    IdempotencyProof, IdempotencyVerdict, IdentityLineage, LineageFact, LineageSource,
    PathRetrySafety, ProducerRef, RetryRoute, TransactionRetrySafety,
};
pub use ordering::{
    DuplicateCoverage, DuplicateHandling, OrderingCheck, OrderingObstacle, OrderingProof,
    OrderingVerdict, OutboxPrecedence, PrecedenceSource,
};
pub use paths::{DecisionTaken, PathRef};
pub use recoverability::{
    ArtifactAvailability, PathResumption, RecoverabilityCheck, RecoverabilityNote,
    RecoverabilityObstacle, RecoverabilityProof, RecoverabilityVerdict, Resolution, RetryDriver,
    TransactionResolution,
};
pub use replay::{
    ArtifactReplay, AsyncLaunch, BoundResult, DecisionGap, DecisionReplay, DecisionRule,
    GoverningKeyDefect, InstanceGap, InstanceStability, PayloadIdentityGap, ReplayAnalysis,
    ReplayGap, ResultGap, ResultReplay, ResultStabilityRule, StabilityGap, StabilityRule,
    StableRoot, UnstableRoot,
};
pub use result_replay::{
    ResultReplayCheck, ResultReplayObstacle, ResultReplayProof, ResultReplayVerdict, ReturnedResult,
};
pub use serialization::{
    GroupingScope, InvocationLockKeyFact, KeyIdentity, MessageKeyFact, OutboxPartitionKeyFact,
    RoutingKeyFact, SerializationCheck, SerializationObstacle, SerializationProof,
    SerializationVerdict,
};
pub use trigger::{
    Consumer, EffectContract, OutboxConsumer, OutboxProducer, Producer, ProducerSite,
    TriggerGraph, collapses_duplicates, effect_contract, key_input, returns_consistently,
};
pub use value_identity::{CanonicalValuePath, canonical_value_path};

use crate::analyzer::{Diagnostic, DiagnosticCode, Severity, VerificationCode};
use crate::spec::{DeliverySemantics, Id, Input, Model};

/// Which semantic layers a successful proof consumed.
///
/// The analyzer reasons across every declared layer, so an L0
/// obligation may well be discharged from L1 facts. What the scope
/// records is the dependency: a `RuntimeDependent` proof holds of the
/// declared runtime realization, and must be re-examined when that
/// realization changes. The proof's own evidence names the exact
/// declarations consumed.
///
/// `L0Only` does not mean implementation-free. A proof resting on
/// `isolation: serializable` is L0-only, and still assumes the
/// concrete database implements serializable execution. Scope
/// identifies dependency on semantic layers, not the absence of
/// conformance assumptions (§1.3, §25).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProofScope {
    /// No explicit L1 fact was required.
    L0Only,

    /// At least one L1 fact was necessary.
    RuntimeDependent,
}

impl ProofScope {
    /// The scope of a proof combining two arguments: runtime-dependent
    /// if either leg was.
    pub fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::L0Only, Self::L0Only) => Self::L0Only,
            _ => Self::RuntimeDependent,
        }
    }

    /// The scope of a proof combining any number of arguments.
    pub fn joined(scopes: impl IntoIterator<Item = Self>) -> Self {
        scopes.into_iter().fold(Self::L0Only, Self::join)
    }
}

impl std::fmt::Display for ProofScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::L0Only => "l0_only",
            Self::RuntimeDependent => "runtime_dependent",
        })
    }
}

/// Which semantic layer holds the facts an unproven obligation is
/// waiting on.
///
/// The dual of [`ProofScope`]: scope records the layers a proof
/// *consumed*, remedy records the layer a missing proof *needs*. It
/// exists so a coordinator can tell an obligation blocked on the
/// runtime realization from one blocked on the application model,
/// without parsing prose.
///
/// This is a routing hint, not a verdict. It says where the next
/// declaration must go, not that adding one there will close the
/// proof.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RemedyLayer {
    /// At least one obstacle names an L0 fact: the operation's
    /// program, its interface, or the requirement itself. An L1
    /// declaration alone cannot discharge the obligation.
    Application,

    /// Every obstacle names an L1 fact: grouping, ordering, routing,
    /// member assignment, or pool concurrency.
    Runtime,
}

impl RemedyLayer {
    /// The remedy for an obligation blocked by several obstacles.
    ///
    /// `Runtime` only when every obstacle is a runtime one. Obstacles
    /// are conjunctive — each must clear for the proof to close — so a
    /// single application obstacle means topology work alone cannot
    /// finish, and the application fix is what to ask for first. Once
    /// it lands the obligation re-reports, and what remains routes to
    /// the runtime.
    pub fn joined(layers: impl IntoIterator<Item = Self>) -> Option<Self> {
        layers
            .into_iter()
            .reduce(|a, b| match (a, b) {
                (Self::Runtime, Self::Runtime) => Self::Runtime,
                _ => Self::Application,
            })
    }
}

impl std::fmt::Display for RemedyLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Application => "application",
            Self::Runtime => "runtime",
        })
    }
}

/// A model-wide observation raised next to the verdicts. Not an
/// obligation — no declaration asks for it — but a gap no verdict
/// would otherwise point out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelNote {
    /// A subscription admits duplicate deliveries — at-least-once or
    /// unspecified delivery — and its operation declares no
    /// idempotency requirement keyed from it. The topic contract
    /// admits the duplicate invocation and its safety is nobody's
    /// obligation, so the work it repeats is checked by nothing.
    DuplicateDeliveryUnchecked {
        operation: Id,
        input: Id,
        topic: Id,
        delivery: DeliverySemantics,
    },

    /// The outbox counterpart: intrinsic durable re-drive admits
    /// duplicate and overlapping consumption attempts for every
    /// outbox input — there is no delivery fact that could exclude
    /// them — and the operation declares no idempotency requirement
    /// keyed from the input, so nothing declares the repeated work
    /// safe.
    DuplicateOutboxDeliveryUnchecked {
        operation: Id,
        input: Id,
        outbox: Id,
    },

    /// A `present` condition over a path with no optional segment:
    /// vacuously true, so redundant — and its never-taken arm is
    /// still an admitted path, since conditions never prune paths.
    /// A warning, never an error: the predicate has a well-defined
    /// meaning (§16).
    RedundantPresenceCheck {
        operation: Id,
        location: crate::spec::StepLocation,
        root: crate::spec::ValueRef,
    },
}

impl ModelNote {
    pub fn subject(&self) -> Option<Id> {
        match self {
            Self::DuplicateDeliveryUnchecked { input, .. }
            | Self::DuplicateOutboxDeliveryUnchecked { input, .. } => Some(input.clone()),

            Self::RedundantPresenceCheck { operation, .. } => Some(operation.clone()),
        }
    }

    pub fn message(&self) -> String {
        let admits = |delivery: &DeliverySemantics| match delivery {
            DeliverySemantics::AtLeastOnce => {
                "declares at-least-once delivery, so a logical message may invoke it more than once"
            }
            _ => "declares no delivery fact, so duplicate invocations cannot be excluded",
        };

        match self {
            Self::DuplicateDeliveryUnchecked {
                operation,
                input,
                topic,
                delivery,
            } => {
                format!(
                    "`{input}` of `{operation}` subscribes to `{topic}` and {}; the \
                     operation declares no idempotency requirement keyed from that input, so \
                     the work a duplicate delivery repeats is checked by nothing.",
                    admits(delivery)
                )
            }

            Self::DuplicateOutboxDeliveryUnchecked {
                operation,
                input,
                outbox,
            } => {
                format!(
                    "`{input}` of `{operation}` consumes outbox `{outbox}`, whose \
                     intrinsic durable re-drive admits duplicate and overlapping \
                     consumption attempts; the operation declares no idempotency \
                     requirement keyed from that input, so the work a repeated attempt \
                     performs is checked by nothing."
                )
            }

            Self::RedundantPresenceCheck {
                operation,
                location,
                root,
            } => {
                format!(
                    "Program step `{location}` of `{operation}` tests `present` on a path \
                     with no optional segment: the condition is vacuously true. The \
                     never-taken arm remains an admitted path — conditions never prune \
                     paths — so delete the dead arm rather than carrying phantom \
                     obligations through it. Root: {}.",
                    root.source.id()
                )
            }
        }
    }

    pub fn diagnostic(&self) -> Diagnostic {
        let code = match self {
            Self::DuplicateDeliveryUnchecked { .. }
            | Self::DuplicateOutboxDeliveryUnchecked { .. } => {
                VerificationCode::DuplicateDeliveryUnchecked
            }

            Self::RedundantPresenceCheck { .. } => VerificationCode::RedundantPresenceCheck,
        };

        Diagnostic {
            code: DiagnosticCode::Verification(code),
            severity: Severity::Warning,
            subject: self.subject(),
            message: self.message(),
            evidence: Vec::new(),
        }
    }
}

/// The model-wide notes: every subscription that admits duplicate
/// deliveries without an idempotency requirement keyed from it.
pub fn notes(model: &Model) -> Vec<ModelNote> {
    let mut notes = Vec::new();

    for redundant in crate::analyzer::validation::redundant_presence_checks(model) {
        notes.push(ModelNote::RedundantPresenceCheck {
            operation: redundant.operation,
            location: redundant.location,
            root: redundant.root,
        });
    }

    for (operation_id, operation) in &model.operations {
        for (input_id, input) in &operation.inputs {
            match input {
                Input::Subscription(subscription) => {
                    let delivery = model.delivery(operation_id, input_id);

                    if delivery == DeliverySemantics::AtMostOnce {
                        continue;
                    }

                    if !collapses_duplicates(operation, input_id) {
                        notes.push(ModelNote::DuplicateDeliveryUnchecked {
                            operation: operation_id.clone(),
                            input: input_id.clone(),
                            topic: subscription.topic.clone(),
                            delivery,
                        });
                    }
                }

                // Intrinsic re-drive admits duplicates for every
                // outbox input; no delivery fact can exclude them.
                Input::Outbox(declared) => {
                    if !collapses_duplicates(operation, input_id) {
                        notes.push(ModelNote::DuplicateOutboxDeliveryUnchecked {
                            operation: operation_id.clone(),
                            input: input_id.clone(),
                            outbox: declared.outbox.clone(),
                        });
                    }
                }

                Input::Request(_) => {}
            }
        }
    }

    notes
}

/// Verdicts for every requirement the model declares, in deterministic
/// model order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    pub serialization: Vec<SerializationCheck>,
    pub ordering: Vec<OrderingCheck>,
    pub idempotency: Vec<IdempotencyCheck>,
    pub result_replay: Vec<ResultReplayCheck>,
    pub recoverability: Vec<RecoverabilityCheck>,

    /// Model-wide notes, raised as warnings.
    #[serde(default)]
    pub notes: Vec<ModelNote>,
}

impl VerificationReport {
    /// Diagnostics for the requirements the model does not establish,
    /// and warnings raised next to proven ones.
    ///
    /// A proven requirement's argument lives in its structured
    /// verdict; it produces a diagnostic only for a note worth
    /// raising alongside, such as guaranteed retries whose safety no
    /// requirement declares.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.serialization
            .iter()
            .filter_map(SerializationCheck::diagnostic)
            .chain(self.ordering.iter().filter_map(OrderingCheck::diagnostic))
            .chain(
                self.idempotency
                    .iter()
                    .filter_map(IdempotencyCheck::diagnostic),
            )
            .chain(
                self.result_replay
                    .iter()
                    .filter_map(ResultReplayCheck::diagnostic),
            )
            .chain(
                self.recoverability
                    .iter()
                    .filter_map(RecoverabilityCheck::diagnostic),
            )
            .chain(
                self.recoverability
                    .iter()
                    .flat_map(RecoverabilityCheck::note_diagnostics),
            )
            .chain(self.notes.iter().map(ModelNote::diagnostic))
            .collect()
    }

    pub fn all_proven(&self) -> bool {
        self.serialization
            .iter()
            .all(|entry| matches!(entry.verdict, SerializationVerdict::Proven { .. }))
            && self
                .ordering
                .iter()
                .all(|entry| matches!(entry.verdict, OrderingVerdict::Proven { .. }))
            && self
                .idempotency
                .iter()
                .all(|entry| matches!(entry.verdict, IdempotencyVerdict::Proven { .. }))
            && self
                .result_replay
                .iter()
                .all(|entry| matches!(entry.verdict, ResultReplayVerdict::Proven { .. }))
            && self
                .recoverability
                .iter()
                .all(|entry| matches!(entry.verdict, RecoverabilityVerdict::Proven { .. }))
    }
}

/// Verifies every declared requirement the checker currently supports.
///
/// Result replay comes first: it depends on nothing but itself, and
/// its proven set is what idempotency and recoverability consult when
/// a decision or a value rests on a request effect's result.
pub fn verify(model: &Model) -> VerificationReport {
    let result_replay = result_replay::check(model);
    let consistent = result_replay::consistent_set(&result_replay);

    let idempotency = idempotency::check(model, &consistent);
    let ordering = ordering::check(model, &idempotency);

    VerificationReport {
        serialization: serialization::check(model),
        ordering,
        idempotency,
        result_replay,
        recoverability: recoverability::check(model, &consistent),
        notes: notes(model),
    }
}
