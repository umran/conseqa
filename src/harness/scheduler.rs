//! The scheduler: turning logical task specifications into completed
//! confluence commits, restarting a session when its task is
//! invalidated and running a fanout's tasks concurrently.
//!
//! `run_many` fans out a batch of tasks — the agents reason in
//! parallel, bounded by `max_concurrent_agents`, while their commits
//! serialize through the engine's single sequencer and each validates
//! against the live head (§28, §64). The scheduler owns the
//! work-assignment policy the spec calls for: one primary program
//! writer per operation (§65), and staged fanout behind decomposition
//! so OCC is an exception path rather than the coordination mechanism
//! (§96–§97). An invalidated task is terminal; its replacement is a new
//! task in a new session against a fresh snapshot (§2.6), and the
//! scheduler bounds how many times it will retry.

use std::collections::HashSet;

use parking_lot::Mutex;

use crate::confluence::{
    BundleSpec, ConfluenceEngine, CreateTask, EngineError, PromptEvidence, TaskBudget, TaskId,
    TaskKind, TaskState, WriteScope,
};
use crate::spec::Id;

use super::backend::InvocationBudget;
use super::supervisor::{SessionOutcome, Supervisor};
use super::task_prompt;

/// Releases an operation's advisory primary-writer claim on drop.
struct ProgramWriterGuard<'a> {
    writers: &'a Mutex<HashSet<Id>>,
    operation: Id,
}

impl Drop for ProgramWriterGuard<'_> {
    fn drop(&mut self) {
        self.writers.lock().remove(&self.operation);
    }
}

/// One logical unit of architecture work: its kind, objective, write
/// scope, and the context slice its bundle should cover.
#[derive(Debug, Clone)]
pub struct LogicalTask {
    pub kind: TaskKind,
    pub objective: String,
    pub write_scope: WriteScope,
    pub bundle: BundleSpec,
    pub prompt_evidence: Vec<PromptEvidence>,

    /// Whether the worker may commit repeatedly (building incrementally)
    /// rather than being held to a single patch. Used for decomposition,
    /// which authors a whole skeleton across several commits.
    pub interactive: bool,
}

/// Scheduler policy knobs.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerPolicy {
    /// How many fresh sessions a single logical task may consume before
    /// the scheduler gives up (invalidation restarts and process
    /// failures both count).
    pub max_attempts: u32,

    /// The maximum number of agent sessions running concurrently
    /// during a fanout (§88 `--max-agents`). Commits still serialize at
    /// the engine's single sequencer; this bounds how many agents
    /// *reason* at once.
    pub max_concurrent_agents: usize,

    pub task_budget: TaskBudget,
    pub invocation_budget: InvocationBudget,
}

impl Default for SchedulerPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            max_concurrent_agents: 4,
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
    /// writer per operation (§65). Behind a mutex so concurrent fanout
    /// shares it; the check is advisory — the OCC gate is the real
    /// safety net (§65), so distinct-operation tasks never contend.
    program_writers: Mutex<HashSet<crate::spec::Id>>,
}

impl Scheduler {
    pub fn new(engine: ConfluenceEngine, supervisor: Supervisor, policy: SchedulerPolicy) -> Self {
        Self {
            engine,
            supervisor,
            policy,
            program_writers: Mutex::new(HashSet::new()),
        }
    }

    pub fn engine(&self) -> &ConfluenceEngine {
        &self.engine
    }

    /// The name of the backend the supervisor drives, for manifests.
    pub fn backend_name(&self) -> &str {
        self.supervisor.backend_name()
    }

    /// Runs a batch of logical tasks concurrently, at most
    /// `max_concurrent_agents` at a time, and returns their runs. The
    /// agents *reason* in parallel; their commits still serialize
    /// through the engine's single sequencer, and each task validates
    /// against the live head at commit — so concurrent fanout over
    /// distinct operations never stale-commits (§28, §64).
    ///
    /// The first task to exhaust its attempts fails the batch, matching
    /// the sequential caller's error propagation.
    pub async fn run_many(
        &self,
        tasks: Vec<LogicalTask>,
    ) -> Result<Vec<TaskRun>, SchedulerError> {
        use futures::stream::{self, StreamExt};

        let concurrency = self.policy.max_concurrent_agents.max(1);

        let results: Vec<Result<TaskRun, SchedulerError>> = stream::iter(tasks)
            .map(|task| async move { self.run(&task).await })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        results.into_iter().collect()
    }

    /// Runs one logical task to a terminal confluence state, creating a
    /// fresh task and session each time an attempt is invalidated or
    /// its process fails, up to the policy's attempt bound.
    pub async fn run(&self, logical: &LogicalTask) -> Result<TaskRun, SchedulerError> {
        // Enforce one primary program writer per operation for
        // program-scope tasks; the OCC gate is the safety net, not the
        // work-assignment mechanism (§65). The guard releases the
        // operation on drop, even if the task panics.
        let _guard = self.claim_program_writer(logical);

        self.run_inner(logical).await
    }

    /// Advisory claim on an operation's primary-writer slot, released
    /// when the returned guard drops. `None` for non-program tasks.
    fn claim_program_writer(&self, logical: &LogicalTask) -> Option<ProgramWriterGuard<'_>> {
        let operation = self.program_write_target(logical)?;

        if !self.program_writers.lock().insert(operation.clone()) {
            tracing::warn!(
                %operation,
                "a primary program writer is already active for this operation; \
                 running anyway relies on OCC"
            );
        }

        Some(ProgramWriterGuard {
            writers: &self.program_writers,
            operation,
        })
    }

    async fn run_inner(&self, logical: &LogicalTask) -> Result<TaskRun, SchedulerError> {
        let mut attempts = Vec::new();

        for attempt in 0..self.policy.max_attempts {
            let params = CreateTask {
                kind: logical.kind,
                objective: logical.objective.clone(),
                write_scope: logical.write_scope.clone(),
                prompt_evidence: logical.prompt_evidence.clone(),
                budget: self.policy.task_budget,
            };

            // An interactive worker commits incrementally (rolling its
            // token on each commit); a single-patch worker commits once.
            let handle = if logical.interactive {
                self.engine.create_interactive_task(params)?
            } else {
                self.engine.create_task(params)?
            };

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
