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

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use uuid::Uuid;

use crate::analyzer::verification::transaction_serializability::{
    TransactionSerializabilityObstacle, TransactionSerializabilityVerdict,
};
use crate::analyzer::verification::{
    TransactionOrderingCheck, TransactionOrderingObstacle, TransactionOrderingVerdict,
};
use crate::confluence::{
    BundleSpec, CandidateVerdict, CommitRequest, DraftOperation, EngineError, Mutation, PatchId,
    RequirementRef, SpecPatch, SymbolKey, WriteGrant, standing,
};
use crate::spec::{CursorAdvanceRule, DataObject, FieldPath, Id, TransactionStep};
use crate::system_one::DecisionRequest;
use crate::system_one::questions::repair as wording;

use super::describe::summarize;
use super::remedies::{self, KeyedCommitGap, OrderingGap, Programs, Remedy, RemedyKind};
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
            max_candidates: 12,
        }
    }
}

/// A candidate and what the analyzer made of it.
struct Judged {
    remedy: Remedy,

    /// The targets it proves.
    proves: Vec<RequirementRef>,

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

    // Every family has remedies in the catalogue now; what is left over
    // is what no candidate proves, and the fixpoint offers it again.
    let targets = unproven;

    let report = before;

    let unproven_ordering: Vec<&TransactionOrderingCheck> = report
        .transaction_ordering
        .iter()
        .filter(|check| programs.contains_key(&check.operation))
        .filter(|check| matches!(check.verdict, TransactionOrderingVerdict::Unproven { .. }))
        .collect();

    // Serializability obstacles, including those an ordering proof is
    // waiting on: ordering rests on the closure being serializable.
    let mut obstacles: Vec<&TransactionSerializabilityObstacle> = report
        .transaction_serializability
        .iter()
        .filter(|check| programs.contains_key(&check.operation))
        .filter_map(|check| match &check.verdict {
            TransactionSerializabilityVerdict::Unproven { obstacles } => Some(obstacles),
            TransactionSerializabilityVerdict::Proven { .. } => None,
        })
        .flatten()
        .collect();

    for check in &unproven_ordering {
        if let TransactionOrderingVerdict::Unproven {
            obstacles: ordering,
        } = &check.verdict
        {
            for obstacle in ordering {
                if let TransactionOrderingObstacle::OrderingMissingSerializability {
                    obstacles: inner,
                } = obstacle
                {
                    obstacles.extend(inner.iter());
                }
            }
        }
    }

    let ordering_gaps: Vec<OrderingGap<'_>> = unproven_ordering
        .iter()
        .filter_map(|check| {
            let TransactionOrderingVerdict::Unproven { obstacles } = &check.verdict else {
                return None;
            };

            let requirement = programs
                .get(&check.operation)?
                .transaction(&check.transaction)?
                .requirements
                .ordering
                .get(check.requirement)?;

            Some(OrderingGap {
                operation: check.operation.clone(),
                transaction: check.transaction.clone(),
                requirement,
                obstacles,
            })
        })
        .collect();

    let keyed = keyed_commit_gaps(report, &programs);

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

    candidates.extend(remedies::ordering_candidates(&programs, &ordering_gaps));

    // A keyed commit edits no step, so it composes with every other
    // candidate: replay and serializability gaps often stand together.
    if !keyed.is_empty() {
        let routes = candidates.clone();

        candidates.extend(remedies::keyed_commits(&programs, &keyed));

        candidates.extend(
            routes
                .iter()
                .filter_map(|remedy| remedies::with_keyed_commits(&programs, remedy, &keyed)),
        );
    }

    candidates.sort_by_key(Remedy::invasiveness);
    candidates.dedup_by(|later, earlier| later.programs == earlier.programs);
    candidates.truncate(policy.max_candidates);

    if candidates.is_empty() {
        return Ok(abstain(
            "the catalogue offers no repair of the programs in scope",
            vec![format!(
                "every route the catalogue knows needs an edit outside the programs in scope, \
                 or a judgment about what a rejected transaction should do; unproven: {}",
                targets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )],
        ));
    }

    let judged = judge(context, &programs, candidates, before, &targets, &roots).await?;

    // Admissible, most targets proven first; among equals the catalogue's
    // least-invasive order stands (the sort is stable).
    let mut admissible: Vec<&Judged> = judged
        .iter()
        .filter(|judged| judged.inadmissible.is_none())
        .collect();

    admissible.sort_by_key(|judged| std::cmp::Reverse(judged.proves.len()));

    let verdicts: Vec<String> = judged
        .iter()
        .map(|judged| match &judged.inadmissible {
            None => format!("proven, not chosen: {}", judged.remedy.summary),
            Some(reason) => format!("tried: {} — {reason}", judged.remedy.summary),
        })
        .collect();

    if admissible.is_empty() {
        return Ok(abstain(
            "no candidate repair proves a target without un-proving another requirement",
            verdicts,
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

    let left: Vec<&RequirementRef> = targets
        .iter()
        .filter(|target| !chosen.proves.contains(target))
        .collect();

    let outcome = context
        .engine
        .submit(CommitRequest {
            task: context.task,
            patch_id: PatchId::fresh(),
            base_revision: task.snapshot_revision,
            patch: replacing(&programs, &chosen.remedy),
            client_nonce: Uuid::new_v4(),
        })
        .await?;

    Ok(match outcome {
        Ok(_) => Built::Committed {
            summary: format!(
                "{named}: {} — proves {}; {} of {} candidates were admissible{why}{}",
                chosen.remedy.summary,
                chosen
                    .proves
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                admissible.len(),
                judged.len(),
                if left.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; left for another pass: {}",
                        left.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ),
        },

        Err(rejection) if rejection.is_stale_context() => Built::Stale,

        Err(rejection) => {
            let mut findings = vec![format!(
                "the gate rejected `{}`: {rejection:?}",
                chosen.remedy.summary
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

            let (proves, inadmissible) = inadmissibility(&verdict, before, targets);

            Ok(Judged {
                inadmissible,
                proves,
                remedy,
            })
        })
        .collect()
}

/// The targets a candidate proves, and why it may not be submitted when
/// it may not: it is not verified, it proves none of its targets, or it
/// un-proves something proven before it. A candidate that proves some
/// targets and leaves others is admissible — the workflow offers what
/// is left again on its next pass (§18.4).
fn inadmissibility(
    verdict: &CandidateVerdict,
    before: &crate::analyzer::verification::VerificationReport,
    targets: &[RequirementRef],
) -> (Vec<RequirementRef>, Option<String>) {
    let Some(after) = verdict.verified() else {
        return (
            Vec::new(),
            Some(
                verdict
                    .refusal()
                    .unwrap_or_else(|| "it was not verified".to_string()),
            ),
        );
    };

    let after = standing(after);

    let proves: Vec<RequirementRef> = targets
        .iter()
        .filter(|target| after.get(*target) == Some(&true))
        .cloned()
        .collect();

    if proves.is_empty() {
        return (
            proves,
            Some(format!(
                "it proves none of its targets: it leaves {} unproven",
                targets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        );
    }

    let regressed = verdict
        .regressions(before)
        .first()
        .map(|regressed| format!("it un-proves {regressed}"));

    (proves, regressed)
}

/// The candidate to submit, and a clause saying why when it is not the
/// head of the deterministic order.
///
/// `admissible` is in that order: most targets proven, then least
/// invasive. A System One decider may reorder candidates that prove the
/// same targets, and only on a fact the prompt states:
///
/// - heavy demand for one record moves exclusive locks ahead of
///   serializable isolation (under serializable isolation a conflicting
///   execution is aborted and retried; under a lock it waits);
/// - every position being applied, none skipped, moves a `successor`
///   cursor ahead of a `monotonic_after` one.
///
/// Anything less leaves the order standing. Each question is asked only
/// when its answer could change the head.
async fn prefer<'a>(
    context: &BuildContext<'_>,
    policy: &RepairPolicy,
    prompt_evidence: &[crate::confluence::PromptEvidence],
    draft: &DraftOperation,
    programs: &Programs,
    operation: &Id,
    admissible: &[&'a Judged],
) -> (&'a Judged, &'static str) {
    let first = admissible[0];

    // Only candidates proving what the head proves may replace it.
    let peers: Vec<&'a Judged> = admissible
        .iter()
        .copied()
        .filter(|judged| judged.proves == first.proves)
        .collect();

    let locks = peers
        .iter()
        .copied()
        .find(|judged| judged.remedy.kind == RemedyKind::StrictLocks);

    let successor = peers.iter().copied().find(|judged| {
        judged.remedy.kind == RemedyKind::OrderedCursor
            && uses_rule(&judged.remedy, programs, CursorAdvanceRule::Successor)
    });

    let (question, spec, alternative, why) = match (first.remedy.kind, locks, successor) {
        (RemedyKind::SerializableClosure, Some(locks), _) => (
            wording::contention(),
            wording::CONTENTION,
            locks,
            ", preferred to serializable isolation because the prompt states heavy demand for \
             one record",
        ),

        (RemedyKind::OrderedCursor, _, Some(successor))
            if !uses_rule(&first.remedy, programs, CursorAdvanceRule::Successor) =>
        {
            (
                wording::gap_free(),
                wording::GAP_FREE,
                successor,
                ", a successor cursor because the prompt states that no position may be skipped",
            )
        }

        _ => return (first, ""),
    };

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

    let id = spec.id.rsplit('.').next().unwrap_or(spec.id).to_string();

    let request = DecisionRequest::new(json!({
        "prompt": prompt,
        "operation": { "id": operation, "description": draft.description },
        "work": work,
    }))
    .ask(id.clone(), question)
    .tag("task", context.task.to_string())
    .tag("builder", "requirement_repair")
    .tag("operation", operation.to_string())
    .tag(format!("spec.{}", spec.id), spec.tag());

    let stated = context
        .decider
        .decide(&request)
        .await
        .ok()
        .and_then(|decision| decision.noul(&id))
        .is_some_and(|probability| probability >= policy.act);

    if stated {
        (alternative, why)
    } else {
        (first, "")
    }
}

/// Whether a candidate introduces a cursor advance under `rule`.
fn uses_rule(remedy: &Remedy, before: &Programs, rule: CursorAdvanceRule) -> bool {
    remedy.edited(before).any(|(_, program)| {
        program.transactions().into_iter().any(|(_, transaction)| {
            transaction.steps.iter().any(|step| {
                matches!(step, TransactionStep::AdvanceCursor(advance) if advance.rule == rule)
            })
        })
    })
}

/// The transactions whose replay routes lack a keyed commit, per
/// unproven idempotency, result-replay and recoverability requirement
/// in scope, with that requirement's governing key.
///
/// Found as every `{transaction, recovery}` in the verdict's evidence
/// whose recovery gaps name a missing or unstable keyed commit — the
/// shape every obstacle that carries replay gaps shares.
fn keyed_commit_gaps(
    report: &crate::analyzer::verification::VerificationReport,
    programs: &Programs,
) -> Vec<KeyedCommitGap> {
    use crate::analyzer::verification::{
        IdempotencyVerdict, RecoverabilityVerdict, ResultReplayVerdict,
    };

    let mut unproven: Vec<(&Id, &crate::spec::IdempotencyKey, serde_json::Value)> = Vec::new();

    for check in &report.idempotency {
        if let IdempotencyVerdict::Unproven { obstacles } = &check.verdict {
            unproven.push((&check.operation, &check.key, json!(obstacles)));
        }
    }

    for check in &report.result_replay {
        if let ResultReplayVerdict::Unproven { obstacles } = &check.verdict {
            unproven.push((&check.operation, &check.key, json!(obstacles)));
        }
    }

    for check in &report.recoverability {
        if let RecoverabilityVerdict::Unproven { obstacles } = &check.verdict {
            unproven.push((&check.operation, &check.key, json!(obstacles)));
        }
    }

    let mut gaps: Vec<KeyedCommitGap> = Vec::new();

    for (operation, key, evidence) in unproven {
        if !programs.contains_key(operation) {
            continue;
        }

        let mut named = BTreeSet::new();

        unkeyed_transactions(&evidence, &mut named);

        for transaction in named {
            if !gaps
                .iter()
                .any(|gap| &gap.operation == operation && gap.transaction == transaction)
            {
                gaps.push(KeyedCommitGap {
                    operation: operation.clone(),
                    transaction,
                    key: key.clone(),
                });
            }
        }
    }

    gaps
}

fn unkeyed_transactions(value: &serde_json::Value, into: &mut BTreeSet<Id>) {
    match value {
        serde_json::Value::Object(map) => {
            if let (
                Some(serde_json::Value::String(transaction)),
                Some(serde_json::Value::Array(recovery)),
            ) = (map.get("transaction"), map.get("recovery"))
                && recovery.iter().any(|gap| {
                    matches!(
                        gap.get("kind").and_then(serde_json::Value::as_str),
                        Some("no_keyed_commit" | "commit_key_root_unstable")
                    )
                })
            {
                into.insert(Id(transaction.clone()));
            }

            for nested in map.values() {
                unkeyed_transactions(nested, into);
            }
        }

        serde_json::Value::Array(items) => {
            for item in items {
                unkeyed_transactions(item, into);
            }
        }

        _ => {}
    }
}
