//! The requirement repair builder: generate and verify (§18 of the
//! System One orchestration revision).
//!
//! An unproven serializability obligation names its obstacles, and each
//! obstacle its gaps. Code synthesizes the candidate repairs
//! ([`remedies`](super::remedies)), the analyzer judges every one of
//! them against the task's pinned snapshot
//! ([`evaluate_candidate`](crate::confluence::ConfluenceEngine::evaluate_candidate)),
//! and only a candidate that is *admissible* — it validates, it proves
//! every obligation it targets, and it un-proves nothing anywhere in
//! the model — is ever submitted.
//!
//! So nothing here makes an obligation proven, and no model is asked
//! whether a repair is correct. A System One decider is asked at most
//! one thing: given two repairs that are both proven, whether the
//! prompt states the fact that would prefer the more invasive one.
//! Absent that, the preference is code's — least invasive first — and a
//! decider that is unsure, or unavailable, reorders nothing.
//!
//! Unlike discovery, repair need not decide its task whole. The
//! workflow re-enumerates what is unproven on every pass of its
//! fixpoint, so an obligation this builder leaves is offered again, and
//! the regression check is what makes a partial repair safe.

use std::collections::BTreeMap;

use serde_json::json;
use uuid::Uuid;

use crate::analyzer::verification::transaction_serializability::{
    TransactionSerializabilityObstacle, TransactionSerializabilityVerdict,
};
use crate::confluence::{
    BundleSpec, CandidateVerdict, CommitRequest, DraftOperation, EngineError, Mutation, PatchId,
    RequirementFamily, RequirementRef, SpecPatch, SymbolKey, WriteGrant, standing,
};
use crate::spec::{DataObject, FieldPath, Id};
use crate::system_one::DecisionRequest;
use crate::system_one::questions::repair as wording;

use super::describe::summarize;
use super::remedies::{self, Programs, Remedy, RemedyKind};
use super::{Abstention, BuildContext, Built};

/// What repair acts on. Provisional, as every threshold is (§13.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepairPolicy {
    /// At or above, the prompt states the fact a preference rests on.
    /// Below, the deterministic order stands.
    pub act: f64,

    /// The most candidates judged for one task. Evaluation is full
    /// verification of the patched model, so its cost bounds this.
    pub max_candidates: usize,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            act: 0.8,
            max_candidates: 8,
        }
    }
}

/// A candidate and what the analyzer made of it.
struct Judged {
    remedy: Remedy,

    /// Why it may not be submitted, when it may not.
    inadmissible: Option<String>,
}

pub(super) async fn build(context: &BuildContext<'_>, policy: &RepairPolicy) -> Built {
    match repair(context, policy).await {
        Ok(built) => built,

        Err(error) => Built::Abstained(Abstention::because(format!(
            "the engine refused a read: {error}"
        ))),
    }
}

fn abstain(reason: impl Into<String>, findings: Vec<String>) -> Built {
    Built::Abstained(Abstention::because(reason).with_findings(findings))
}

async fn repair(context: &BuildContext<'_>, policy: &RepairPolicy) -> Result<Built, EngineError> {
    let task = context.engine.task_context(context.task)?;

    // Every program the task may rewrite: one, or every program of one
    // conflict closure (§18.3).
    let scope: Vec<Id> = task
        .write_scope
        .grants
        .iter()
        .filter_map(|grant| match grant {
            WriteGrant::OperationProgram(operation) => Some(operation.clone()),
            _ => None,
        })
        .collect();

    let Some((operation, peers)) = scope.split_first() else {
        return Ok(abstain(
            "the task's scope names no program to repair",
            Vec::new(),
        ));
    };

    let operation = operation.clone();

    let bundle = context.engine.context_bundle(
        context.task,
        &BundleSpec {
            operation: Some(operation.clone()),
            requirements: Vec::new(),
            include: Vec::new(),
            peers: peers
                .iter()
                .map(|peer| (peer.clone(), Vec::new()))
                .collect(),
        },
    )?;

    let drafts: Vec<DraftOperation> = bundle
        .operation
        .iter()
        .chain(&bundle.peer_operations)
        .filter_map(|draft| serde_json::from_value::<DraftOperation>(draft.clone()).ok())
        .collect();

    if drafts.len() != scope.len() {
        return Ok(abstain(
            "an operation's draft could not be read",
            Vec::new(),
        ));
    }

    let mut programs: Programs = BTreeMap::new();

    for (id, draft) in scope.iter().zip(&drafts) {
        let Some(program) = &draft.program else {
            return Ok(abstain(format!("{id} has no program yet"), Vec::new()));
        };

        programs.insert(id.clone(), program.clone());
    }

    let draft = &drafts[0];

    // Every transaction whose serializability the verdicts below speak
    // to: their closures are what those verdicts rest on.
    let roots: Vec<(Id, Id)> = programs
        .iter()
        .flat_map(|(id, program)| {
            program
                .transactions()
                .into_iter()
                .filter(|(_, transaction)| !transaction.requirements.serializability.is_empty())
                .map(|(_, transaction)| (id.clone(), transaction.id.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    // The baseline is the empty candidate: the same pipeline, tracked
    // the same way, so what is unproven is observed rather than assumed.
    let baseline = context
        .engine
        .evaluate_candidate(context.task, &SpecPatch::default(), &roots)
        .await?;

    let Some(before) = baseline.verified() else {
        return Ok(abstain(
            format!(
                "the model cannot be verified at this snapshot: {}",
                baseline
                    .refusal()
                    .unwrap_or_else(|| "no verdict".to_string())
            ),
            Vec::new(),
        ));
    };

    let unproven: Vec<RequirementRef> = standing(before)
        .into_iter()
        .filter(|(requirement, proven)| programs.contains_key(&requirement.operation) && !proven)
        .map(|(requirement, _)| requirement)
        .collect();

    let named = scope
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    if unproven.is_empty() {
        return Ok(Built::NothingToDo {
            summary: format!("{named}: nothing is unproven at this snapshot"),
        });
    }

    let (targets, others): (Vec<RequirementRef>, Vec<RequirementRef>) =
        unproven.into_iter().partition(|requirement| {
            requirement.family == RequirementFamily::TransactionSerializability
        });

    let left: Vec<String> = others
        .iter()
        .map(|requirement| format!("not in the catalogue: {requirement}"))
        .collect();

    if targets.is_empty() {
        return Ok(abstain(
            "none of its unproven obligations is one the remedy catalogue covers",
            left,
        ));
    }

    let obstacles: Vec<&TransactionSerializabilityObstacle> = before
        .transaction_serializability
        .iter()
        .filter(|check| programs.contains_key(&check.operation))
        .filter_map(|check| match &check.verdict {
            TransactionSerializabilityVerdict::Unproven { obstacles } => Some(obstacles),
            TransactionSerializabilityVerdict::Proven { .. } => None,
        })
        .flatten()
        .collect();

    // Which objects carry a version is a fact of the data model, read
    // from the symbols the bundle already recorded as observed.
    let versions: BTreeMap<Id, FieldPath> = bundle
        .shared_symbols
        .iter()
        .filter_map(|view| match &view.key {
            SymbolKey::DataObject { object, .. } => {
                serde_json::from_value::<DataObject>(view.content.clone())
                    .ok()
                    .and_then(|data| data.version)
                    .map(|version| (object.clone(), version.field))
            }
            _ => None,
        })
        .collect();

    let mut candidates = remedies::candidates(&programs, &obstacles, &versions);

    candidates.truncate(policy.max_candidates);

    if candidates.is_empty() {
        let mut findings = vec![
            "every route the catalogue knows needs an edit outside the programs in scope, \
             or a judgment about what a rejected transaction should do"
                .to_string(),
        ];

        findings.extend(left);

        return Ok(abstain(
            "the catalogue offers no repair of the programs in scope",
            findings,
        ));
    }

    let judged = judge(context, &programs, candidates, before, &targets, &roots).await?;

    let admissible: Vec<&Remedy> = judged
        .iter()
        .filter(|judged| judged.inadmissible.is_none())
        .map(|judged| &judged.remedy)
        .collect();

    let verdicts: Vec<String> = judged
        .iter()
        .map(|judged| match &judged.inadmissible {
            None => format!("proven, not chosen: {}", judged.remedy.summary),
            Some(reason) => format!("tried: {} — {reason}", judged.remedy.summary),
        })
        .collect();

    if admissible.is_empty() {
        let mut findings = verdicts;

        findings.extend(left);

        return Ok(abstain(
            "no candidate repair proves its target without un-proving another requirement",
            findings,
        ));
    }

    let (chosen, why) = prefer(
        context,
        policy,
        &task.prompt_evidence,
        draft,
        &programs,
        &operation,
        &admissible,
    )
    .await;

    let outcome = context
        .engine
        .submit(CommitRequest {
            task: context.task,
            patch_id: PatchId::fresh(),
            base_revision: task.snapshot_revision,
            patch: replacing(&programs, chosen),
            client_nonce: Uuid::new_v4(),
        })
        .await?;

    Ok(match outcome {
        Ok(_) => Built::Committed {
            summary: format!(
                "{named}: {} — proves {}; {} of {} candidates were admissible{why}{}",
                chosen.summary,
                targets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                admissible.len(),
                judged.len(),
                if others.is_empty() {
                    String::new()
                } else {
                    format!("; {} other obligations are left", others.len())
                }
            ),
        },

        Err(rejection) if rejection.is_stale_context() => Built::Stale,

        Err(rejection) => {
            let mut findings = vec![format!(
                "the gate rejected `{}`: {rejection:?}",
                chosen.summary
            )];

            findings.extend(verdicts);

            abstain(
                "the gate rejected a repair the analyzer had admitted",
                findings,
            )
        }
    })
}

/// One program replacement per program the remedy changes.
fn replacing(before: &Programs, remedy: &Remedy) -> SpecPatch {
    SpecPatch {
        mutations: remedy
            .edited(before)
            .map(|(operation, program)| Mutation::ReplaceOperationProgram {
                operation: operation.clone(),
                program: program.clone(),
            })
            .collect(),
    }
}

/// Judges every candidate concurrently. A candidate is admissible when
/// it verifies, every target is declared and proven under it, and
/// nothing proven before it is unproven under it (§18.4).
async fn judge(
    context: &BuildContext<'_>,
    programs: &Programs,
    candidates: Vec<Remedy>,
    before: &crate::analyzer::verification::VerificationReport,
    targets: &[RequirementRef],
    roots: &[(Id, Id)],
) -> Result<Vec<Judged>, EngineError> {
    let verdicts = futures::future::join_all(candidates.iter().map(|remedy| {
        let patch = replacing(programs, remedy);

        async move {
            context
                .engine
                .evaluate_candidate(context.task, &patch, roots)
                .await
        }
    }))
    .await;

    candidates
        .into_iter()
        .zip(verdicts)
        .map(|(remedy, verdict)| {
            let verdict = verdict?;

            Ok(Judged {
                inadmissible: inadmissibility(&verdict, before, targets),
                remedy,
            })
        })
        .collect()
}

fn inadmissibility(
    verdict: &CandidateVerdict,
    before: &crate::analyzer::verification::VerificationReport,
    targets: &[RequirementRef],
) -> Option<String> {
    let Some(after) = verdict.verified() else {
        return Some(
            verdict
                .refusal()
                .unwrap_or_else(|| "it was not verified".to_string()),
        );
    };

    let after = standing(after);

    if let Some(target) = targets
        .iter()
        .find(|target| after.get(*target) != Some(&true))
    {
        return Some(format!("it leaves {target} unproven"));
    }

    verdict
        .regressions(before)
        .first()
        .map(|regressed| format!("it un-proves {regressed}"))
}

/// The candidate to submit, and a clause saying why when it is not the
/// least invasive one.
///
/// `admissible` is in the deterministic order. A System One decider may
/// move exclusive locks ahead of serializable isolation, and only on
/// the prompt stating heavy demand for one record: under serializable
/// isolation a conflicting execution is aborted and retried, under a
/// lock it waits. Anything less than that leaves the order standing.
async fn prefer<'a>(
    context: &BuildContext<'_>,
    policy: &RepairPolicy,
    prompt_evidence: &[crate::confluence::PromptEvidence],
    draft: &DraftOperation,
    programs: &Programs,
    operation: &Id,
    admissible: &[&'a Remedy],
) -> (&'a Remedy, &'static str) {
    let first = admissible[0];

    let position = |kind: RemedyKind| admissible.iter().position(|remedy| remedy.kind == kind);

    let (Some(isolation), Some(locks)) = (
        position(RemedyKind::SerializableClosure),
        position(RemedyKind::StrictLocks),
    ) else {
        return (first, "");
    };

    // Only the head of the order is submitted, so the question matters
    // only when it could change the head.
    if isolation != 0 {
        return (first, "");
    }

    let prompt: String = prompt_evidence
        .iter()
        .map(|evidence| evidence.excerpt.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    if prompt.trim().is_empty() {
        return (first, "");
    }

    let work: Vec<String> = programs
        .values()
        .flat_map(|program| program.transactions())
        .filter_map(|(_, transaction)| summarize(transaction))
        .collect();

    let request = DecisionRequest::new(json!({
        "prompt": prompt,
        "operation": { "id": operation, "description": draft.description },
        "work": work,
    }))
    .ask("contention", wording::contention())
    .tag("task", context.task.to_string())
    .tag("builder", "requirement_repair")
    .tag("operation", operation.to_string())
    .tag(
        format!("spec.{}", wording::CONTENTION.id),
        wording::CONTENTION.tag(),
    );

    let stated = context
        .decider
        .decide(&request)
        .await
        .ok()
        .and_then(|decision| decision.noul("contention"))
        .is_some_and(|probability| probability >= policy.act);

    if stated {
        (
            admissible[locks],
            ", preferred to serializable isolation because the prompt states heavy demand for \
             one record",
        )
    } else {
        (first, "")
    }
}
