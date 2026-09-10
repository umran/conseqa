//! Verification of result replay consistency (§9, §15, §16 of the
//! semantics contract).
//!
//! When an idempotency requirement declares `result: replay_consistent`:
//!
//! > Repeated admitted attempts in the same logical idempotency class
//! > that return a request result must return the same result variant
//! > and a replay-equivalent payload.
//!
//! The population is the attempt class defined by the requirement's
//! governing key (§12). The obligation constrains the results those
//! attempts return: the admitted paths of the program that end at a
//! `return` for the triggering request input. A key triggered by a
//! subscription, or an operation whose admitted paths return no result
//! for that input, leaves nothing to stabilize and is vacuously
//! consistent.
//!
//! For each returning path the proof is control-path replay plus
//! ordinary provenance (§9, §16). Every decision on the path must
//! replay (§16): a class then follows one path to one terminal, which
//! fixes the variant. The terminal derivation must be
//! replay-deterministic in the context at the terminal — deterministic
//! over roots the §18 rules make stable, including transaction outputs
//! by route A or B and effect results whose targets prove their own
//! consistency. No privileged result artifact is involved.
//!
//! That last premise names other operations' verdicts, so the checks
//! are computed as a greatest fixpoint over the replay-consistent
//! requirements, exactly as idempotency's are
//! (§9): a cycle of requests whose
//! members each pass their local checks proves, and the proof is
//! marked coinductive. The argument is the same minimal-counterexample
//! one — a differing observation is a violation at strictly shorter
//! causal distance — and uses a downstream requirement only for what
//! its local check provides.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::analyzer::{Diagnostic, DiagnosticCode, Evidence, Severity, VerificationCode};
use crate::spec::{
    Derivation, Id, IdempotencyKey, Model, Operation, ResultReplayRequirement, ResultVariant,
};

use super::ProofScope;
use super::describe::{
    decision_gap_sentence, describe_decision, describe_path, governing_key_evidence, unstable_roots,
};
use super::paths::{DecisionTaken, PathRef, paths};
use super::replay::{
    DecisionGap, DecisionReplay, GoverningKeyDefect, ReplayAnalysis, StableRoot, UnstableRoot,
};
use super::trigger::key_input;

/// The verdict for one declared result-replay obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultReplayCheck {
    pub operation: Id,

    /// Index into `operation.requirements.idempotency`.
    pub requirement: usize,

    /// The governing key, copied so the check is self-contained.
    pub key: IdempotencyKey,

    pub verdict: ResultReplayVerdict,

    /// Set when the proof holds only together with the proofs of the
    /// requirements it reaches through request effects, and theirs
    /// hold only with it: the greatest fixpoint admits such a cycle.
    #[serde(default)]
    pub coinductive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultReplayVerdict {
    Proven {
        proof: ResultReplayProof,
        scope: ProofScope,
    },
    Unproven {
        obstacles: Vec<ResultReplayObstacle>,
    },
}

impl ResultReplayVerdict {
    fn proven(proof: ResultReplayProof) -> Self {
        Self::Proven {
            scope: proof.scope(),
            proof,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultReplayProof {
    /// The triggering subscription admits no message schemas, so no
    /// attempt can bear the key.
    NoAdmittedInvocations { input: Id },

    /// No admitted path returns a result for the triggering input;
    /// there is no result to stabilize.
    NoReturnedResult { input: Id },

    /// Every path returning a result for the triggering input replays
    /// its decisions, so each class reaches one terminal, whose payload
    /// is replay-deterministic over the cited roots.
    ClassFixedResult { returns: Vec<ReturnedResult> },
}

impl ResultReplayProof {
    /// Result replay is decided entirely from the program: which
    /// terminal a class reaches and what the returned payload is
    /// derived from. No route consults a runtime fact, so every
    /// result-replay proof is L0-only.
    pub fn scope(&self) -> ProofScope {
        ProofScope::L0Only
    }
}

/// One returning path's argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnedResult {
    pub path: PathRef,
    pub variant: ResultVariant,

    /// Why every attempt in a class reaching this terminal took the
    /// same arms.
    pub decisions: Vec<DecisionReplay>,

    /// The stable roots the returned payload is derived from.
    pub derivation: Vec<StableRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultReplayObstacle {
    /// The governing key defines no pre-execution equivalence class
    /// (§12).
    GoverningKeyInadmissible { defect: GoverningKeyDefect },

    /// A decision on a returning path is not established to replay, so
    /// a retry may reach a different terminal.
    PathDecisionUnstable {
        path: PathRef,
        decision: DecisionTaken,
        gap: DecisionGap,
    },

    /// The returned payload declares no provenance.
    ResultDerivationUnspecified { path: PathRef, request: Id },

    /// The returned payload depends on roots that are not
    /// replay-stable.
    ResultDerivationRootUnstable {
        path: PathRef,
        request: Id,
        roots: Vec<UnstableRoot>,
    },
}

/// Checks every declared result-replay obligation in the model, as the
/// greatest fixpoint over the obligations that requests into other
/// operations rest on.
pub fn check(model: &Model) -> Vec<ResultReplayCheck> {
    let least = consistent_set(&fixpoint(model, BTreeSet::new()));

    let every: BTreeSet<(Id, Id)> = model
        .operations
        .iter()
        .flat_map(|(operation, declaration)| {
            declaration
                .requirements
                .idempotency
                .iter()
                .filter(|requirement| {
                    requirement.result == ResultReplayRequirement::ReplayConsistent
                })
                .filter_map(move |requirement| {
                    Some((operation.clone(), key_input(&requirement.key)?.clone()))
                })
        })
        .collect();

    let mut checks = fixpoint(model, every);

    for check in &mut checks {
        if matches!(check.verdict, ResultReplayVerdict::Proven { .. })
            && let Some(input) = key_input(&check.key)
            && !least.contains(&(check.operation.clone(), input.clone()))
        {
            check.coinductive = true;
        }
    }

    checks
}

/// The `(operation, input)` pairs whose replay-consistent result
/// requirement is proven: what a request into them may rely on.
pub fn consistent_set(checks: &[ResultReplayCheck]) -> BTreeSet<(Id, Id)> {
    checks
        .iter()
        .filter(|check| matches!(check.verdict, ResultReplayVerdict::Proven { .. }))
        .filter_map(|check| Some((check.operation.clone(), key_input(&check.key)?.clone())))
        .collect()
}

/// Iterates `run` from `assumed` until the proven set is stable. From
/// the empty set the chain ascends to the least fixpoint; from every
/// declared requirement it descends to the greatest, since fewer
/// assumptions never prove more.
fn fixpoint(model: &Model, mut assumed: BTreeSet<(Id, Id)>) -> Vec<ResultReplayCheck> {
    loop {
        let checks = run(model, &assumed);

        let next = consistent_set(&checks);

        if next == assumed {
            return checks;
        }

        assumed = next;
    }
}

fn run(model: &Model, assumed: &BTreeSet<(Id, Id)>) -> Vec<ResultReplayCheck> {
    let mut checks = Vec::new();

    for (operation_id, operation) in &model.operations {
        for (index, requirement) in operation.requirements.idempotency.iter().enumerate() {
            if requirement.result != ResultReplayRequirement::ReplayConsistent {
                continue;
            }

            checks.push(ResultReplayCheck {
                operation: operation_id.clone(),
                requirement: index,
                key: requirement.key.clone(),
                verdict: check_requirement(model, operation, &requirement.key, assumed),
                coinductive: false,
            });
        }
    }

    checks
}

fn check_requirement(
    model: &Model,
    operation: &Operation,
    key: &IdempotencyKey,
    assumed: &BTreeSet<(Id, Id)>,
) -> ResultReplayVerdict {
    let analysis = match ReplayAnalysis::new(model, operation, key, assumed) {
        Ok(analysis) => analysis,

        Err(defect) => {
            return ResultReplayVerdict::Unproven {
                obstacles: vec![ResultReplayObstacle::GoverningKeyInadmissible { defect }],
            };
        }
    };

    if analysis.admits_no_attempts() {
        return ResultReplayVerdict::proven(ResultReplayProof::NoAdmittedInvocations {
            input: analysis.input().clone(),
        });
    }

    let input = analysis.input();

    // The sites: admitted paths returning a result for the triggering
    // input.
    let all = paths(&operation.program);

    let sites: Vec<_> = all
        .iter()
        .filter_map(|path| path.returns_for(input).map(|outcome| (path, outcome)))
        .collect();

    if sites.is_empty() {
        return ResultReplayVerdict::proven(ResultReplayProof::NoReturnedResult {
            input: input.clone(),
        });
    }

    let mut obstacles = Vec::new();
    let mut returns = Vec::new();

    for (path, outcome) in sites {
        let reference = path.reference();
        let trace = analysis.trace(path);

        let before = obstacles.len();

        for (taken, gap) in trace.unstable_decisions() {
            obstacles.push(ResultReplayObstacle::PathDecisionUnstable {
                path: reference.clone(),
                decision: taken.clone(),
                gap: gap.clone(),
            });
        }

        let derivation = match outcome.values() {
            Derivation::Unspecified => {
                obstacles.push(ResultReplayObstacle::ResultDerivationUnspecified {
                    path: reference.clone(),
                    request: input.clone(),
                });

                Vec::new()
            }

            Derivation::Deterministic { from } => {
                let roots: Vec<_> = from.iter().collect();

                let (stable, unstable) = analysis.roots_stability(&trace.end, &roots);

                if !unstable.is_empty() {
                    obstacles.push(ResultReplayObstacle::ResultDerivationRootUnstable {
                        path: reference.clone(),
                        request: input.clone(),
                        roots: unstable,
                    });
                }

                stable
            }
        };

        if obstacles.len() == before {
            returns.push(ReturnedResult {
                path: reference,
                variant: outcome.variant(),
                decisions: trace.stable_decisions(),
                derivation,
            });
        }
    }

    if obstacles.is_empty() {
        ResultReplayVerdict::proven(ResultReplayProof::ClassFixedResult { returns })
    } else {
        ResultReplayVerdict::Unproven {
            obstacles: super::idempotency::dedupe(obstacles, ResultReplayObstacle::site),
        }
    }
}

impl ResultReplayObstacle {
    /// The obstacle with its path forgotten — and, for a decision, the
    /// arm — so the same fact on two paths compares equal.
    fn site(&self) -> Self {
        let mut site = self.clone();

        match &mut site {
            Self::GoverningKeyInadmissible { .. } => {}

            Self::PathDecisionUnstable { path, decision, .. } => {
                *path = PathRef::default();

                match decision {
                    DecisionTaken::Match { arm, .. } => *arm = ResultVariant::Ok,
                    DecisionTaken::Branch { arm, .. } => *arm = crate::spec::Arm::Then,
                }
            }

            Self::ResultDerivationUnspecified { path, .. }
            | Self::ResultDerivationRootUnstable { path, .. } => *path = PathRef::default(),
        }

        site
    }
}

impl ResultReplayCheck {
    /// The diagnostic for an unproven obligation; a proven one
    /// produces none.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let ResultReplayVerdict::Unproven { obstacles } = &self.verdict else {
            return None;
        };

        let operation = &self.operation;
        let requirement = self.requirement;

        Some(Diagnostic {
            code: DiagnosticCode::Verification(VerificationCode::ResultReplayUnproven),
            severity: Severity::Unknown,
            subject: Some(operation.clone()),
            message: format!(
                "Result replay for idempotency requirement {requirement} of \
                 `{operation}` is not established: retries sharing the declared \
                 key are not proven to return the same result."
            ),
            evidence: obstacles
                .iter()
                .map(ResultReplayObstacle::evidence)
                .collect(),
        })
    }
}

impl ResultReplayObstacle {
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
                    "On {}, {} is not established to replay, so a retry may reach a \
                     different terminal: {}.",
                    describe_path(path),
                    describe_decision(decision),
                    decision_gap_sentence(gap)
                ),
            },

            Self::ResultDerivationUnspecified { path, request } => Evidence {
                subject: Some(request.clone()),
                message: format!(
                    "The result {} returns for `{request}` declares no provenance; no \
                     replay-consistency proof may be derived from it.",
                    describe_path(path)
                ),
            },

            Self::ResultDerivationRootUnstable {
                path,
                request,
                roots,
            } => Evidence {
                subject: Some(request.clone()),
                message: format!(
                    "The result {} returns for `{request}` depends on roots that are \
                     not replay-stable: {}.",
                    describe_path(path),
                    unstable_roots(roots)
                ),
            },
        }
    }
}
