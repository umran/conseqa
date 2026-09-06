//! The scheduler: turning a logical task specification into a
//! completed confluence commit, restarting in a fresh session when the
//! task is invalidated.
//!
//! The scheduler owns the work-assignment policy the spec calls for:
//! one primary program writer per operation (§65), and staged fanout
//! behind an interface epoch so OCC is an exception path rather than
//! the coordination mechanism (§96–§97). An invalidated task is
//! terminal; its replacement is a new task in a new session against a
//! fresh snapshot (§2.6), and the scheduler bounds how many times it
//! will retry.

use std::collections::HashSet;

use crate::confluence::{
    BundleSpec, ConfluenceEngine, CreateTask, EngineError, PromptEvidence, TaskBudget, TaskId,
    TaskKind, TaskState, WriteScope,
};

use super::backend::InvocationBudget;
use super::supervisor::{SessionOutcome, Supervisor};
use super::task_prompt;

/// One logical unit of architecture work: its kind, objective, write
/// scope, and the context slice its bundle should cover.
#[derive(Debug, Clone)]
pub struct LogicalTask {
    pub kind: TaskKind,
    pub objective: String,
    pub write_scope: WriteScope,
    pub bundle: BundleSpec,
    pub prompt_evidence: Vec<PromptEvidence>,
}

/// Scheduler policy knobs.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerPolicy {
    /// How many fresh sessions a single logical task may consume before
    /// the scheduler gives up (invalidation restarts and process
    /// failures both count).
    pub max_attempts: u32,

    pub task_budget: TaskBudget,
    pub invocation_budget: InvocationBudget,
}

impl Default for SchedulerPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            task_budget: TaskBudget::default(),
            invocation_budget: InvocationBudget::default(),
        }
    }
}

/// How a logical task ultimately ended, after any restarts.
#[derive(Debug, Clone)]
pub struct TaskRun {
    /// The id of the final task attempt.
    pub task: TaskId,
    pub final_state: TaskState,

    /// Every session outcome in order, including restarted ones.
    pub attempts: Vec<SessionOutcome>,
}

impl TaskRun {
    pub fn committed(&self) -> bool {
        self.final_state == TaskState::Committed
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error("task {task} exhausted its {attempts} attempts without a terminal outcome")]
    AttemptsExhausted { task: TaskId, attempts: u32 },
}

pub struct Scheduler {
    engine: ConfluenceEngine,
    supervisor: Supervisor,
    policy: SchedulerPolicy,

    /// Operations with an active primary program writer, enforcing one
    /// writer per operation (§65).
    program_writers: HashSet<crate::spec::Id>,
}

impl Scheduler {
    pub fn new(engine: ConfluenceEngine, supervisor: Supervisor, policy: SchedulerPolicy) -> Self {
        Self {
            engine,
            supervisor,
            policy,
            program_writers: HashSet::new(),
        }
    }

    pub fn engine(&self) -> &ConfluenceEngine {
        &self.engine
    }

    /// The name of the backend the supervisor drives, for manifests.
    pub fn backend_name(&self) -> &str {
        self.supervisor.backend_name()
    }

    /// Runs one logical task to a terminal confluence state, creating a
    /// fresh task and session each time an attempt is invalidated or
    /// its process fails, up to the policy's attempt bound.
    pub async fn run(&mut self, logical: &LogicalTask) -> Result<TaskRun, SchedulerError> {
        // Enforce one primary program writer per operation for
        // program-scope tasks; the OCC gate is the safety net, not the
        // work-assignment mechanism (§65).
        let guarded = self.program_write_target(logical);

        if let Some(operation) = &guarded
            && !self.program_writers.insert(operation.clone())
        {
            tracing::warn!(
                %operation,
                "a primary program writer is already active for this operation; \
                 running anyway relies on OCC"
            );
        }

        let result = self.run_inner(logical).await;

        if let Some(operation) = &guarded {
            self.program_writers.remove(operation);
        }

        result
    }

    async fn run_inner(&self, logical: &LogicalTask) -> Result<TaskRun, SchedulerError> {
        let mut attempts = Vec::new();

        for attempt in 0..self.policy.max_attempts {
            let handle = self.engine.create_task(CreateTask {
                kind: logical.kind,
                objective: logical.objective.clone(),
                write_scope: logical.write_scope.clone(),
                prompt_evidence: logical.prompt_evidence.clone(),
                budget: self.policy.task_budget,
            })?;

            // Build the tracked bundle against this attempt's pinned
            // snapshot, then render it into the prompt.
            let bundle = self.engine.context_bundle(handle.id, &logical.bundle)?;
            let prompt = task_prompt::build(logical.kind, &logical.objective, &bundle);

            tracing::info!(
                task = %handle.id,
                kind = %logical.kind,
                attempt,
                "running logical task"
            );

            let outcome = self
                .supervisor
                .run_task(&handle, prompt, self.policy.invocation_budget)
                .await;

            let needs_replacement = outcome.needs_replacement();
            let final_state = outcome.task_state;

            attempts.push(outcome);

            if !needs_replacement {
                return Ok(TaskRun {
                    task: handle.id,
                    final_state,
                    attempts,
                });
            }

            tracing::info!(
                task = %handle.id,
                attempt,
                "task attempt did not complete; starting a fresh session"
            );
        }

        // Attempts exhausted: report the last attempt's state.
        let last = attempts.last().expect("at least one attempt ran");

        Err(SchedulerError::AttemptsExhausted {
            task: last.task,
            attempts: self.policy.max_attempts,
        })
    }

    /// The operation a program-scope task writes, for one-writer
    /// bookkeeping.
    fn program_write_target(&self, logical: &LogicalTask) -> Option<crate::spec::Id> {
        use crate::confluence::WriteGrant;

        logical.write_scope.grants.iter().find_map(|grant| match grant {
            WriteGrant::OperationProgram(operation) | WriteGrant::Operation(operation) => {
                Some(operation.clone())
            }
            _ => None,
        })
    }
}
