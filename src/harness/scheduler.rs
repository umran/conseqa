//! The scheduler: turning logical task specifications into completed
//! confluence commits, restarting a session when its task is
//! invalidated and running a fanout's tasks concurrently.
//!
//! `run_many` fans out a batch of tasks in conflict-free waves — the
//! agents of one wave reason in parallel, bounded by
//! `max_concurrent_agents`, while their commits serialize through the
//! engine's single sequencer and each validates against the live head
//! (§28, §64). Waves are planned from the statically predictable
//! footprints: write scopes are declared, and the observations a
//! task's context bundle will record are computable from the head
//! before anything launches — so tasks that would invalidate each
//! other never run concurrently, and OCC is an exception path for
//! voluntary reads rather than the coordination mechanism (§96–§97).
//! The scheduler also owns one primary program writer per operation
//! (§65). An invalidated task is terminal; its replacement is a new
//! task in a new session against a fresh snapshot (§2.6) that inherits
//! the predecessor's rejected patch and invalidation causes as data —
//! a warm restart — and the scheduler bounds how many times it will
//! retry.

use std::collections::{BTreeSet, HashSet, VecDeque};

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use crate::confluence::{
    AnalysisState, BundleSpec, ConfluenceEngine, CreateTask, EngineError, InvalidationNote,
    PromptEvidence, SymbolKey, TaskBudget, TaskId, TaskKind, TaskState, WriteGrant, WriteScope,
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

    /// The attempt bound was reached without a terminal outcome. A
    /// recorded fact, not a batch-aborting error: the workflow's
    /// fixpoint re-enumerates unfinished work from the head, so one
    /// stuck task must not discard its siblings' committed progress.
    pub exhausted: bool,
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

    /// Runs a batch of logical tasks in conflict-free waves: within a
    /// wave, at most `max_concurrent_agents` agents reason in
    /// parallel; waves run in sequence. Two tasks share a wave only
    /// when neither's statically predicted footprint — its write
    /// scope, and the observations its bundle will record — touches
    /// the other's writes, so mutual invalidation cannot arise from
    /// scheduled context, only from a task's own voluntary reads. The
    /// commits still serialize through the engine's single sequencer
    /// and each validates against the live head (§28, §64).
    ///
    /// Runs are returned in submission order. A task that exhausts its
    /// attempts is reported in its `TaskRun`, never as a batch error.
    pub async fn run_many(
        &self,
        tasks: Vec<LogicalTask>,
    ) -> Result<Vec<TaskRun>, SchedulerError> {
        use futures::stream::{self, StreamExt};

        let concurrency = self.policy.max_concurrent_agents.max(1);

        let snapshot = self.engine.head_snapshot();

        let callees: FxHashMap<Id, Vec<Id>> = snapshot
            .graph
            .indexes
            .callees
            .iter()
            .map(|(operation, edges)| {
                (
                    operation.clone(),
                    edges.iter().map(|edge| edge.target.clone()).collect(),
                )
            })
            .collect();

        // Bundles record callee programs and requirements only when
        // the head's analysis is ready with summaries; before that a
        // callee enters as its interface, which peers never write.
        let deep_reads = matches!(
            self.engine.analysis_state(snapshot.revision),
            AnalysisState::Ready(_)
        );

        let waves = plan_waves(&tasks, &callees, deep_reads);

        if waves.len() > 1 {
            tracing::info!(
                tasks = tasks.len(),
                waves = waves.len(),
                "batch planned into conflict-free waves"
            );
        }

        let mut runs: Vec<Option<TaskRun>> = tasks.iter().map(|_| None).collect();

        for wave in waves {
            let results: Vec<(usize, Result<TaskRun, SchedulerError>)> = stream::iter(wave)
                .map(|index| {
                    let task = &tasks[index];

                    async move { (index, self.run(task).await) }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;

            for (index, result) in results {
                runs[index] = Some(result?);
            }
        }

        Ok(runs
            .into_iter()
            .map(|run| run.expect("every task was planned into exactly one wave"))
            .collect())
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

        // What a replacement inherits from its invalidated predecessor:
        // the causes, and the rejected patch when one was submitted.
        let mut inherited: Option<InvalidationNote> = None;

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
            // snapshot. A warm restart also bundles every symbol the
            // predecessor's patch references, so the inherited draft
            // can be judged against fresh, observed copies without
            // tripping read-before-reference on resubmission.
            let mut bundle_spec = logical.bundle.clone();

            if let Some(patch) = inherited
                .as_ref()
                .and_then(|note| note.rejected_patch.as_ref())
            {
                bundle_spec.include.extend(patch.external_references());
            }

            let bundle = self.engine.context_bundle(handle.id, &bundle_spec)?;
            let mut prompt = task_prompt::build(logical.kind, &logical.objective, &bundle);

            if let Some(note) = &inherited {
                prompt.push_str(&task_prompt::render_predecessor(note));
            }

            tracing::info!(
                task = %handle.id,
                kind = %logical.kind,
                attempt,
                warm = inherited.is_some(),
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
                    exhausted: false,
                });
            }

            inherited = self.engine.invalidation_note(handle.id);

            tracing::info!(
                task = %handle.id,
                attempt,
                salvaged_patch = inherited
                    .as_ref()
                    .is_some_and(|note| note.rejected_patch.is_some()),
                "task attempt did not complete; starting a fresh session"
            );
        }

        // Attempts exhausted: a recorded outcome, not a batch-aborting
        // error. The workflow re-enumerates unfinished work from the
        // head, so siblings' committed progress survives one stuck
        // task.
        let (task, final_state) = {
            let last = attempts.last().expect("at least one attempt ran");

            (last.task, last.task_state)
        };

        tracing::warn!(
            %task,
            attempts = self.policy.max_attempts,
            "logical task exhausted its attempts without a terminal outcome"
        );

        Ok(TaskRun {
            task,
            final_state,
            attempts,
            exhausted: true,
        })
    }

    /// The operation a program-scope task writes, for one-writer
    /// bookkeeping.
    fn program_write_target(&self, logical: &LogicalTask) -> Option<crate::spec::Id> {
        logical.write_scope.grants.iter().find_map(|grant| match grant {
            WriteGrant::OperationProgram(operation) | WriteGrant::Operation(operation) => {
                Some(operation.clone())
            }
            _ => None,
        })
    }
}

/// The statically predictable read/write footprint of one logical
/// task: its declared write targets, and the observations its context
/// bundle will record when it launches. Voluntary tool reads made
/// during the session are not predictable and stay OCC's business.
struct TaskFootprint {
    /// Holds a grant that can touch arbitrary shared symbols
    /// (`All`, `SharedSkeleton`, `RuntimeTopology`); conflicts with
    /// everything, so it runs in a wave of its own.
    global: bool,

    writes: BTreeSet<SymbolKey>,
    reads: BTreeSet<SymbolKey>,
}

fn footprint(
    task: &LogicalTask,
    callees: &FxHashMap<Id, Vec<Id>>,
    deep_reads: bool,
) -> TaskFootprint {
    let mut global = false;
    let mut writes = BTreeSet::new();

    for grant in &task.write_scope.grants {
        match grant {
            WriteGrant::All | WriteGrant::SharedSkeleton | WriteGrant::RuntimeTopology => {
                global = true;
            }

            WriteGrant::TopLevelSymbol(key) => {
                writes.insert(key.clone());
            }

            WriteGrant::Operation(operation) => {
                writes.insert(SymbolKey::OperationInterface(operation.clone()));
                writes.insert(SymbolKey::OperationProgram(operation.clone()));
                writes.insert(SymbolKey::OperationRequirements(operation.clone()));
            }

            WriteGrant::OperationProgram(operation) => {
                writes.insert(SymbolKey::OperationProgram(operation.clone()));
            }

            WriteGrant::OperationRequirements(operation) => {
                writes.insert(SymbolKey::OperationRequirements(operation.clone()));
            }

            WriteGrant::OperationInterface(operation) => {
                writes.insert(SymbolKey::OperationInterface(operation.clone()));
            }
        }
    }

    let mut reads: BTreeSet<SymbolKey> = task.bundle.include.iter().cloned().collect();

    if let Some(operation) = &task.bundle.operation {
        reads.insert(SymbolKey::OperationInterface(operation.clone()));
        reads.insert(SymbolKey::OperationProgram(operation.clone()));
        reads.insert(SymbolKey::OperationRequirements(operation.clone()));

        for callee in callees.get(operation).into_iter().flatten() {
            reads.insert(SymbolKey::OperationInterface(callee.clone()));

            // With analysis ready, the bundle serves the callee's
            // summary and conservatively records its program and
            // requirements as observations (§41 V1).
            if deep_reads {
                reads.insert(SymbolKey::OperationProgram(callee.clone()));
                reads.insert(SymbolKey::OperationRequirements(callee.clone()));
            }
        }
    }

    TaskFootprint {
        global,
        writes,
        reads,
    }
}

fn conflicts(a: &TaskFootprint, b: &TaskFootprint) -> bool {
    if a.global || b.global {
        return true;
    }

    !a.writes.is_disjoint(&b.writes)
        || !a.reads.is_disjoint(&b.writes)
        || !b.reads.is_disjoint(&a.writes)
}

/// Topological order over the batch's operation-scoped tasks, callees
/// first: a caller then lands in a later wave than a callee it
/// observes, and works against the callee's *final* state instead of
/// racing it. Call cycles and non-operation tasks keep submission
/// order.
fn callee_first_order(tasks: &[LogicalTask], callees: &FxHashMap<Id, Vec<Id>>) -> Vec<usize> {
    let owner: FxHashMap<&Id, usize> = tasks
        .iter()
        .enumerate()
        .filter_map(|(index, task)| task.bundle.operation.as_ref().map(|op| (op, index)))
        .collect();

    let mut indegree = vec![0usize; tasks.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); tasks.len()];

    for (caller, task) in tasks.iter().enumerate() {
        let Some(operation) = &task.bundle.operation else {
            continue;
        };

        for callee in callees.get(operation).into_iter().flatten() {
            if let Some(&callee_task) = owner.get(callee)
                && callee_task != caller
            {
                dependents[callee_task].push(caller);
                indegree[caller] += 1;
            }
        }
    }

    let mut ready: VecDeque<usize> = (0..tasks.len())
        .filter(|index| indegree[*index] == 0)
        .collect();

    let mut order = Vec::with_capacity(tasks.len());
    let mut placed = vec![false; tasks.len()];

    while let Some(index) = ready.pop_front() {
        placed[index] = true;
        order.push(index);

        for &dependent in &dependents[index] {
            indegree[dependent] -= 1;

            if indegree[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }

    // Whatever remains sits on a call cycle; append it in submission
    // order and let the conflict check place it.
    order.extend(
        placed
            .iter()
            .enumerate()
            .filter(|(_, placed)| !**placed)
            .map(|(index, _)| index),
    );

    order
}

/// Greedy wave assignment over the callee-first order: each task joins
/// the earliest wave containing nothing it conflicts with. Within a
/// wave, mutual invalidation cannot arise from scheduled context;
/// across waves, a later task launches only after the commits it would
/// have raced are in its snapshot.
fn plan_waves(
    tasks: &[LogicalTask],
    callees: &FxHashMap<Id, Vec<Id>>,
    deep_reads: bool,
) -> Vec<Vec<usize>> {
    let footprints: Vec<TaskFootprint> = tasks
        .iter()
        .map(|task| footprint(task, callees, deep_reads))
        .collect();

    let mut waves: Vec<Vec<usize>> = Vec::new();

    for index in callee_first_order(tasks, callees) {
        let fitting = waves.iter_mut().find(|wave| {
            wave.iter()
                .all(|&placed| !conflicts(&footprints[index], &footprints[placed]))
        });

        match fitting {
            Some(wave) => wave.push(index),
            None => waves.push(vec![index]),
        }
    }

    waves
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> Id {
        Id(text.to_string())
    }

    fn op_task(operation: &str, scope: WriteScope) -> LogicalTask {
        LogicalTask {
            kind: TaskKind::OperationSynthesis,
            objective: format!("work {operation}"),
            write_scope: scope,
            bundle: BundleSpec {
                operation: Some(id(operation)),
                requirements: Vec::new(),
                include: Vec::new(),
            },
            prompt_evidence: Vec::new(),
            interactive: false,
        }
    }

    fn calls(edges: &[(&str, &str)]) -> FxHashMap<Id, Vec<Id>> {
        let mut map: FxHashMap<Id, Vec<Id>> = FxHashMap::default();

        for (caller, callee) in edges {
            map.entry(id(caller)).or_default().push(id(callee));
        }

        map
    }

    fn all_indices(waves: &[Vec<usize>], count: usize) -> bool {
        let mut seen: Vec<usize> = waves.iter().flatten().copied().collect();

        seen.sort_unstable();

        seen == (0..count).collect::<Vec<_>>()
    }

    // The initial synthesis fanout is conflict-free by construction:
    // disjoint write scopes, and callees enter bundles as interfaces
    // only while analysis is not ready. Everything must share one wave
    // — otherwise wave planning would destroy the parallelism the
    // fanout exists for.
    #[test]
    fn disjoint_synthesis_tasks_share_one_wave() {
        let tasks = vec![
            op_task("operation.a", WriteScope::operation_synthesis(id("operation.a"))),
            op_task("operation.b", WriteScope::operation_synthesis(id("operation.b"))),
            op_task("operation.c", WriteScope::operation_synthesis(id("operation.c"))),
        ];

        let waves = plan_waves(&tasks, &calls(&[("operation.a", "operation.b")]), false);

        assert_eq!(waves.len(), 1, "{waves:?}");
        assert!(all_indices(&waves, 3));
    }

    // With analysis ready, a caller's bundle records its callee's
    // program and requirements — so a repair of the caller must not
    // run concurrently with a repair of the callee, and the callee
    // goes first so the caller works against its final state.
    #[test]
    fn a_caller_repairs_after_the_callee_it_observes() {
        let tasks = vec![
            op_task("operation.caller", WriteScope::requirement_repair(id("operation.caller"))),
            op_task("operation.callee", WriteScope::requirement_repair(id("operation.callee"))),
        ];

        let waves = plan_waves(
            &tasks,
            &calls(&[("operation.caller", "operation.callee")]),
            true,
        );

        assert_eq!(waves.len(), 2, "{waves:?}");
        assert_eq!(waves[0], vec![1], "the callee's task goes first: {waves:?}");
        assert_eq!(waves[1], vec![0]);
    }

    // The same pair without ready analysis observes interfaces only,
    // which repairs never write — one wave.
    #[test]
    fn shallow_reads_leave_a_call_pair_concurrent() {
        let tasks = vec![
            op_task("operation.caller", WriteScope::requirement_repair(id("operation.caller"))),
            op_task("operation.callee", WriteScope::requirement_repair(id("operation.callee"))),
        ];

        let waves = plan_waves(
            &tasks,
            &calls(&[("operation.caller", "operation.callee")]),
            false,
        );

        assert_eq!(waves.len(), 1, "{waves:?}");
    }

    // Two writers of one program are a guaranteed write-write conflict
    // and must serialize, whatever their bundles say.
    #[test]
    fn same_program_writers_never_share_a_wave() {
        let tasks = vec![
            op_task("operation.x", WriteScope::requirement_repair(id("operation.x"))),
            op_task("operation.x", WriteScope::requirement_repair(id("operation.x"))),
        ];

        let waves = plan_waves(&tasks, &FxHashMap::default(), false);

        assert_eq!(waves.len(), 2, "{waves:?}");
        assert!(all_indices(&waves, 2));
    }

    // A grant over arbitrary shared symbols (the topology author, the
    // decomposer) conflicts with everything: it runs alone.
    #[test]
    fn a_global_scope_runs_alone() {
        let topology = LogicalTask {
            kind: TaskKind::TopologySynthesis,
            objective: "author the runtime".to_string(),
            write_scope: WriteScope::runtime_topology(),
            bundle: BundleSpec::default(),
            prompt_evidence: Vec::new(),
            interactive: false,
        };

        let tasks = vec![
            topology,
            op_task("operation.a", WriteScope::operation_synthesis(id("operation.a"))),
        ];

        let waves = plan_waves(&tasks, &FxHashMap::default(), false);

        assert_eq!(waves.len(), 2, "{waves:?}");
        assert!(waves.iter().all(|wave| wave.len() == 1));
        assert!(all_indices(&waves, 2));
    }

    // A call cycle has no callee-first order; every task must still be
    // planned exactly once, serialized by the conflict check.
    #[test]
    fn call_cycles_still_plan_every_task() {
        let tasks = vec![
            op_task("operation.a", WriteScope::requirement_repair(id("operation.a"))),
            op_task("operation.b", WriteScope::requirement_repair(id("operation.b"))),
        ];

        let waves = plan_waves(
            &tasks,
            &calls(&[
                ("operation.a", "operation.b"),
                ("operation.b", "operation.a"),
            ]),
            true,
        );

        assert_eq!(waves.len(), 2, "{waves:?}");
        assert!(all_indices(&waves, 2));
    }
}
