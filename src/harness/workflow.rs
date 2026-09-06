//! The design workflow (§61–§76, §107 phase 7 of the confluence
//! spec): natural-language prompt to validated Conseqa model through
//! staged multi-agent synthesis and requirement-scoped repair to a
//! semantic fixpoint.
//!
//! The workflow enumerates work from the current confluence head and
//! runs it through the scheduler; it never decides correctness itself.
//! Success is a validated, all-obligations-proven, all-obligations-
//! mapped head with no unresolved dependency requests and no active
//! tasks (§75).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::confluence::{
    AnalysisState, BundleSpec, ConfluenceEngine, PromptObligationStatus, RequirementFamily,
    TaskKind, WriteScope,
};
use crate::spec::{Id, Model, Revision};

use super::scheduler::{LogicalTask, Scheduler, SchedulerError};

/// What the workflow was asked to design.
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    /// Where to write the finalized model and reports.
    pub out_dir: PathBuf,

    /// How long to wait for one revision's background analysis before
    /// treating it as stuck.
    pub analysis_timeout: Duration,

    /// Maximum fixpoint iterations before declaring the run incomplete.
    pub max_iterations: u32,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            out_dir: PathBuf::from("."),
            analysis_timeout: Duration::from_secs(120),
            max_iterations: 8,
        }
    }
}

/// The workflow's terminal status (§75).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunStatus {
    /// Validated, every adopted obligation proven, every explicit
    /// prompt obligation mapped, nothing unresolved.
    Success { revision: u64 },

    /// The run could not converge; the reason and the unresolved
    /// obligations are preserved (§75).
    Incomplete {
        revision: u64,
        reason: String,
        unresolved: Vec<String>,
    },
}

/// The full result of a workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub status: RunStatus,
    pub final_revision: u64,
    pub iterations: u32,

    /// Written artifact paths, when finalization ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),

    #[error(transparent)]
    Engine(#[from] crate::confluence::EngineError),

    #[error("analysis for revision {} did not finish within the timeout", .0.0)]
    AnalysisTimeout(Revision),

    #[error("finalization: {0}")]
    Finalization(String),
}

pub struct Workflow {
    scheduler: Scheduler,
    config: WorkflowConfig,
}

impl Workflow {
    pub fn new(scheduler: Scheduler, config: WorkflowConfig) -> Self {
        Self { scheduler, config }
    }

    fn engine(&self) -> &ConfluenceEngine {
        self.scheduler.engine()
    }

    /// Drives the whole design to a fixpoint.
    pub async fn run(&mut self) -> Result<RunReport, WorkflowError> {
        // Phase 2: decomposition establishes the interface epoch before
        // any operation fanout (§62, §96).
        self.decompose().await?;

        // Phase 3: one synthesis task per planned operation (§64).
        self.operation_fanout().await?;

        // Phases 4–7 repeat to a fixpoint (§75).
        let mut iterations = 0;

        loop {
            iterations += 1;

            if iterations > self.config.max_iterations {
                return self.finalize(iterations - 1, None).await;
            }

            // Phase 4: assembly and structural convergence.
            let revision = self.engine().head_revision();
            let analysis = self.await_analysis(revision).await?;

            match analysis {
                AnalysisState::NotAssemblable { gaps } => {
                    // Operations still lack programs — synthesize the
                    // missing ones, then reconverge.
                    let missing = self.missing_program_operations();

                    if missing.is_empty() {
                        return self
                            .incomplete(
                                revision,
                                format!(
                                    "not assemblable but no operation is missing a program: {}",
                                    describe_gaps(&gaps)
                                ),
                            )
                            .await;
                    }

                    self.synthesize_operations(&missing).await?;

                    continue;
                }

                AnalysisState::ValidationFailed { errors } => {
                    // Phase 4 repair: structural diagnostics become
                    // targeted repair. V1 surfaces them and retries
                    // synthesis of every operation an error names.
                    let operations = self.operations_named_by(&errors);

                    if operations.is_empty() {
                        return self
                            .incomplete(
                                revision,
                                format!(
                                    "validation failed with no operation to repair: {}",
                                    errors
                                        .iter()
                                        .map(|error| error.message.clone())
                                        .collect::<Vec<_>>()
                                        .join("; ")
                                ),
                            )
                            .await;
                    }

                    self.synthesize_operations(&operations).await?;

                    continue;
                }

                AnalysisState::Ready(_) => {
                    // Structurally valid. Phase 5: discover
                    // requirements for operations that have none yet.
                    let discovered = self.requirement_discovery().await?;

                    if discovered > 0 {
                        // New requirements changed the head; reconverge
                        // before judging proofs.
                        continue;
                    }

                    // Phase 6–7: verify and repair the unproven.
                    let repaired = self.repair_unproven(revision).await?;

                    if repaired == 0 {
                        // Nothing left to repair this iteration: either
                        // success or a stuck obstacle.
                        return self.finalize(iterations, Some(revision)).await;
                    }
                }

                other => {
                    return self
                        .incomplete(revision, format!("analysis stuck in {}", other.label()))
                        .await;
                }
            }
        }
    }

    async fn decompose(&mut self) -> Result<(), WorkflowError> {
        // Only decompose an empty head; an adopted model skips
        // straight to convergence.
        if !self.engine().head_snapshot().workspace.operations.is_empty() {
            return Ok(());
        }

        let objective =
            "Decompose the application prompt into the shared architecture skeleton."
                .to_string();

        self.scheduler
            .run(&LogicalTask {
                kind: TaskKind::Decompose,
                objective,
                write_scope: WriteScope::shared_skeleton(),
                bundle: BundleSpec::default(),
                prompt_evidence: Vec::new(),
            })
            .await?;

        Ok(())
    }

    async fn operation_fanout(&mut self) -> Result<(), WorkflowError> {
        let planned = self.missing_program_operations();

        self.synthesize_operations(&planned).await
    }

    async fn synthesize_operations(&mut self, operations: &[Id]) -> Result<(), WorkflowError> {
        for operation in operations {
            self.scheduler
                .run(&LogicalTask {
                    kind: TaskKind::OperationSynthesis,
                    objective: format!("Synthesize the program and execution facts of {operation}."),
                    write_scope: WriteScope::operation_synthesis(operation.clone()),
                    bundle: BundleSpec {
                        operation: Some(operation.clone()),
                        requirement: None,
                        include: Vec::new(),
                    },
                    prompt_evidence: Vec::new(),
                })
                .await?;
        }

        Ok(())
    }

    /// Phase 5: one discovery task per operation with no declared
    /// requirements yet. Returns how many ran.
    async fn requirement_discovery(&mut self) -> Result<u32, WorkflowError> {
        let candidates: Vec<Id> = {
            let head = self.engine().head_snapshot();

            head.workspace
                .operations
                .iter()
                .filter(|(_, draft)| requirements_empty(&draft.requirements))
                .map(|(id, _)| id.clone())
                .collect()
        };

        let mut ran = 0;

        for operation in candidates {
            self.scheduler
                .run(&LogicalTask {
                    kind: TaskKind::RequirementDiscovery,
                    objective: format!("Discover the correctness requirements of {operation}."),
                    write_scope: WriteScope::requirement_discovery(operation.clone()),
                    bundle: BundleSpec {
                        operation: Some(operation.clone()),
                        requirement: None,
                        include: Vec::new(),
                    },
                    prompt_evidence: Vec::new(),
                })
                .await?;

            ran += 1;
        }

        Ok(ran)
    }

    /// Phases 6–7: for every unproven obligation at `revision`, run one
    /// requirement-scoped repair task. Returns how many ran.
    async fn repair_unproven(&mut self, revision: Revision) -> Result<u32, WorkflowError> {
        let unproven = self.unproven_obligations(revision)?;

        let mut ran = 0;

        for target in unproven {
            self.scheduler
                .run(&LogicalTask {
                    kind: TaskKind::RequirementRepair,
                    objective: format!(
                        "Make the {} requirement #{} of {} provable.",
                        target.family, target.index, target.operation
                    ),
                    write_scope: WriteScope::requirement_repair(target.operation.clone()),
                    bundle: BundleSpec {
                        operation: Some(target.operation.clone()),
                        requirement: Some((target.family, target.index)),
                        include: Vec::new(),
                    },
                    prompt_evidence: Vec::new(),
                })
                .await?;

            ran += 1;
        }

        Ok(ran)
    }

    /// Waits for a revision's analysis to reach a terminal state,
    /// bounded by the configured timeout.
    async fn await_analysis(&self, revision: Revision) -> Result<AnalysisState, WorkflowError> {
        let state = self.engine().analysis_state(revision);

        if state.is_terminal() {
            return Ok(state);
        }

        tokio::time::timeout(self.config.analysis_timeout, self.engine().analysis_ready(revision))
            .await
            .map_err(|_| WorkflowError::AnalysisTimeout(revision))
    }

    fn missing_program_operations(&self) -> Vec<Id> {
        let head = self.engine().head_snapshot();

        head.workspace
            .operations
            .iter()
            .filter(|(_, draft)| draft.program.is_none() || draft.execution.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn operations_named_by(
        &self,
        errors: &[crate::confluence::AnalysisDiagnostic],
    ) -> Vec<Id> {
        let head = self.engine().head_snapshot();
        let mut named = Vec::new();

        for id in head.workspace.operations.keys() {
            if errors.iter().any(|error| {
                error.message.contains(&id.0) || error.subject.as_deref() == Some(&id.0)
            }) {
                named.push(id.clone());
            }
        }

        named
    }

    /// The unproven obligations at `revision`, as repair targets.
    fn unproven_obligations(&self, revision: Revision) -> Result<Vec<RepairTarget>, WorkflowError> {
        let AnalysisState::Ready(analysis) = self.engine().analysis_state(revision) else {
            return Ok(Vec::new());
        };

        let mut targets = Vec::new();

        for obligation in &analysis.obligations.obligations {
            if obligation.status != crate::analyzer::report::Status::Unknown {
                continue;
            }

            let crate::analyzer::report::Subject::Operation {
                operation,
                requirement: Some(index),
            } = &obligation.subject
            else {
                continue;
            };

            if let Some(family) = repair_family(&obligation.property) {
                targets.push(RepairTarget {
                    operation: operation.clone(),
                    family,
                    index: *index,
                });
            }
        }

        Ok(targets)
    }

    async fn incomplete(
        &mut self,
        revision: Revision,
        reason: String,
    ) -> Result<RunReport, WorkflowError> {
        let unresolved = self.unresolved_labels(revision);

        Ok(RunReport {
            status: RunStatus::Incomplete {
                revision: revision.0,
                reason,
                unresolved,
            },
            final_revision: revision.0,
            iterations: 0,
            artifacts: Vec::new(),
        })
    }

    /// Evaluates the success condition and writes the finalization
    /// artifacts (§76). When `revision` is `None`, the fixpoint budget
    /// was exhausted.
    async fn finalize(
        &mut self,
        iterations: u32,
        revision: Option<Revision>,
    ) -> Result<RunReport, WorkflowError> {
        let revision = revision.unwrap_or_else(|| self.engine().head_revision());

        let AnalysisState::Ready(analysis) = self.await_analysis(revision).await? else {
            return Ok(RunReport {
                status: RunStatus::Incomplete {
                    revision: revision.0,
                    reason: "the final head does not validate".to_string(),
                    unresolved: self.unresolved_labels(revision),
                },
                final_revision: revision.0,
                iterations,
                artifacts: Vec::new(),
            });
        };

        let unmapped = self.unmapped_obligations();
        let all_proven = analysis.verification.all_proven();

        let head = self.engine().head_snapshot();

        let model = head
            .workspace
            .assemble_model()
            .map_err(|error| WorkflowError::Finalization(error.to_string()))?;

        let status = if all_proven && unmapped.is_empty() {
            RunStatus::Success {
                revision: revision.0,
            }
        } else {
            let mut unresolved = self.unproven_labels(revision);

            unresolved.extend(unmapped);

            RunStatus::Incomplete {
                revision: revision.0,
                reason: if all_proven {
                    "explicit prompt obligations remain unmapped".to_string()
                } else {
                    "not every adopted obligation is proven".to_string()
                },
                unresolved,
            }
        };

        let artifacts = self
            .write_artifacts(&model, &analysis, &status)
            .map_err(WorkflowError::Finalization)?;

        Ok(RunReport {
            status,
            final_revision: revision.0,
            iterations,
            artifacts,
        })
    }

    fn write_artifacts(
        &self,
        model: &Model,
        analysis: &crate::confluence::AnalysisSnapshot,
        status: &RunStatus,
    ) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(&self.config.out_dir)
            .map_err(|error| format!("cannot create {}: {error}", self.config.out_dir.display()))?;

        let mut artifacts = Vec::new();

        let yaml_path = self.config.out_dir.join("conseqa.yaml");
        let yaml =
            serde_yaml::to_string(model).map_err(|error| format!("serialize model: {error}"))?;

        std::fs::write(&yaml_path, yaml)
            .map_err(|error| format!("write {}: {error}", yaml_path.display()))?;

        artifacts.push(yaml_path.display().to_string());

        let report_path = self.config.out_dir.join("verification-report.json");
        let report = serde_json::to_string_pretty(&analysis.obligations)
            .map_err(|error| format!("serialize report: {error}"))?;

        std::fs::write(&report_path, format!("{report}\n"))
            .map_err(|error| format!("write {}: {error}", report_path.display()))?;

        artifacts.push(report_path.display().to_string());

        let manifest = self.build_manifest(model.revision, status);
        let manifest_path = self.config.out_dir.join("confluence-manifest.json");
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("serialize manifest: {error}"))?;

        std::fs::write(&manifest_path, format!("{manifest_json}\n"))
            .map_err(|error| format!("write {}: {error}", manifest_path.display()))?;

        artifacts.push(manifest_path.display().to_string());

        Ok(artifacts)
    }

    fn build_manifest(&self, revision: Revision, status: &RunStatus) -> serde_json::Value {
        let head = self.engine().head_snapshot();

        let obligations: BTreeMap<String, String> = head
            .workspace
            .prompt_obligations
            .iter()
            .map(|(id, obligation)| {
                let status = match &obligation.status {
                    PromptObligationStatus::Unmapped => "unmapped".to_string(),
                    PromptObligationStatus::Mapped { requirements } => {
                        format!("mapped ({} requirements)", requirements.len())
                    }
                    PromptObligationStatus::UnsupportedByCurrentDsl { reason } => {
                        format!("unsupported: {reason}")
                    }
                    PromptObligationStatus::ExplicitlyWaivedByUser => "waived".to_string(),
                };

                (id.0.clone(), status)
            })
            .collect();

        serde_json::json!({
            "final_revision": revision.0,
            "run": head.workspace.run_meta.run.0,
            "backend": self.scheduler.backend_name(),
            "status": status,
            "prompt_obligations": obligations,
            "tasks": self.engine().list_tasks().len(),
        })
    }

    fn unmapped_obligations(&self) -> Vec<String> {
        let head = self.engine().head_snapshot();

        head.workspace
            .prompt_obligations
            .iter()
            .filter(|(_, obligation)| {
                matches!(obligation.status, PromptObligationStatus::Unmapped)
            })
            .map(|(id, _)| format!("prompt obligation {} is unmapped", id.0))
            .collect()
    }

    fn unproven_labels(&self, revision: Revision) -> Vec<String> {
        self.unproven_obligations(revision)
            .unwrap_or_default()
            .into_iter()
            .map(|target| {
                format!(
                    "{} requirement #{} of {} is unproven",
                    target.family, target.index, target.operation
                )
            })
            .collect()
    }

    fn unresolved_labels(&self, revision: Revision) -> Vec<String> {
        let mut labels = self.unproven_labels(revision);

        labels.extend(self.unmapped_obligations());

        labels
    }
}

/// One requirement to repair.
#[derive(Debug, Clone)]
struct RepairTarget {
    operation: Id,
    family: RequirementFamily,
    index: usize,
}

fn requirements_empty(requirements: &crate::spec::OperationRequirements) -> bool {
    requirements.serialization.is_empty()
        && requirements.ordering.is_empty()
        && requirements.idempotency.is_empty()
        && requirements.recoverability.is_empty()
}

fn repair_family(property: &crate::analyzer::report::Property) -> Option<RequirementFamily> {
    use crate::analyzer::report::Property;

    Some(match property {
        Property::Serialization => RequirementFamily::Serialization,
        Property::Ordering => RequirementFamily::Ordering,
        Property::Idempotency => RequirementFamily::Idempotency,
        Property::ResultReplay => RequirementFamily::ResultReplay,
        Property::Recoverability => RequirementFamily::Recoverability,
        Property::Custom { .. } => return None,
    })
}

fn describe_gaps(gaps: &[crate::confluence::AssemblyGap]) -> String {
    gaps.iter()
        .map(|gap| gap.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
