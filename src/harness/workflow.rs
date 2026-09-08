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
    AnalysisState, BundleSpec, ConfluenceEngine, EvidenceRef, PromptEvidence,
    PromptObligationStatus, RequirementFamily, TaskKind, WriteScope,
};
use crate::analyzer::verification::RemedyLayer;
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

    /// Extra guidance for this run's workers, layered on top of the
    /// project prompt as additional prompt evidence — how an
    /// interactive caller of `request_design` steers a fanout.
    pub objective: Option<String>,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            out_dir: PathBuf::from("."),
            analysis_timeout: Duration::from_secs(120),
            max_iterations: 8,
            objective: None,
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
    pub async fn run(&self) -> Result<RunReport, WorkflowError> {
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
                    // targeted repair. Each operation an error names is
                    // repaired with the specific diagnostics as its
                    // obstacle, so the agent is told exactly what failed
                    // validation rather than resynthesizing it blind.
                    //
                    // A diagnostic against an L1 declaration names no
                    // operation and belongs to no operation's program,
                    // so it is routed to the topology author instead.
                    let (runtime, application) = self.split_diagnostics(&errors);
                    let operations = self.operations_named_by(&application);

                    if runtime.is_empty() && operations.is_empty() {
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

                    if !runtime.is_empty() {
                        self.repair_topology_validation(&runtime).await?;
                    }

                    if !operations.is_empty() {
                        self.repair_validation(&operations, &application).await?;
                    }

                    continue;
                }

                AnalysisState::Ready(_) => {
                    // Structurally valid. Phase 5: discover
                    // requirements for operations that have none yet.
                    // Reconverge only if discovery actually committed
                    // something — gating on tasks *run* re-runs discovery
                    // every iteration when no proposal is adoptable (a
                    // prompt with no explicit obligation under a policy
                    // that adopts none), spinning to the iteration bound
                    // instead of proceeding to verify and finalize.
                    let before = self.engine().head_revision();

                    self.requirement_discovery().await?;

                    if self.engine().head_revision() != before {
                        continue;
                    }

                    // Phase 6–7b: verify, then repair the unproven —
                    // runtime obstacles through the topology author,
                    // the rest per operation.
                    let repaired = self.repair_unproven(revision).await?;

                    if repaired == 0 {
                        // Nothing left to repair this iteration: either
                        // success or a stuck obstacle.
                        return self.finalize(iterations, Some(revision)).await;
                    }

                    if self.engine().head_revision() == before {
                        // Repair ran and committed nothing at all. The
                        // next iteration would build the same tasks
                        // from the same snapshot and reach the same
                        // place, so the obstacle is stuck: finalize now
                        // with the gaps preserved rather than spending
                        // the rest of the iteration budget on identical
                        // work. Per-task nondeterminism is the
                        // scheduler's retry policy to absorb, not the
                        // fixpoint loop's.
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

    /// The run's natural-language prompt as task evidence, so every
    /// worker is told what to build — plus the caller's run objective,
    /// when one was given. Without this a decompose worker has nothing
    /// to decompose.
    fn prompt_evidence(&self) -> Vec<PromptEvidence> {
        let mut evidence: Vec<PromptEvidence> = self
            .engine()
            .head_snapshot()
            .workspace
            .run_meta
            .prompt
            .clone()
            .map(|prompt| {
                vec![PromptEvidence {
                    source: EvidenceRef("run.prompt".to_string()),
                    excerpt: prompt,
                }]
            })
            .unwrap_or_default();

        if let Some(objective) = &self.config.objective {
            evidence.push(PromptEvidence {
                source: EvidenceRef("run.objective".to_string()),
                excerpt: objective.clone(),
            });
        }

        evidence
    }

    async fn decompose(&self) -> Result<(), WorkflowError> {
        // Only decompose an empty head; an adopted model skips
        // straight to convergence.
        if !self.engine().head_snapshot().workspace.operations.is_empty() {
            return Ok(());
        }

        let objective =
            "Decompose the application prompt into the shared architecture skeleton: \
             services, schemas, data models, topics, state machines, one interface per \
             planned operation, and the explicit prompt obligations. Commit one patch."
                .to_string();

        self.scheduler
            .run(&LogicalTask {
                kind: TaskKind::Decompose,
                objective,
                write_scope: WriteScope::shared_skeleton(),
                bundle: BundleSpec::default(),
                prompt_evidence: self.prompt_evidence(),
                // Decomposition builds a whole skeleton, so let it commit
                // incrementally rather than in a single locked-in patch.
                interactive: true,
            })
            .await?;

        Ok(())
    }

    async fn operation_fanout(&self) -> Result<(), WorkflowError> {
        let planned = self.missing_program_operations();

        self.synthesize_operations(&planned).await
    }

    /// Phase 3: synthesize the given operations concurrently (§64).
    /// Each task writes only its own operation's program and execution
    /// facts and reads shared symbols frozen for the duration of the
    /// fanout, so the sessions reason in parallel without contending.
    async fn synthesize_operations(&self, operations: &[Id]) -> Result<(), WorkflowError> {
        let tasks: Vec<LogicalTask> = operations
            .iter()
            .map(|operation| LogicalTask {
                kind: TaskKind::OperationSynthesis,
                objective: format!("Synthesize the program of {operation}."),
                write_scope: WriteScope::operation_synthesis(operation.clone()),
                bundle: BundleSpec {
                    operation: Some(operation.clone()),
                    requirement: None,
                    include: Vec::new(),
                },
                prompt_evidence: self.prompt_evidence(),
                interactive: false,
            })
            .collect();

        self.scheduler.run_many(tasks).await?;

        Ok(())
    }

    /// Phase 4 repair: for every operation a structural diagnostic names,
    /// run one repair task carrying the specific diagnostics as its
    /// obstacle. The agent is told exactly what failed validation and
    /// revises the program to resolve it — the structural counterpart of
    /// requirement-scoped repair, closing the asymmetry where only
    /// verification obstacles were fed back precisely. Returns how many
    /// ran.
    async fn repair_validation(
        &self,
        operations: &[Id],
        errors: &[crate::confluence::AnalysisDiagnostic],
    ) -> Result<u32, WorkflowError> {
        let tasks: Vec<LogicalTask> = operations
            .iter()
            .map(|operation| {
                let obstacle = errors
                    .iter()
                    .filter(|error| {
                        error.message.contains(&operation.0)
                            || error.subject.as_deref() == Some(&operation.0)
                    })
                    .map(|error| format!("- {}", error.message))
                    .collect::<Vec<_>>()
                    .join("\n");

                LogicalTask {
                    kind: TaskKind::OperationSynthesis,
                    objective: format!(
                        "The program of {operation} does not pass structural validation. \
                         Revise the program so these obstacles are resolved, keeping the \
                         operation's interface stable:\n{obstacle}"
                    ),
                    write_scope: WriteScope::operation_synthesis(operation.clone()),
                    bundle: BundleSpec {
                        operation: Some(operation.clone()),
                        requirement: None,
                        include: Vec::new(),
                    },
                    prompt_evidence: self.prompt_evidence(),
                    interactive: false,
                }
            })
            .collect();

        let ran = tasks.len() as u32;

        self.scheduler.run_many(tasks).await?;

        Ok(ran)
    }

    /// Splits validation diagnostics into the L1 ones and the rest.
    ///
    /// Two signals mark an L1 diagnostic. A code raised only by runtime
    /// validation is one outright. A generic code — an unknown
    /// reference, an invalid field path — can come from either layer,
    /// so it counts only when its subject is a symbol that exists
    /// nowhere but the runtime model: a router, an execution pool, or a
    /// storage layout. Topic ids are deliberately not runtime subjects,
    /// since a topic is an L0 declaration and its runtime-specific
    /// faults already carry L1 codes.
    ///
    /// The split is exclusive because `operations_named_by` falls back
    /// to a substring match on the message, and an L1 diagnostic
    /// routinely names the operation whose boundary it concerns. Left
    /// in, it would spawn a program repair for an obstacle no program
    /// can reach.
    fn split_diagnostics(
        &self,
        errors: &[crate::confluence::AnalysisDiagnostic],
    ) -> (
        Vec<crate::confluence::AnalysisDiagnostic>,
        Vec<crate::confluence::AnalysisDiagnostic>,
    ) {
        let head = self.engine().head_snapshot();
        let runtime = &head.workspace.runtime;

        let runtime_owned = |subject: &str| {
            let id = Id(subject.to_string());

            runtime.routers.contains_key(&id)
                || runtime.execution_pools.contains_key(&id)
                || runtime.storage_layouts.contains_key(&id)
        };

        errors.iter().cloned().partition(|error| {
            error.runtime || error.subject.as_deref().is_some_and(runtime_owned)
        })
    }

    /// Sends the L1 validation obstacles to the single topology
    /// author. Returns how many tasks ran.
    async fn repair_topology_validation(
        &self,
        errors: &[crate::confluence::AnalysisDiagnostic],
    ) -> Result<u32, WorkflowError> {
        let obstacle = errors
            .iter()
            .map(|error| format!("- {}", error.message))
            .collect::<Vec<_>>()
            .join("\n");

        let task = LogicalTask {
            kind: TaskKind::TopologySynthesis,
            objective: format!(
                "The runtime topology does not pass structural validation. Revise \
                 the L1 declarations so these obstacles are resolved, leaving the \
                 application model unchanged:\n{obstacle}"
            ),
            write_scope: WriteScope::runtime_topology(),
            bundle: BundleSpec {
                operation: None,
                requirement: None,
                include: crate::confluence::topology_symbols(
                    &self.engine().head_snapshot().workspace,
                ),
            },
            prompt_evidence: self.prompt_evidence(),
            interactive: false,
        };

        self.scheduler.run_many(vec![task]).await?;

        Ok(1)
    }

    /// Phase 5: one discovery task per operation with no declared
    /// requirements yet, run concurrently. Returns how many ran.
    async fn requirement_discovery(&self) -> Result<u32, WorkflowError> {
        let candidates: Vec<Id> = {
            let head = self.engine().head_snapshot();

            head.workspace
                .operations
                .iter()
                .filter(|(_, draft)| requirements_empty(&draft.requirements))
                .map(|(id, _)| id.clone())
                .collect()
        };

        let tasks: Vec<LogicalTask> = candidates
            .into_iter()
            .map(|operation| LogicalTask {
                kind: TaskKind::RequirementDiscovery,
                objective: format!("Discover the correctness requirements of {operation}."),
                write_scope: WriteScope::requirement_discovery(operation.clone()),
                bundle: BundleSpec {
                    operation: Some(operation),
                    requirement: None,
                    include: Vec::new(),
                },
                prompt_evidence: self.prompt_evidence(),
                interactive: false,
            })
            .collect();

        let ran = tasks.len() as u32;

        self.scheduler.run_many(tasks).await?;

        Ok(ran)
    }

    /// Phases 6–7: repair every unproven obligation at `revision`.
    ///
    /// Obligations split by the layer their obstacles name. Those
    /// waiting on the runtime realization go to a single topology task
    /// holding the whole L1 grant — one writer, because a grouping key,
    /// its router and the pool it terminates at are one decision.
    /// Everything else fans out per operation as before.
    ///
    /// Topology goes first, and a topology commit ends the round: it
    /// moves the head, which leaves both the unproven set and its
    /// remedy classification stale. An obligation classified
    /// `application` because one of its obstacles was an L0 one may
    /// have had its runtime obstacles cleared in passing, or not — and
    /// a repair task created now would pin a snapshot whose analysis
    /// has not run, so it would carry no obstacle evidence either.
    /// Re-verifying first costs one loop iteration and repairs against
    /// facts that are actually current.
    ///
    /// Returns how many tasks ran.
    async fn repair_unproven(&self, revision: Revision) -> Result<u32, WorkflowError> {
        let (runtime, application): (Vec<RepairTarget>, Vec<RepairTarget>) = self
            .unproven_obligations(revision)?
            .into_iter()
            .partition(RepairTarget::is_runtime);

        let mut ran = 0;

        if !runtime.is_empty() {
            let before = self.engine().head_revision();

            ran += self.synthesize_topology(&runtime).await?;

            if self.engine().head_revision() != before {
                return Ok(ran);
            }

            // The author declined to change anything. Fall through:
            // the application repairs may still make progress, and
            // without them a run whose topology is genuinely finished
            // would stop at the no-progress check with L0 work left.
        }

        let tasks: Vec<LogicalTask> = application
            .into_iter()
            .map(|target| LogicalTask {
                kind: TaskKind::RequirementRepair,
                objective: format!("Make the {} provable.", target.label()),
                write_scope: WriteScope::requirement_repair(target.operation.clone()),
                bundle: BundleSpec {
                    operation: Some(target.operation),
                    requirement: Some((target.family, target.index)),
                    include: Vec::new(),
                },
                prompt_evidence: self.prompt_evidence(),
                interactive: false,
            })
            .collect();

        ran += tasks.len() as u32;

        self.scheduler.run_many(tasks).await?;

        Ok(ran)
    }

    /// Runs the single L1 author over the obligations waiting on it.
    ///
    /// One task, not one per obligation: the runtime model is shared,
    /// and two concurrent writers would each see the other's pool and
    /// router declarations as conflicting writes. Batching them also
    /// lets the author declare one grouping that discharges several
    /// requirements at once, which per-obligation tasks cannot see.
    async fn synthesize_topology(&self, targets: &[RepairTarget]) -> Result<u32, WorkflowError> {
        let listed = targets
            .iter()
            .map(|target| format!("- the {}", target.label()))
            .collect::<Vec<_>>()
            .join("\n");

        let task = LogicalTask {
            kind: TaskKind::TopologySynthesis,
            objective: format!(
                "Author the runtime topology that discharges these obligations, \
                 which are unproven for want of L1 facts alone:\n{listed}\n\n\
                 Read each one's `requirement_report` for the specific missing \
                 fact. Leave unproven anything the architecture does not \
                 genuinely constrain."
            ),
            write_scope: WriteScope::runtime_topology(),
            bundle: BundleSpec {
                operation: None,
                requirement: None,
                include: crate::confluence::topology_symbols(
                    &self.engine().head_snapshot().workspace,
                ),
            },
            prompt_evidence: self.prompt_evidence(),
            interactive: false,
        };

        self.scheduler.run_many(vec![task]).await?;

        Ok(1)
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
            .filter(|(_, draft)| draft.program.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn operations_named_by(
        &self,
        errors: &[crate::confluence::AnalysisDiagnostic],
    ) -> Vec<Id> {
        let head = self.engine().head_snapshot();
        let mut named = Vec::new();

        for (id, draft) in &head.workspace.operations {
            // A structural diagnostic usually names an effect or
            // transaction id, not the operation, so match against every
            // symbol the operation owns — otherwise the obstacle reaches
            // no repair task.
            let owned = operation_owned_ids(id, draft);

            let matches = errors.iter().any(|error| {
                error
                    .subject
                    .as_deref()
                    .is_some_and(|subject| owned.contains(subject))
                    || error.message.contains(&id.0)
            });

            if matches {
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
                    remedy: obligation.remedy,
                });
            }
        }

        Ok(targets)
    }

    async fn incomplete(
        &self,
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
        &self,
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

        // A model with no operations is vacuously "all proven" — but the
        // workers built nothing. That is not success; report it as such
        // so an empty run cannot masquerade as a completed design.
        let built_nothing = model.operations.is_empty();

        let status = if built_nothing {
            RunStatus::Incomplete {
                revision: revision.0,
                reason: "no operations were synthesized: the decomposition and fanout \
                         produced no architecture. The worker agents likely could not run \
                         (check their availability and authentication) or committed nothing."
                    .to_string(),
                unresolved: vec!["the model has no operations".to_string()],
            }
        } else if all_proven && unmapped.is_empty() {
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

    /// Which layer the checker says the missing facts belong to.
    /// `None` for families that do not classify their obstacles, read
    /// as the application layer — the pre-existing behavior.
    remedy: Option<RemedyLayer>,
}

impl RepairTarget {
    /// Whether this obligation is waiting on the runtime topology
    /// alone, so no program edit can discharge it.
    fn is_runtime(&self) -> bool {
        self.remedy == Some(RemedyLayer::Runtime)
    }

    fn label(&self) -> String {
        format!("{} requirement #{} of {}", self.family, self.index, self.operation)
    }
}

/// Every symbol id an operation owns: the operation and its inputs, plus
/// the transactions, effects, intent and output bindings, and reads its
/// program declares. A structural diagnostic's subject is usually one of
/// these (an effect or transaction id, not the operation), so matching
/// against the owned set attributes the obstacle to the operation that
/// must repair it.
fn operation_owned_ids(
    operation: &Id,
    draft: &crate::confluence::DraftOperation,
) -> std::collections::BTreeSet<String> {
    use crate::spec::{OperationStep, TransactionStep};

    let mut owned = std::collections::BTreeSet::new();

    owned.insert(operation.0.clone());

    for input in draft.inputs.keys() {
        owned.insert(input.0.clone());
    }

    let Some(program) = &draft.program else {
        return owned;
    };

    for (_, step) in program.steps_with_locations() {
        match step {
            OperationStep::Transaction(transaction) => {
                owned.insert(transaction.id.0.clone());

                for inner in &transaction.steps {
                    match inner {
                        TransactionStep::Read(read) => {
                            owned.insert(read.bind.0.clone());
                        }
                        TransactionStep::EstablishEffectIntent(establish) => {
                            owned.insert(establish.effect_id.0.clone());
                            owned.insert(establish.bind.0.clone());
                        }
                        TransactionStep::EstablishTransactionOutput(establish) => {
                            owned.insert(establish.bind.0.clone());
                        }
                        TransactionStep::Transition(transition) => {
                            for intent in transition.effect_intents.values() {
                                owned.insert(intent.bind.0.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }

            OperationStep::ExecuteEffect(execute) => {
                owned.insert(execute.effect_id.0.clone());

                if let Some(bind) = &execute.bind {
                    owned.insert(bind.0.clone());
                }
            }

            OperationStep::ExecuteEffectIntent(execute) => {
                if let Some(bind) = &execute.bind {
                    owned.insert(bind.0.clone());
                }
            }

            _ => {}
        }
    }

    owned
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

#[cfg(test)]
mod tests {
    use super::operation_owned_ids;
    use crate::confluence::{DraftOperation, OperationInterfaceDraft};
    use crate::spec::{
        Derivation, Effect, EstablishEffectIntent, EstablishTransactionOutput, ExecuteEffectIntent,
        Id, IdempotencyGuarantee, OperationBlock, OperationStep, PublicationEffect, ResultOutcome,
        Return, Transaction, TransactionIsolation, TransactionStep,
    };
    use std::collections::BTreeMap;

    fn id(text: &str) -> Id {
        Id(text.to_string())
    }

    // A structural diagnostic's subject is an owned symbol id — an effect
    // or transaction — not the operation id, so attribution must resolve
    // those to the owning operation for the obstacle to reach a repair
    // task. This pins that the owned set covers a program's declared
    // transactions, effects, and bindings.
    #[test]
    fn owned_ids_cover_a_programs_declared_symbols() {
        let mut draft = DraftOperation::planned(OperationInterfaceDraft {
            service: id("service.x"),
            description: None,
            inputs: BTreeMap::new(),
        });

        draft.program = Some(OperationBlock {
            steps: vec![
                OperationStep::Transaction(Transaction {
                    id: id("tx.echo.write"),
                    data_model: None,
                    isolation: TransactionIsolation::ReadCommitted,
                    idempotency: IdempotencyGuarantee::NotDeduplicated,
                    steps: vec![
                        TransactionStep::EstablishTransactionOutput(EstablishTransactionOutput {
                            bind: id("output.echo"),
                            schema: id("schema.Result"),
                            values: Derivation::Unspecified,
                        }),
                        TransactionStep::EstablishEffectIntent(EstablishEffectIntent {
                            bind: id("intent.echo.notify"),
                            effect_id: id("effect.echo.notify"),
                            effect: Effect::Publication(PublicationEffect {
                                topic: id("topic.events"),
                                schema: id("schema.Event"),
                                idempotency_key_propagation: Vec::new(),
                            }),
                            values: Derivation::Unspecified,
                        }),
                    ],
                }),
                OperationStep::ExecuteEffectIntent(ExecuteEffectIntent {
                    intent: id("intent.echo.notify"),
                    bind: None,
                }),
                OperationStep::Return(Return {
                    request: id("input.echo.request"),
                    outcome: ResultOutcome::Ok {
                        values: Derivation::Unspecified,
                    },
                }),
            ],
        });

        let owned = operation_owned_ids(&id("operation.echo"), &draft);

        for expected in [
            "operation.echo",
            "tx.echo.write",
            "output.echo",
            "intent.echo.notify",
            "effect.echo.notify",
        ] {
            assert!(
                owned.contains(expected),
                "owned ids should include `{expected}`, got: {owned:?}"
            );
        }
    }
}
