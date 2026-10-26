//! Candidate evaluation: what a patch *would* do, judged without
//! committing it (§15 of the System One orchestration revision).
//!
//! A builder that synthesizes a repair must learn whether the repair
//! validates, and whether it proves its target, before choosing to
//! submit it. The pipeline is the analysis pipeline — apply, assemble,
//! validate, verify — and every stage is a pure function, so a
//! candidate is judged against a copy of the task's pinned workspace
//! and nothing else is touched: nothing is committed, no event is
//! published, no task is invalidated.
//!
//! A verdict is advisory. The gate validates a submission against the
//! head, and the analysis of the resulting revision is the authority.
//! A verdict that went stale is rejected by the read-set; one that
//! slips through is re-verified by the workflow's fixpoint.

use std::collections::{BTreeMap, BTreeSet};

use crate::analyzer::verification::transaction_conflicts::ConflictIndex;
use crate::analyzer::verification::{
    IdempotencyVerdict, RecoverabilityVerdict, ResultReplayVerdict, TransactionOrderingVerdict,
    TransactionSerializabilityVerdict,
};
use crate::analyzer::{self, Diagnostic, verification::VerificationReport};
use crate::spec::{Id, Model};

use super::analysis::AnalysisDiagnostic;
use super::commit::{DraftDiagnostic, apply_patch};
use super::patch::SpecPatch;
use super::symbol::{RequirementFamily, SymbolKey};
use super::workspace::{AssemblyGap, WorkspaceState};

/// One declared requirement, identified the same way across two
/// verifications of a model: the family, the operation, the transaction
/// for a transaction family, and the index into the declaration list.
///
/// A candidate that adds or removes a declaration shifts the indices
/// after it, so the identity is meaningful only between models that
/// declare the same requirements — which is what a repair of a program
/// body leaves alone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequirementRef {
    pub family: RequirementFamily,
    pub operation: Id,
    pub transaction: Option<Id>,
    pub index: usize,
}

impl std::fmt::Display for RequirementRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.transaction {
            Some(transaction) => write!(
                f,
                "{} #{} of {transaction} in {}",
                self.family, self.index, self.operation
            ),
            None => write!(f, "{} #{} of {}", self.family, self.index, self.operation),
        }
    }
}

/// Whether each declared requirement is proven, by identity.
pub fn standing(report: &VerificationReport) -> BTreeMap<RequirementRef, bool> {
    let transactional = |family, operation: &Id, transaction: &Id, index| RequirementRef {
        family,
        operation: operation.clone(),
        transaction: Some(transaction.clone()),
        index,
    };

    let operational = |family, operation: &Id, index| RequirementRef {
        family,
        operation: operation.clone(),
        transaction: None,
        index,
    };

    let serializability = report.transaction_serializability.iter().map(|check| {
        (
            transactional(
                RequirementFamily::TransactionSerializability,
                &check.operation,
                &check.transaction,
                check.requirement,
            ),
            matches!(
                check.verdict,
                TransactionSerializabilityVerdict::Proven { .. }
            ),
        )
    });

    let ordering = report.transaction_ordering.iter().map(|check| {
        (
            transactional(
                RequirementFamily::TransactionOrdering,
                &check.operation,
                &check.transaction,
                check.requirement,
            ),
            matches!(check.verdict, TransactionOrderingVerdict::Proven { .. }),
        )
    });

    let idempotency = report.idempotency.iter().map(|check| {
        (
            operational(
                RequirementFamily::Idempotency,
                &check.operation,
                check.requirement,
            ),
            matches!(check.verdict, IdempotencyVerdict::Proven { .. }),
        )
    });

    let result_replay = report.result_replay.iter().map(|check| {
        (
            operational(
                RequirementFamily::ResultReplay,
                &check.operation,
                check.requirement,
            ),
            matches!(check.verdict, ResultReplayVerdict::Proven { .. }),
        )
    });

    let recoverability = report.recoverability.iter().map(|check| {
        (
            operational(
                RequirementFamily::Recoverability,
                &check.operation,
                check.requirement,
            ),
            matches!(check.verdict, RecoverabilityVerdict::Proven { .. }),
        )
    });

    serializability
        .chain(ordering)
        .chain(idempotency)
        .chain(result_replay)
        .chain(recoverability)
        .collect()
}

/// How far a candidate got, and what the checker made of it. Exactly
/// the stages a committed revision passes through, in order: a later
/// field is meaningful only when every earlier one is empty.
#[derive(Debug, Clone, Default)]
pub struct CandidateVerdict {
    /// The symbol a mutation addresses that the task's write scope
    /// does not authorize. Nothing else was judged.
    pub scope_violation: Option<SymbolKey>,

    /// Draft-local diagnostics from applying the patch.
    pub draft_diagnostics: Vec<DraftDiagnostic>,

    /// Why the patched workspace cannot become a model.
    pub assembly_gaps: Vec<AssemblyGap>,

    /// Structural validation errors of the assembled model.
    pub validation: Vec<AnalysisDiagnostic>,

    /// The verification of the patched model, when it validates.
    pub verification: Option<VerificationReport>,
}

impl CandidateVerdict {
    /// The verification report, when the candidate got that far.
    pub fn verified(&self) -> Option<&VerificationReport> {
        self.verification.as_ref()
    }

    /// The requirements proven before the candidate and not under it.
    /// A requirement the candidate no longer declares is not among
    /// them: what is not declared is not unproven.
    pub fn regressions(&self, baseline: &VerificationReport) -> Vec<RequirementRef> {
        let Some(verification) = &self.verification else {
            return Vec::new();
        };

        let after = standing(verification);

        standing(baseline)
            .into_iter()
            .filter(|(requirement, proven)| *proven && after.get(requirement) == Some(&false))
            .map(|(requirement, _)| requirement)
            .collect()
    }

    /// Why the candidate could not be verified, in one line.
    pub fn refusal(&self) -> Option<String> {
        if let Some(symbol) = &self.scope_violation {
            return Some(format!("it writes {symbol}, outside the task's scope"));
        }

        if let Some(diagnostic) = self.draft_diagnostics.first() {
            return Some(format!("it does not apply: {}", diagnostic.message));
        }

        if let Some(gap) = self.assembly_gaps.first() {
            return Some(format!("it does not assemble: {gap}"));
        }

        self.validation
            .first()
            .map(|error| format!("it does not validate: {}", error.message))
    }
}

/// What a verdict about some transactions rests on: the operations of
/// their conflict closures, and every object those closures access.
/// Observing the first catches a changed member; observing who reads
/// and writes the second catches a new one — the phantom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ClosureFootprint {
    pub operations: BTreeSet<Id>,

    /// `(data model, object)`.
    pub objects: BTreeSet<(Id, Id)>,
}

/// Applies `patch` to `workspace` and judges the result. Pure, and
/// synchronous: the caller runs it on a blocking worker.
///
/// The footprint is taken over the *patched* model, because that is
/// the model the verdict is about; `roots` names the transactions, as
/// `(operation, transaction)`, whose closures it rests on.
pub(super) fn judge(
    mut workspace: WorkspaceState,
    patch: &SpecPatch,
    roots: &[(Id, Id)],
) -> (CandidateVerdict, ClosureFootprint) {
    let mut verdict = CandidateVerdict {
        draft_diagnostics: apply_patch(&mut workspace, patch),
        ..Default::default()
    };

    if !verdict.draft_diagnostics.is_empty() {
        return (verdict, ClosureFootprint::default());
    }

    let model = match workspace.assemble_model() {
        Ok(model) => model,

        Err(error) => {
            verdict.assembly_gaps = error.gaps;

            return (verdict, ClosureFootprint::default());
        }
    };

    let footprint = closure_footprint(&model, roots);

    verdict.validation = analyzer::validate(&model)
        .into_iter()
        .map(|error| AnalysisDiagnostic::from(Diagnostic::from(error)))
        .collect();

    if verdict.validation.is_empty() {
        verdict.verification = Some(analyzer::verification::verify(&model));
    }

    (verdict, footprint)
}

fn closure_footprint(model: &Model, roots: &[(Id, Id)]) -> ClosureFootprint {
    let mut footprint = ClosureFootprint::default();

    if roots.is_empty() {
        return footprint;
    }

    let index = ConflictIndex::build(model);

    let data_model_of = |object: &Id| {
        model
            .data_models
            .iter()
            .find(|(_, data_model)| data_model.objects.contains_key(object))
            .map(|(id, _)| id.clone())
    };

    for (operation, transaction) in roots {
        let Some(root) = index.position(operation, transaction) else {
            continue;
        };

        for member in index.closure(root) {
            let template = &index.templates[member];

            footprint
                .operations
                .insert(template.reference.operation.clone());

            let accessed = template
                .accesses
                .iter()
                .map(|access| &access.object)
                .chain(template.locks.iter().map(|lock| &lock.object));

            for object in accessed {
                if let Some(data_model) = data_model_of(object) {
                    footprint.objects.insert((data_model, object.clone()));
                }
            }
        }
    }

    footprint
}
