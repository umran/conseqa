//! Verification of recoverability requirements (§9 of the semantics
//! contract).
//!
//! > The logical invocation identified by the key must reach a valid
//! > terminal of the operation program — `return` or `complete` —
//! > after any modeled interruption.
//!
//! Recoverability is a progress obligation, deliberately separate
//! from idempotency's safety obligation. V1 discharges it by
//! **same-path continuation**: for every prefix at which an attempt
//! may fail, re-driving the same path of the program from its first
//! step reaches the path's terminal. This is a sufficient route that
//! does not prejudge §27 question 7 (which *other* paths a
//! resumed attempt may take). A decision a retry is not established
//! to take the same way is no obstacle to progress: whichever
//! admitted path it then follows is analyzed on its own, and the
//! difference in work is idempotency's concern.
//!
//! The population is the attempt class of the requirement's governing
//! key (§12), and the analyzed paths are those an invocation of the
//! triggering input can complete: paths ending at `complete`, or at a
//! `return` for that input.
//!
//! Per admitted path, three conditions establish the continuation:
//!
//! 1. every committed transaction resolves on re-encounter — by keyed
//!    commit over a stable key, or by natural replay — except a
//!    transaction that is the final step of a path ending at
//!    `complete`, after which no failing prefix exists;
//! 2. every artifact a later step consumes is replay-available by
//!    route A or route B of §17, judged by the replay engine —
//!    including intents the path executes (which must be established
//!    at all), transaction outputs referenced by later transaction
//!    bodies or effect derivations, and the outputs the terminal
//!    result is derived from; references within the establishing
//!    transaction itself are exempt by atomicity, and a commit key is
//!    judged by condition 1, not double-counted here;
//! 3. nothing else blocks, by construction: transactions are atomic,
//!    effect executions can be re-attempted (their duplicate-safety
//!    is idempotency's concern) and re-observed, and consumption does
//!    not remove an artifact from the context.
//!
//! Re-executing a committed transaction that resolves by neither
//! route is not a continuation of the same logical invocation, so V1
//! refuses to discharge a progress obligation through it.
//!
//! `completion: guaranteed` additionally requires a modeled retry
//! driver on the triggering input: `at_least_once` delivery on the
//! subscription, or a modeled caller declaring a `may_repeat` request
//! effect targeting the input — whether among an operation's effects
//! or as a state-machine transition side effect (§22). Driver facts
//! are duplicate-delivery facts, not bounded-liveness facts; the
//! proof is conditional on the abstraction genuinely re-driving until
//! success (§1.3).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{
    CompletionRequirement, DeliverySemantics, Effect, Id, IdempotencyKey, Input, Model, Operation,
    RetrySemantics, TransactionStep, TransitionSideEffect, ValueSource,
};

use super::ProofScope;
use super::describe::{describe_path, gap_sentences, governing_key_evidence};
use super::paths::{Path, PathRef, Terminal, paths};
use super::replay::{
    ArtifactReplay, EffectSite, GoverningKeyDefect, ReplayAnalysis, ReplayGap, StableRoot,
    TracedStep,
};
use super::trigger::collapses_duplicates;

/// The verdict for one declared recoverability requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverabilityCheck {
    pub operation: Id,

    /// Index into `operation.requirements.recoverability`.
    pub requirement: usize,

    /// The governing key, copied so the check is self-contained.
    pub key: IdempotencyKey,

    pub completion: CompletionRequirement,

    pub verdict: RecoverabilityVerdict,

    /// Facts that do not bear on the verdict but belong next to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<RecoverabilityNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoverabilityVerdict {
    Proven {
        proof: RecoverabilityProof,
        scope: ProofScope,
    },
    Unproven {
        obstacles: Vec<RecoverabilityObstacle>,
    },
}

impl RecoverabilityVerdict {
    fn proven(proof: RecoverabilityProof) -> Self {
        Self::Proven {
            scope: proof.scope(),
            proof,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoverabilityProof {
    /// The triggering subscription admits no message schemas, so no
    /// attempt can bear the key.
    NoAdmittedInvocations { input: Id },

    /// Every admitted path resumes from every failing prefix.
    Resumable { paths: Vec<PathResumption> },

    /// Resumable, and a modeled driver re-drives interrupted
    /// invocations.
    Guaranteed {
        driver: RetryDriver,
        paths: Vec<PathResumption>,
    },
}

impl RecoverabilityProof {
    pub fn scope(&self) -> ProofScope {
        match self {
            // Resumability rests entirely on the program: transaction
            // re-encounter resolution and artifact replay availability
            // are L0 facts about the abstract machine.
            Self::NoAdmittedInvocations { .. } | Self::Resumable { .. } => ProofScope::L0Only,

            Self::Guaranteed { driver, .. } => driver.scope(),
        }
    }
}

impl RetryDriver {
    fn scope(&self) -> ProofScope {
        match self {
            // Delivery is a realization fact.
            Self::AtLeastOnceDelivery { .. } => ProofScope::RuntimeDependent,

            // A caller's `retry: may_repeat` is an L0 guarantee about
            // the request effect.
            Self::InboundRepeatableRequest { .. }
            | Self::InboundRepeatableTransitionEffect { .. } => ProofScope::L0Only,
        }
    }
}

/// The same-path continuation argument for one admitted path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathResumption {
    pub path: PathRef,

    /// Re-encounter resolution per transaction step, in path order.
    pub transactions: Vec<TransactionResolution>,

    /// Every artifact a later step consumes, with its replay route.
    pub artifacts: Vec<ArtifactAvailability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionResolution {
    pub transaction: Id,
    pub resolution: Resolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    /// Route B: the re-encounter resolves the single keyed commit,
    /// whose key is stable through the cited rules.
    KeyedCommit { key: Vec<StableRoot> },

    /// Route A: the re-encounter safely re-executes the naturally
    /// replayable body.
    NaturalReplay,

    /// The transaction is the final step of a path ending at
    /// `complete`: no failing prefix follows its commit, so it is
    /// never re-encountered by a resumption.
    TerminalStep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAvailability {
    pub artifact: Id,
    pub replay: ArtifactReplay,
}

/// The modeled fact that re-drives interrupted invocations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryDriver {
    /// The triggering subscription declares at-least-once delivery.
    AtLeastOnceDelivery { input: Id, topic: Id },

    /// A modeled caller declares a repeatable request effect
    /// targeting the triggering input.
    InboundRepeatableRequest { operation: Id, effect: Id },

    /// A state-machine transition side effect is a repeatable request
    /// targeting the triggering input.
    InboundRepeatableTransitionEffect {
        machine: Id,
        transition: Id,
        effect: Id,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoverabilityObstacle {
    /// The governing key defines no pre-execution equivalence class
    /// (§12).
    GoverningKeyInadmissible { defect: GoverningKeyDefect },

    /// No path of the program is completable by invocations of the
    /// triggering input.
    NoAdmittedPath { input: Id },

    /// The path falls off the end of the program without a terminal.
    /// Validation rejects the shape; verification records it.
    PathNotTerminated { path: PathRef },

    /// A committed transaction the resumption re-encounters resolves
    /// by neither route.
    TransactionNotResolvable {
        path: PathRef,
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },

    /// A later step consumes an artifact no earlier step establishes.
    ArtifactNotEstablished {
        path: PathRef,
        artifact: Id,
        consumer: Id,
    },

    /// A consumed artifact is replay-available through neither route.
    ArtifactNotReplayAvailable {
        path: PathRef,
        artifact: Id,
        transaction: Id,
        recovery: Vec<ReplayGap>,
        reconstruction: Vec<ReplayGap>,
    },

    /// Completion is `guaranteed`, but no modeled driver re-drives
    /// interrupted invocations of the triggering input.
    NoModeledRetryDriver {
        input: Id,

        /// The subscription's declared delivery, or `None` for a
        /// request input with no modeled repeatable caller.
        delivery: Option<DeliverySemantics>,
    },
}

/// A fact that does not bear on a verdict but belongs next to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoverabilityNote {
    /// Completion is guaranteed by a retry driver, so repeated
    /// attempts are expected — yet the operation declares no
    /// idempotency requirement keyed from the triggering input, so
    /// the safety of those retries is undeclared and unverified.
    /// Recoverability discharges progress only (§9); this is where a
    /// reader would look for the safety half.
    RetrySafetyUndeclared { input: Id },
}

/// Checks every recoverability requirement declared by the model.
/// `consistent` names the `(operation, input)` pairs whose result
/// replay is proven, which a commit key or mutation resting on a
/// request effect's result depends on.
pub fn check(model: &Model, consistent: &BTreeSet<(Id, Id)>) -> Vec<RecoverabilityCheck> {
    let mut checks = Vec::new();

    for (operation_id, operation) in &model.operations {
        for (index, requirement) in operation.requirements.recoverability.iter().enumerate() {
            let (verdict, notes) = check_requirement(
                model,
                operation_id,
                operation,
                &requirement.key,
                requirement.completion,
                consistent,
            );

            checks.push(RecoverabilityCheck {
                operation: operation_id.clone(),
                requirement: index,
                key: requirement.key.clone(),
                completion: requirement.completion,
                verdict,
                notes,
            });
        }
    }

    checks
}

fn check_requirement(
    model: &Model,
    operation_id: &Id,
    operation: &Operation,
    key: &IdempotencyKey,
    completion: CompletionRequirement,
    consistent: &BTreeSet<(Id, Id)>,
) -> (RecoverabilityVerdict, Vec<RecoverabilityNote>) {
    let analysis = match ReplayAnalysis::new(model, operation, key, consistent) {
        Ok(analysis) => analysis,

        Err(defect) => {
            return (
                RecoverabilityVerdict::Unproven {
                    obstacles: vec![RecoverabilityObstacle::GoverningKeyInadmissible { defect }],
                },
                Vec::new(),
            );
        }
    };

    if analysis.admits_no_attempts() {
        return (
            RecoverabilityVerdict::proven(RecoverabilityProof::NoAdmittedInvocations {
                input: analysis.input().clone(),
            }),
            Vec::new(),
        );
    }

    // Paths an invocation of the triggering input can complete.
    let all = paths(&operation.program);

    let admitted: Vec<&Path<'_>> = all
        .iter()
        .filter(|path| path.admitted_for(analysis.input()))
        .collect();

    let mut obstacles = Vec::new();

    if admitted.is_empty() {
        obstacles.push(RecoverabilityObstacle::NoAdmittedPath {
            input: analysis.input().clone(),
        });
    }

    let mut resumptions = Vec::new();

    for path in admitted {
        if let Some(resumption) = analyze_path(&analysis, path, &mut obstacles) {
            resumptions.push(resumption);
        }
    }

    let driver = match completion {
        CompletionRequirement::Resumable => None,

        CompletionRequirement::Guaranteed => {
            match find_driver(model, operation_id, operation, analysis.input()) {
                Ok(driver) => Some(driver),

                Err(delivery) => {
                    obstacles.push(RecoverabilityObstacle::NoModeledRetryDriver {
                        input: analysis.input().clone(),
                        delivery,
                    });

                    None
                }
            }
        }
    };

    if !obstacles.is_empty() {
        return (
            RecoverabilityVerdict::Unproven {
                obstacles: super::idempotency::dedupe(obstacles, RecoverabilityObstacle::site),
            },
            Vec::new(),
        );
    }

    // A driver makes retries expected. Recoverability says nothing
    // about their safety, so when no idempotency requirement keyed
    // from the triggering input does either, say so next to the proof.
    let mut notes = Vec::new();

    if driver.is_some() && !collapses_duplicates(operation, analysis.input()) {
        notes.push(RecoverabilityNote::RetrySafetyUndeclared {
            input: analysis.input().clone(),
        });
    }

    (
        RecoverabilityVerdict::proven(match driver {
            None => RecoverabilityProof::Resumable { paths: resumptions },
            Some(driver) => RecoverabilityProof::Guaranteed {
                driver,
                paths: resumptions,
            },
        }),
        notes,
    )
}

/// The same-path continuation analysis for one admitted path. Returns
/// the resumption argument, or `None` after pushing the path's
/// obstacles.
fn analyze_path(
    analysis: &ReplayAnalysis<'_>,
    path: &Path<'_>,
    obstacles: &mut Vec<RecoverabilityObstacle>,
) -> Option<PathResumption> {
    let before = obstacles.len();

    let reference = path.reference();
    let trace = analysis.trace(path);

    let mut transactions = Vec::new();
    let mut artifacts = Vec::new();
    let mut reported: BTreeSet<Id> = BTreeSet::new();

    let last = trace.steps.len().saturating_sub(1);

    // A transaction that is the final step of a path ending at
    // `complete` has no failing prefix after it. A `return` follows its
    // last transaction: the result must still be constructed and
    // returned, so every transaction before it may be re-encountered.
    let completes = matches!(trace.terminal, Terminal::Complete { .. });

    for (position, step) in trace.steps.iter().enumerate() {
        match step {
            TracedStep::Transaction {
                transaction,
                before: context,
                recovery,
                natural,
                ..
            } => {
                // Cross-step consumption is judged against the
                // context before this transaction; same-transaction
                // references are exempt by atomicity.
                let mut established_here: BTreeSet<&Id> = BTreeSet::new();

                for inner in &transaction.steps {
                    for root in inner.roots() {
                        let ValueSource::TransactionOutput(artifact) = &root.source else {
                            continue;
                        };

                        if !established_here.contains(artifact) {
                            require_artifact(
                                &reference,
                                &transaction.id,
                                artifact,
                                &context.artifacts,
                                &mut artifacts,
                                &mut reported,
                                obstacles,
                            );
                        }
                    }

                    if let TransactionStep::EstablishTransactionOutput(establish) = inner {
                        established_here.insert(&establish.bind);
                    }
                }

                if position < last || !completes {
                    let resolution = match (recovery, natural) {
                        (Ok(key), _) => Some(Resolution::KeyedCommit { key: key.clone() }),

                        (Err(_), Ok(())) => Some(Resolution::NaturalReplay),

                        (Err(recovery), Err(reconstruction)) => {
                            obstacles.push(RecoverabilityObstacle::TransactionNotResolvable {
                                path: reference.clone(),
                                transaction: transaction.id.clone(),
                                recovery: recovery.clone(),
                                reconstruction: reconstruction.clone(),
                            });

                            None
                        }
                    };

                    if let Some(resolution) = resolution {
                        transactions.push(TransactionResolution {
                            transaction: transaction.id.clone(),
                            resolution,
                        });
                    }
                } else {
                    transactions.push(TransactionResolution {
                        transaction: transaction.id.clone(),
                        resolution: Resolution::TerminalStep,
                    });
                }
            }

            TracedStep::Effect {
                site,
                before: context,
                ..
            } => match site {
                EffectSite::Direct { effect, values } => {
                    for root in values.roots() {
                        if let ValueSource::TransactionOutput(artifact) = &root.source {
                            require_artifact(
                                &reference,
                                effect,
                                artifact,
                                &context.artifacts,
                                &mut artifacts,
                                &mut reported,
                                obstacles,
                            );
                        }
                    }
                }

                EffectSite::Intent { intent, .. } => {
                    require_artifact(
                        &reference,
                        intent,
                        intent,
                        &context.artifacts,
                        &mut artifacts,
                        &mut reported,
                        obstacles,
                    );
                }
            },

            TracedStep::Decision { .. } => {}
        }
    }

    match &trace.terminal {
        Terminal::Return {
            request, outcome, ..
        } => {
            for root in outcome.values().roots() {
                if let ValueSource::TransactionOutput(artifact) = &root.source {
                    require_artifact(
                        &reference,
                        request,
                        artifact,
                        &trace.end.artifacts,
                        &mut artifacts,
                        &mut reported,
                        obstacles,
                    );
                }
            }
        }

        Terminal::Complete { .. } => {}

        Terminal::None => obstacles.push(RecoverabilityObstacle::PathNotTerminated {
            path: reference.clone(),
        }),
    }

    (obstacles.len() == before).then_some(PathResumption {
        path: reference,
        transactions,
        artifacts,
    })
}

/// Records a consumed artifact's availability, or the obstacle
/// explaining why the resumption cannot supply it. Each artifact is
/// judged once per path.
fn require_artifact(
    path: &PathRef,
    consumer: &Id,
    artifact: &Id,
    context: &BTreeMap<Id, ArtifactReplay>,
    artifacts: &mut Vec<ArtifactAvailability>,
    reported: &mut BTreeSet<Id>,
    obstacles: &mut Vec<RecoverabilityObstacle>,
) {
    if !reported.insert(artifact.clone()) {
        return;
    }

    match context.get(artifact) {
        None => obstacles.push(RecoverabilityObstacle::ArtifactNotEstablished {
            path: path.clone(),
            artifact: artifact.clone(),
            consumer: consumer.clone(),
        }),

        Some(ArtifactReplay::Unavailable {
            transaction,
            recovery,
            reconstruction,
        }) => obstacles.push(RecoverabilityObstacle::ArtifactNotReplayAvailable {
            path: path.clone(),
            artifact: artifact.clone(),
            transaction: transaction.clone(),
            recovery: recovery.clone(),
            reconstruction: reconstruction.clone(),
        }),

        Some(replay) => artifacts.push(ArtifactAvailability {
            artifact: artifact.clone(),
            replay: replay.clone(),
        }),
    }
}

/// The modeled retry driver for the triggering input, or the declared
/// delivery fact that fails to be one.
fn find_driver(
    model: &Model,
    operation_id: &Id,
    operation: &Operation,
    input: &Id,
) -> Result<RetryDriver, Option<DeliverySemantics>> {
    match operation.inputs.get(input) {
        Some(Input::Subscription(subscription)) => {
            let delivery = model.delivery(operation_id, input);

            if delivery == DeliverySemantics::AtLeastOnce {
                Ok(RetryDriver::AtLeastOnceDelivery {
                    input: input.clone(),
                    topic: subscription.topic.clone(),
                })
            } else {
                Err(Some(delivery))
            }
        }

        Some(Input::Request(_)) => {
            for (caller_id, caller) in &model.operations {
                for (effect_id, effect) in caller.program.effect_declarations() {
                    if let Effect::Request(request) = effect
                        && &request.target.operation == operation_id
                        && &request.target.input == input
                        && request.retry == RetrySemantics::MayRepeat
                    {
                        return Ok(RetryDriver::InboundRepeatableRequest {
                            operation: caller_id.clone(),
                            effect: effect_id.clone(),
                        });
                    }
                }
            }

            for (machine_id, machine) in &model.state_machines {
                for (transition_id, transition) in &machine.transitions {
                    for (effect_id, effect) in &transition.side_effects {
                        if let TransitionSideEffect::Request(request) = effect
                            && &request.target.operation == operation_id
                            && &request.target.input == input
                            && request.retry == RetrySemantics::MayRepeat
                        {
                            return Ok(RetryDriver::InboundRepeatableTransitionEffect {
                                machine: machine_id.clone(),
                                transition: transition_id.clone(),
                                effect: effect_id.clone(),
                            });
                        }
                    }
                }
            }

            Err(None)
        }

        None => Err(None),
    }
}

impl RecoverabilityNote {
    pub fn evidence(&self) -> Evidence {
        match self {
            Self::RetrySafetyUndeclared { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "Completion is guaranteed by re-driving invocations of \
                     `{input}`, so repeated attempts are expected, but no \
                     idempotency requirement keyed from `{input}` declares them \
                     safe; recoverability establishes progress only."
                ),
            },
        }
    }

    fn diagnostic(&self, operation: &Id, requirement: usize) -> Diagnostic {
        match self {
            Self::RetrySafetyUndeclared { input } => Diagnostic {
                code: DiagnosticCode::Verification(
                    VerificationCode::RecoverabilityRetrySafetyUndeclared,
                ),
                severity: Severity::Warning,
                subject: Some(operation.clone()),
                message: format!(
                    "Recoverability requirement {requirement} of `{operation}` \
                     guarantees completion through retries, but `{operation}` \
                     declares no idempotency requirement keyed from `{input}`: \
                     the retries are expected, and their safety is undeclared \
                     and unverified."
                ),
                evidence: vec![self.evidence()],
            },
        }
    }
}

impl RecoverabilityCheck {
    /// Warnings worth raising next to a proven verdict.
    pub fn note_diagnostics(&self) -> Vec<Diagnostic> {
        self.notes
            .iter()
            .map(|note| note.diagnostic(&self.operation, self.requirement))
            .collect()
    }

    /// The diagnostic for an unproven requirement; a proven one
    /// produces none.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let RecoverabilityVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let requirement = self.requirement;

        let completion = match self.completion {
            CompletionRequirement::Resumable => "resumable",
            CompletionRequirement::Guaranteed => "guaranteed",
        };

        Some(Diagnostic {
            code: DiagnosticCode::Verification(VerificationCode::RecoverabilityUnproven),
            severity: Severity::Unknown,
            subject: Some(operation.clone()),
            message: format!(
                "Recoverability requirement {requirement} of `{operation}` \
                 (`{completion}`) is not established: interrupted invocations \
                 sharing the declared key are not proven to reach a terminal of \
                 the operation program."
            ),
            evidence: obstacles
                .iter()
                .map(RecoverabilityObstacle::evidence)
                .collect(),
        })
    }
}

impl RecoverabilityObstacle {
    /// The obstacle with its path forgotten, so the same fact on two
    /// paths compares equal. Whether a path terminates is the path's
    /// own fact and is kept apart.
    fn site(&self) -> Self {
        let mut site = self.clone();

        match &mut site {
            Self::GoverningKeyInadmissible { .. }
            | Self::NoAdmittedPath { .. }
            | Self::PathNotTerminated { .. }
            | Self::NoModeledRetryDriver { .. } => {}

            Self::TransactionNotResolvable { path, .. }
            | Self::ArtifactNotEstablished { path, .. }
            | Self::ArtifactNotReplayAvailable { path, .. } => *path = PathRef::default(),
        }

        site
    }

    fn evidence(&self) -> Evidence {
        match self {
            Self::GoverningKeyInadmissible { defect } => governing_key_evidence(defect),

            Self::NoAdmittedPath { input } => Evidence {
                subject: Some(input.clone()),
                message: format!(
                    "No path of the program is completable by invocations of \
                     `{input}`: every path returns another input's result."
                ),
            },

            Self::PathNotTerminated { path } => Evidence {
                subject: None,
                message: format!(
                    "{} falls off the end of the program without reaching a \
                     `return` or `complete` terminal.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::TransactionNotResolvable {
                path,
                transaction,
                recovery,
                reconstruction,
            } => Evidence {
                subject: Some(transaction.clone()),
                message: format!(
                    "Resuming {} re-encounters `{transaction}` after it may have \
                     committed, but the commit resolves by neither route. \
                     Recovery: {}. Reconstruction: {}.",
                    describe_path(path),
                    gap_sentences(recovery),
                    gap_sentences(reconstruction),
                ),
            },

            Self::ArtifactNotEstablished {
                path,
                artifact,
                consumer,
            } => Evidence {
                subject: Some(artifact.clone()),
                message: format!(
                    "{} consumes artifact `{artifact}` at `{consumer}`, but no \
                     earlier step establishes it.",
                    capitalize(&describe_path(path))
                ),
            },

            Self::ArtifactNotReplayAvailable {
                path,
                artifact,
                transaction,
                recovery,
                reconstruction,
            } => Evidence {
                subject: Some(transaction.clone()),
                message: format!(
                    "{} consumes artifact `{artifact}`, established by \
                     `{transaction}`, but a resumption can supply it through \
                     neither route. Recovery: {}. Reconstruction: {}.",
                    capitalize(&describe_path(path)),
                    gap_sentences(recovery),
                    gap_sentences(reconstruction),
                ),
            },

            Self::NoModeledRetryDriver { input, delivery } => Evidence {
                subject: Some(input.clone()),
                message: match delivery {
                    Some(DeliverySemantics::AtMostOnce) => format!(
                        "Completion is guaranteed, but subscription `{input}` \
                         declares `at_most_once` delivery: interrupted \
                         invocations are not redelivered."
                    ),

                    Some(_) => format!(
                        "Completion is guaranteed, but subscription `{input}` \
                         declares no delivery fact that re-drives interrupted \
                         invocations."
                    ),

                    None => format!(
                        "Completion is guaranteed, but request input `{input}` \
                         has no modeled caller declaring a `may_repeat` request \
                         effect, so nothing in the model re-drives interrupted \
                         invocations."
                    ),
                },
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
