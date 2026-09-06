//! The confluence engine: pinned-snapshot reads, tracked observations,
//! and the serializable commit gate.
//!
//! Reads are lock-free on the hot path — task auth, `Arc` snapshot
//! load, graph read, observation record (§82). All mutations pass
//! through one logical commit sequencer: a queue draining into a
//! single worker thread that validates, applies, persists, publishes,
//! and invalidates (§28). The worker is the only head writer, so
//! commit-time validation races nothing.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::spec::{Id, Revision};

use super::auth::{TaskToken, TokenMap};
use super::commit::{
    CommitReceipt, CommitRecord, CommitRejection, CommitRequest, apply_patch,
};
use super::events::{EngineEvent, EventBus, InvalidationCause};
use super::graph_query::{self, GraphQuery, QueryResult};
use super::invalidation::stale_causes;
use super::persistence::{Persistence, PersistenceError, TaskRecord};
use super::read_set::{
    QueryObservation, SearchSpec, SummaryObservation, SymbolObservation, TaskReadSet, run_search,
    search_fingerprint,
};
use super::snapshot::{Head, WorkspaceSnapshot};
use super::symbol::{SymbolKey, SymbolKind, SymbolVersion};
use super::task::{
    DependencyRequest, DependencyRequestId, PromptEvidence, TaskBudget, TaskCompletionGate,
    TaskId, TaskKind, TaskSpec, TaskState, WriteScope,
};
use super::workspace::{EvidenceRef, WorkspaceState};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown task")]
    UnknownTask,

    #[error(
        "task is {0}; architecture tools are unavailable. Do not retry in this session — \
         if the task was invalidated, the harness restarts it against a fresh snapshot"
    )]
    TaskInactive(TaskState),

    #[error("unknown symbol {0} at this task's snapshot")]
    UnknownSymbol(SymbolKey),

    #[error("unknown operation {0} at this task's snapshot")]
    UnknownOperation(Id),

    #[error("analysis is not available for revision {} yet", .0.0)]
    AnalysisNotReady(Revision),

    #[error("persistence: {0}")]
    Persistence(#[from] PersistenceError),

    #[error("the commit sequencer is shut down")]
    Shutdown,
}

/// Parameters for creating a task. The engine pins the current head
/// as the task's snapshot and mints its capability token; an agent
/// can never choose its own scope (§85).
#[derive(Debug, Clone)]
pub struct CreateTask {
    pub kind: TaskKind,
    pub objective: String,
    pub write_scope: WriteScope,
    pub prompt_evidence: Vec<PromptEvidence>,
    pub budget: TaskBudget,
}

/// What the scheduler holds after creating a task. The token is the
/// task's whole authority; it is injected into the agent's MCP
/// configuration and never shared across tasks.
#[derive(Debug, Clone)]
pub struct TaskHandle {
    pub id: TaskId,
    pub token: TaskToken,
    pub snapshot_revision: Revision,
}

/// The `task_context` view (§45).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    pub id: TaskId,
    pub kind: TaskKind,
    pub objective: String,
    pub snapshot_revision: Revision,
    pub write_scope: WriteScope,
    pub prompt_evidence: Vec<PromptEvidence>,
    pub budget: TaskBudget,
    pub completion_gate: TaskCompletionGate,
    pub state: TaskState,
}

/// A tracked symbol read (§46).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolView {
    pub key: SymbolKey,
    pub kind: SymbolKind,
    pub version: SymbolVersion,
    pub content: serde_json::Value,
}

/// Which slice of an operation to read (§48). Prefer `Interface` and
/// `ProofSummary` for dependencies; `Full` only when truly needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationReadMode {
    Interface,
    Program,
    Requirements,
    ProofSummary,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationView {
    pub operation: Id,
    pub mode: OperationReadMode,
    pub content: serde_json::Value,
}

struct TaskEntry {
    spec: TaskSpec,
    snapshot: Arc<WorkspaceSnapshot>,
    state: Mutex<TaskState>,
    read_set: Mutex<TaskReadSet>,
}

struct EngineInner {
    head: Head,
    tasks: RwLock<FxHashMap<TaskId, Arc<TaskEntry>>>,
    tokens: TokenMap,
    events: EventBus,
    persistence: Persistence,
}

struct QueuedCommit {
    request: CommitRequest,
    respond: oneshot::Sender<Result<CommitReceipt, CommitRejection>>,
}

/// Handle to one confluence engine. Cheap to clone; when every clone
/// drops, the commit sequencer drains and exits.
#[derive(Clone)]
pub struct ConfluenceEngine {
    inner: Arc<EngineInner>,
    commits: mpsc::Sender<QueuedCommit>,
}

impl ConfluenceEngine {
    /// An engine over in-memory persistence, for tests and ephemeral
    /// runs.
    pub fn in_memory(initial: WorkspaceState) -> Result<Self, EngineError> {
        Self::start(Persistence::in_memory()?, initial)
    }

    /// Opens (or creates) a file-backed engine. When the database
    /// already holds a head, that head is authoritative and `initial`
    /// is ignored; persisted tasks that were active are conservatively
    /// invalidated, because their live read tracking died with the
    /// previous process (§84).
    pub fn open(path: &std::path::Path, initial: WorkspaceState) -> Result<Self, EngineError> {
        Self::start(Persistence::open_file(path)?, initial)
    }

    fn start(persistence: Persistence, initial: WorkspaceState) -> Result<Self, EngineError> {
        let recovered = match persistence.head()? {
            Some(revision) => {
                let workspace = persistence
                    .load_workspace(revision)?
                    .expect("the head revision is persisted with its workspace");

                Some(workspace)
            }

            None => None,
        };

        let recovering = recovered.is_some();

        let workspace = match recovered {
            Some(workspace) => workspace,
            None => {
                persistence.init_head(&initial)?;

                initial
            }
        };

        let snapshot = WorkspaceSnapshot::build(workspace, None);

        let inner = Arc::new(EngineInner {
            head: Head::new(snapshot),
            tasks: RwLock::new(FxHashMap::default()),
            tokens: TokenMap::default(),
            events: EventBus::new(256),
            persistence,
        });

        if recovering {
            recover_tasks(&inner)?;
        }

        let (sender, receiver) = mpsc::channel(64);

        {
            let inner = Arc::clone(&inner);

            std::thread::Builder::new()
                .name("conseqa-commit-sequencer".to_string())
                .spawn(move || commit_worker(inner, receiver))
                .expect("spawning the commit sequencer succeeds");
        }

        Ok(Self {
            inner,
            commits: sender,
        })
    }

    pub fn head_snapshot(&self) -> Arc<WorkspaceSnapshot> {
        self.inner.head.load()
    }

    pub fn head_revision(&self) -> Revision {
        self.inner.head.load().revision
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EngineEvent> {
        self.inner.events.subscribe()
    }

    pub fn resolve_token(&self, token: &str) -> Option<TaskId> {
        self.inner.tokens.resolve(token)
    }

    /// Creates a task pinned to the current head and mints its
    /// capability token.
    pub fn create_task(&self, params: CreateTask) -> Result<TaskHandle, EngineError> {
        let snapshot = self.inner.head.load();

        let spec = TaskSpec {
            id: TaskId::fresh(),
            kind: params.kind,
            objective: params.objective,
            snapshot_revision: snapshot.revision,
            write_scope: params.write_scope,
            prompt_evidence: params.prompt_evidence,
            budget: params.budget,
            completion_gate: TaskCompletionGate::SinglePatch,
        };

        self.inner
            .persistence
            .record_task(&spec, TaskState::Running, now_unix_ms())?;

        let token = self.inner.tokens.issue(spec.id);

        let handle = TaskHandle {
            id: spec.id,
            token,
            snapshot_revision: snapshot.revision,
        };

        let entry = Arc::new(TaskEntry {
            spec,
            snapshot,
            state: Mutex::new(TaskState::Running),
            read_set: Mutex::new(TaskReadSet::default()),
        });

        self.inner.tasks.write().insert(handle.id, entry);

        self.inner.events.emit(EngineEvent::TaskStateChanged {
            task: handle.id,
            state: TaskState::Running,
        });

        Ok(handle)
    }

    /// The task's own context (§45). Available in every state.
    pub fn task_context(&self, task: TaskId) -> Result<TaskContext, EngineError> {
        let entry = self.entry(task)?;
        let state = *entry.state.lock();

        Ok(TaskContext {
            id: entry.spec.id,
            kind: entry.spec.kind,
            objective: entry.spec.objective.clone(),
            snapshot_revision: entry.spec.snapshot_revision,
            write_scope: entry.spec.write_scope.clone(),
            prompt_evidence: entry.spec.prompt_evidence.clone(),
            budget: entry.spec.budget,
            completion_gate: entry.spec.completion_gate,
            state,
        })
    }

    /// The task's state (§53). Available in every state, so a
    /// notified-out agent can still learn it was invalidated.
    pub fn task_status(&self, task: TaskId) -> Result<TaskState, EngineError> {
        Ok(*self.entry(task)?.state.lock())
    }

    /// Reads one symbol from the task's pinned snapshot, recording the
    /// observation (§46).
    pub fn read_symbol(&self, task: TaskId, key: &SymbolKey) -> Result<SymbolView, EngineError> {
        let entry = self.active_entry(task)?;

        let node = entry
            .snapshot
            .graph
            .node(key)
            .ok_or_else(|| EngineError::UnknownSymbol(key.clone()))?;

        let content = render_symbol(&entry.snapshot.workspace, key)
            .ok_or_else(|| EngineError::UnknownSymbol(key.clone()))?;

        entry.read_set.lock().record_symbol(
            key.clone(),
            SymbolObservation {
                version: node.version,
                fingerprint: node.fingerprint,
            },
        );

        Ok(SymbolView {
            key: key.clone(),
            kind: node.kind,
            version: node.version,
            content,
        })
    }

    /// Reads one slice of an operation, recording observations of the
    /// sub-symbols the slice covers (§48).
    pub fn read_operation(
        &self,
        task: TaskId,
        operation: &Id,
        mode: OperationReadMode,
    ) -> Result<OperationView, EngineError> {
        let entry = self.active_entry(task)?;

        let draft = entry
            .snapshot
            .workspace
            .operations
            .get(operation)
            .ok_or_else(|| EngineError::UnknownOperation(operation.clone()))?;

        let observed: Vec<SymbolKey> = match mode {
            OperationReadMode::Interface => vec![SymbolKey::OperationInterface(operation.clone())],
            OperationReadMode::Program => vec![SymbolKey::OperationProgram(operation.clone())],
            OperationReadMode::Requirements => {
                vec![SymbolKey::OperationRequirements(operation.clone())]
            }

            // V1 validates summaries conservatively through their
            // input symbols (§41).
            OperationReadMode::ProofSummary | OperationReadMode::Full => vec![
                SymbolKey::OperationInterface(operation.clone()),
                SymbolKey::OperationProgram(operation.clone()),
                SymbolKey::OperationRequirements(operation.clone()),
                SymbolKey::OperationExecution(operation.clone()),
            ],
        };

        let content = match mode {
            OperationReadMode::Interface => serde_json::to_value(draft.interface()),
            OperationReadMode::Program => serde_json::to_value(&draft.program),
            OperationReadMode::Requirements => serde_json::to_value(&draft.requirements),
            OperationReadMode::Full => serde_json::to_value(draft),

            OperationReadMode::ProofSummary => {
                // Summaries are analysis output; phase 4 serves them.
                return Err(EngineError::AnalysisNotReady(entry.snapshot.revision));
            }
        }
        .expect("draft slices serialize to JSON");

        {
            let mut read_set = entry.read_set.lock();

            for key in observed {
                let node = entry
                    .snapshot
                    .graph
                    .node(&key)
                    .expect("declared operations have their sub-symbol nodes");

                read_set.record_symbol(
                    key,
                    SymbolObservation {
                        version: node.version,
                        fingerprint: node.fingerprint,
                    },
                );
            }
        }

        Ok(OperationView {
            operation: operation.clone(),
            mode,
            content,
        })
    }

    /// Runs one canonical graph query against the task's pinned
    /// snapshot, recording the result fingerprint for phantom
    /// revalidation (§49).
    pub fn graph_query(&self, task: TaskId, query: &GraphQuery) -> Result<QueryResult, EngineError> {
        let entry = self.active_entry(task)?;

        let result = graph_query::run(&entry.snapshot.workspace, &entry.snapshot.graph, query);

        entry
            .read_set
            .lock()
            .record_query(QueryObservation::graph(query.clone(), result.fingerprint));

        Ok(result)
    }

    /// Symbol search over the pinned snapshot, tracked as a query
    /// observation because agents may rely on set membership (§47).
    pub fn search_symbols(
        &self,
        task: TaskId,
        spec: &SearchSpec,
    ) -> Result<Vec<SymbolKey>, EngineError> {
        let entry = self.active_entry(task)?;

        let rows = run_search(&entry.snapshot.workspace, &entry.snapshot.graph, spec);

        entry
            .read_set
            .lock()
            .record_query(QueryObservation::search(
                spec.clone(),
                search_fingerprint(&rows),
            ));

        Ok(rows)
    }

    /// Records that a task consumed an operation summary; V1 tracks
    /// the summary's inputs as symbol observations at bundle/serve
    /// time.
    pub fn record_summary_observation(
        &self,
        task: TaskId,
        operation: Id,
        observation: SummaryObservation,
    ) -> Result<(), EngineError> {
        let entry = self.active_entry(task)?;

        entry
            .read_set
            .lock()
            .record_summary(operation, observation);

        Ok(())
    }

    /// Queues a commit for the sequencer; the returned receiver
    /// resolves once the gate accepts or rejects.
    pub async fn submit(
        &self,
        request: CommitRequest,
    ) -> Result<Result<CommitReceipt, CommitRejection>, EngineError> {
        let (respond, receive) = oneshot::channel();

        self.commits
            .send(QueuedCommit { request, respond })
            .await
            .map_err(|_| EngineError::Shutdown)?;

        receive.await.map_err(|_| EngineError::Shutdown)
    }

    /// `submit` for synchronous callers. Must not be called from
    /// within an async runtime.
    pub fn submit_blocking(
        &self,
        request: CommitRequest,
    ) -> Result<Result<CommitReceipt, CommitRejection>, EngineError> {
        let (respond, receive) = oneshot::channel();

        self.commits
            .blocking_send(QueuedCommit { request, respond })
            .map_err(|_| EngineError::Shutdown)?;

        receive.blocking_recv().map_err(|_| EngineError::Shutdown)
    }

    /// Files an out-of-scope change request (§52). The task stays
    /// running; whether it can finish an independent portion is the
    /// agent's judgment, and its final outcome is reported separately.
    pub fn dependency_request(
        &self,
        task: TaskId,
        target: SymbolKey,
        requested_change: String,
        reason: String,
        evidence: Vec<EvidenceRef>,
    ) -> Result<DependencyRequestId, EngineError> {
        let entry = self.active_entry(task)?;

        let request = DependencyRequest {
            id: DependencyRequestId::fresh(),
            task: entry.spec.id,
            target,
            requested_change,
            reason,
            evidence,
        };

        self.inner.persistence.record_dependency_request(&request)?;

        self.inner
            .events
            .emit(EngineEvent::DependencyRequested {
                request: request.clone(),
            });

        Ok(request.id)
    }

    /// Cancels a task: its authority ends and its token is revoked.
    pub fn cancel_task(&self, task: TaskId) -> Result<(), EngineError> {
        self.finish_task(task, TaskState::Cancelled)
    }

    /// Marks a task failed (agent crash, backend error).
    pub fn fail_task(&self, task: TaskId) -> Result<(), EngineError> {
        self.finish_task(task, TaskState::Failed)
    }

    /// Marks a task's terminal outcome as reported by the supervisor:
    /// `DependencyRequested`, `Unresolved`, or `Completed`.
    pub fn conclude_task(&self, task: TaskId, outcome: TaskState) -> Result<(), EngineError> {
        debug_assert!(matches!(
            outcome,
            TaskState::DependencyRequested | TaskState::Unresolved | TaskState::Completed
        ));

        self.finish_task(task, outcome)
    }

    fn finish_task(&self, task: TaskId, state: TaskState) -> Result<(), EngineError> {
        let entry = self.entry(task)?;

        {
            let mut current = entry.state.lock();

            if !current.is_active() && *current != TaskState::Committed {
                return Err(EngineError::TaskInactive(*current));
            }

            *current = state;
        }

        self.inner.tokens.revoke(task);

        self.inner
            .persistence
            .update_task_state(task, state, now_unix_ms())?;

        self.inner
            .events
            .emit(EngineEvent::TaskStateChanged { task, state });

        Ok(())
    }

    fn entry(&self, task: TaskId) -> Result<Arc<TaskEntry>, EngineError> {
        self.inner
            .tasks
            .read()
            .get(&task)
            .cloned()
            .ok_or(EngineError::UnknownTask)
    }

    /// The entry, provided the task may still use architecture tools.
    /// An invalidated task's reads fail so wasted reasoning stops even
    /// without push notifications (§53).
    fn active_entry(&self, task: TaskId) -> Result<Arc<TaskEntry>, EngineError> {
        let entry = self.entry(task)?;
        let state = *entry.state.lock();

        if !state.is_active() {
            return Err(EngineError::TaskInactive(state));
        }

        Ok(entry)
    }
}

/// On recovery, persisted tasks that were still active are
/// conservatively invalidated: their live read tracking died with the
/// previous process (§84). Terminal tasks are kept for audit.
fn recover_tasks(inner: &Arc<EngineInner>) -> Result<(), EngineError> {
    let records = inner.persistence.load_tasks()?;
    let snapshot = inner.head.load();

    for TaskRecord { spec, state } in records {
        let state = if state.is_active() || state == TaskState::Planned {
            inner
                .persistence
                .update_task_state(spec.id, TaskState::Invalidated, now_unix_ms())?;

            TaskState::Invalidated
        } else {
            state
        };

        let id = spec.id;

        let entry = Arc::new(TaskEntry {
            spec,
            snapshot: Arc::clone(&snapshot),
            state: Mutex::new(state),
            read_set: Mutex::new(TaskReadSet::default()),
        });

        inner.tasks.write().insert(id, entry);
    }

    Ok(())
}

/// The commit sequencer: one logical writer draining the queue (§28).
fn commit_worker(inner: Arc<EngineInner>, mut queue: mpsc::Receiver<QueuedCommit>) {
    while let Some(QueuedCommit { request, respond }) = queue.blocking_recv() {
        let outcome = process_commit(&inner, request);

        // A dropped receiver means the submitter went away; the commit
        // decision stands either way.
        let _ = respond.send(outcome);
    }
}

/// The commit protocol (§29). The worker is the only head writer, so
/// everything validated here holds when the head publishes.
///
/// Rejections are rich by design and the commit path is cold — one
/// commit per agent task — so the large `Err` variant is fine.
#[allow(clippy::result_large_err)]
fn process_commit(
    inner: &Arc<EngineInner>,
    request: CommitRequest,
) -> Result<CommitReceipt, CommitRejection> {
    let entry = inner
        .tasks
        .read()
        .get(&request.task)
        .cloned()
        .ok_or(CommitRejection::TaskUnknown)?;

    // Idempotent resubmission: an identical nonce returns the
    // previously committed revision (§84).
    if let Ok(Some(revision)) = inner
        .persistence
        .nonce_revision(request.task, request.client_nonce)
    {
        return Ok(CommitReceipt {
            revision,
            replayed: true,
        });
    }

    {
        let mut state = entry.state.lock();

        match *state {
            TaskState::Running => *state = TaskState::Committing,
            TaskState::Invalidated => return Err(CommitRejection::TaskInvalidated),
            other => return Err(CommitRejection::TaskNotRunning { state: other }),
        }
    }

    match validate_and_commit(inner, &entry, &request) {
        Ok(receipt) => {
            *entry.state.lock() = TaskState::Committed;

            inner.events.emit(EngineEvent::TaskCommitted {
                task: request.task,
                revision: receipt.revision,
            });

            Ok(receipt)
        }

        Err(rejection) => {
            if rejection.is_stale_context() {
                *entry.state.lock() = TaskState::Invalidated;

                inner.tokens.revoke(request.task);

                let _ = inner.persistence.update_task_state(
                    request.task,
                    TaskState::Invalidated,
                    now_unix_ms(),
                );

                inner.events.emit(EngineEvent::TaskInvalidated {
                    task: request.task,
                    new_revision: inner.head.load().revision,
                    causes: rejection_causes(&rejection),
                });
            } else {
                // Fixable in-session: scope, unobserved dependency,
                // draft validation.
                *entry.state.lock() = TaskState::Running;
            }

            Err(rejection)
        }
    }
}

#[allow(clippy::result_large_err)]
fn validate_and_commit(
    inner: &Arc<EngineInner>,
    entry: &Arc<TaskEntry>,
    request: &CommitRequest,
) -> Result<CommitReceipt, CommitRejection> {
    // 3. The submission must be built on the task's pinned snapshot.
    if request.base_revision != entry.spec.snapshot_revision {
        return Err(CommitRejection::BaseRevisionMismatch {
            pinned: entry.spec.snapshot_revision,
            submitted: request.base_revision,
        });
    }

    // 4. Write scope.
    for mutation in &request.patch.mutations {
        if let Some(attempted) = entry.spec.write_scope.violation(mutation) {
            return Err(CommitRejection::WriteScopeViolation { attempted });
        }
    }

    let head = inner.head.load();

    // 5. Every observed symbol still holds at the head.
    {
        let read_set = entry.read_set.lock();

        for (key, observation) in read_set.symbols_ordered() {
            let current = head.graph.fingerprint(key);

            if current != Some(observation.fingerprint) {
                return Err(CommitRejection::ReadConflict {
                    symbol: key.clone(),
                    observed: observation.fingerprint,
                    current,
                });
            }
        }

        // 6. Every observed query still returns the same canonical
        // result (§21, §32).
        for observation in read_set.queries_ordered() {
            match &observation.query {
                super::read_set::TrackedQuery::Graph(query) => {
                    let result = graph_query::run(&head.workspace, &head.graph, query);

                    if result.fingerprint != observation.result_fingerprint {
                        return Err(CommitRejection::PhantomConflict {
                            query: query.clone(),
                        });
                    }
                }

                super::read_set::TrackedQuery::Search(spec) => {
                    let rows = run_search(&head.workspace, &head.graph, spec);

                    if search_fingerprint(&rows) != observation.result_fingerprint {
                        return Err(CommitRejection::SearchConflict);
                    }
                }
            }
        }
    }

    // 7. Write targets unchanged since the task's base snapshot, read
    // or not (§31).
    for target in request.patch.write_targets() {
        let base = entry.snapshot.graph.fingerprint(&target);
        let current = head.graph.fingerprint(&target);

        if base != current {
            return Err(CommitRejection::WriteConflict { symbol: target });
        }
    }

    // 8–9. Read-before-reference (§24).
    {
        let read_set = entry.read_set.lock();

        for reference in request.patch.external_references() {
            let observed = read_set.symbols.contains_key(&reference)
                || matches!(
                    &reference,
                    SymbolKey::Operation(operation)
                        if read_set
                            .symbols
                            .contains_key(&SymbolKey::OperationInterface(operation.clone()))
                );

            if !observed {
                return Err(CommitRejection::UnobservedDependency { symbol: reference });
            }
        }
    }

    // 10–11. Apply to a candidate and run draft-local checks.
    let mut candidate = (*head.workspace).clone();

    candidate.revision = Revision(head.revision.0 + 1);

    let diagnostics = apply_patch(&mut candidate, &request.patch);

    if !diagnostics.is_empty() {
        return Err(CommitRejection::DraftValidationFailed { diagnostics });
    }

    // 12. Build the candidate graph, carrying symbol versions forward.
    let snapshot = WorkspaceSnapshot::build(candidate, Some(&head.graph));

    // 13. The changed-symbol set drives invalidation.
    let changed = changed_symbols(&head, &snapshot);

    let record = CommitRecord {
        revision: snapshot.revision,
        parent: head.revision,
        task: request.task,
        patch_id: request.patch_id,
        client_nonce: request.client_nonce,
        changed_symbols: changed.clone(),
        timestamp_unix_ms: now_unix_ms(),
        backend: None,
    };

    // 14. Atomic persistence, then 15. publication.
    if let Err(error) =
        inner
            .persistence
            .persist_commit(&snapshot.workspace, &record, TaskState::Committed)
    {
        // Persistence failure keeps the old head authoritative; the
        // task context is not stale, so this is fixable by resubmitting.
        return Err(CommitRejection::DraftValidationFailed {
            diagnostics: vec![super::commit::DraftDiagnostic {
                subject: None,
                message: format!("commit could not be persisted: {error}"),
            }],
        });
    }

    let snapshot = Arc::new(snapshot);

    inner.head.publish(Arc::clone(&snapshot));

    inner.events.emit(EngineEvent::HeadPublished {
        revision: snapshot.revision,
    });

    // 16. Invalidate active tasks whose observed context changed
    // (§34).
    invalidate_stale_tasks(inner, request.task, &changed, &snapshot);

    Ok(CommitReceipt {
        revision: snapshot.revision,
        replayed: false,
    })
}

fn changed_symbols(head: &WorkspaceSnapshot, candidate: &WorkspaceSnapshot) -> Vec<SymbolKey> {
    let mut changed = Vec::new();

    for node in &candidate.graph.nodes {
        match head.graph.fingerprint(&node.key) {
            Some(before) if before == node.fingerprint => {}
            _ => changed.push(node.key.clone()),
        }
    }

    for node in &head.graph.nodes {
        if candidate.graph.node(&node.key).is_none() {
            changed.push(node.key.clone());
        }
    }

    changed.sort();
    changed.dedup();

    changed
}

fn invalidate_stale_tasks(
    inner: &Arc<EngineInner>,
    committing: TaskId,
    changed: &[SymbolKey],
    head: &Arc<WorkspaceSnapshot>,
) {
    let mut entries: Vec<(TaskId, Arc<TaskEntry>)> = inner
        .tasks
        .read()
        .iter()
        .filter(|(id, _)| **id != committing)
        .map(|(id, entry)| (*id, Arc::clone(entry)))
        .collect();

    entries.sort_by_key(|(id, _)| *id);

    for (id, entry) in entries {
        {
            let state = entry.state.lock();

            if *state != TaskState::Running {
                continue;
            }
        }

        let causes = {
            let read_set = entry.read_set.lock();

            stale_causes(&read_set, changed, head)
        };

        if causes.is_empty() {
            continue;
        }

        {
            let mut state = entry.state.lock();

            // Re-check under the lock; the task may have finished.
            if *state != TaskState::Running {
                continue;
            }

            *state = TaskState::Invalidated;
        }

        inner.tokens.revoke(id);

        let _ = inner
            .persistence
            .update_task_state(id, TaskState::Invalidated, now_unix_ms());

        inner.events.emit(EngineEvent::TaskInvalidated {
            task: id,
            new_revision: head.revision,
            causes,
        });
    }
}

fn rejection_causes(rejection: &CommitRejection) -> Vec<InvalidationCause> {
    match rejection {
        CommitRejection::ReadConflict { symbol, current, .. } => match current {
            Some(_) => vec![InvalidationCause::ChangedSymbol {
                symbol: symbol.clone(),
            }],
            None => vec![InvalidationCause::RemovedSymbol {
                symbol: symbol.clone(),
            }],
        },

        CommitRejection::WriteConflict { symbol } => vec![InvalidationCause::ChangedSymbol {
            symbol: symbol.clone(),
        }],

        CommitRejection::PhantomConflict { query } => vec![InvalidationCause::ChangedQuery {
            query: query.clone(),
        }],

        _ => Vec::new(),
    }
}

fn render_symbol(workspace: &WorkspaceState, key: &SymbolKey) -> Option<serde_json::Value> {
    let value = match key {
        SymbolKey::Service(id) => serde_json::to_value(workspace.services.get(id)?),
        SymbolKey::Schema(id) => serde_json::to_value(workspace.schemas.get(id)?),
        SymbolKey::DataModel(id) => serde_json::to_value(workspace.data_models.get(id)?),

        SymbolKey::DataObject { data_model, object } => {
            serde_json::to_value(workspace.data_models.get(data_model)?.objects.get(object)?)
        }

        SymbolKey::Topic(id) => serde_json::to_value(workspace.topics.get(id)?),
        SymbolKey::StateMachine(id) => serde_json::to_value(workspace.state_machines.get(id)?),

        SymbolKey::Transition {
            machine,
            transition,
        } => serde_json::to_value(
            workspace
                .state_machines
                .get(machine)?
                .transitions
                .get(transition)?,
        ),

        SymbolKey::Operation(id) => serde_json::to_value(workspace.operations.get(id)?),

        SymbolKey::OperationInterface(id) => {
            serde_json::to_value(workspace.operations.get(id)?.interface())
        }

        SymbolKey::OperationProgram(id) => {
            serde_json::to_value(&workspace.operations.get(id)?.program)
        }

        SymbolKey::OperationRequirements(id) => {
            serde_json::to_value(&workspace.operations.get(id)?.requirements)
        }

        SymbolKey::OperationExecution(id) => {
            serde_json::to_value(&workspace.operations.get(id)?.execution)
        }

        SymbolKey::Input { operation, input } => {
            serde_json::to_value(workspace.operations.get(operation)?.inputs.get(input)?)
        }

        SymbolKey::Transaction {
            operation,
            transaction,
        } => serde_json::to_value(
            workspace
                .operations
                .get(operation)?
                .program
                .as_ref()?
                .transaction(transaction)?,
        ),

        SymbolKey::EffectSite { operation, effect } => {
            let program = workspace.operations.get(operation)?.program.as_ref()?;

            let declaration = program
                .effect_declarations()
                .into_iter()
                .find_map(|(id, declared)| (id == effect).then_some(declared))?
                .clone();

            serde_json::to_value(declaration)
        }

        SymbolKey::Binding { operation, binding } => Ok(serde_json::json!({
            "operation": operation.0,
            "binding": binding.0,
            "note": "a typed operation-visible binding; read the program for its producing site",
        })),

        SymbolKey::Requirement {
            operation,
            family,
            fingerprint,
            occurrence,
        } => {
            let draft = workspace.operations.get(operation)?;

            requirement_content(draft, *family, *fingerprint, *occurrence)?
        }

        SymbolKey::OperationSummary(_) => return None,

        SymbolKey::PromptObligation(id) => {
            serde_json::to_value(workspace.prompt_obligations.get(id)?)
        }
    };

    value.ok()
}

fn requirement_content(
    draft: &super::workspace::DraftOperation,
    family: super::symbol::RequirementFamily,
    fingerprint: super::fingerprint::SemanticHash,
    occurrence: u32,
) -> Option<Result<serde_json::Value, serde_json::Error>> {
    use super::fingerprint::SemanticHash;
    use super::symbol::RequirementFamily;

    fn pick<T: serde::Serialize>(
        list: &[T],
        fingerprint: SemanticHash,
        occurrence: u32,
    ) -> Option<Result<serde_json::Value, serde_json::Error>> {
        let mut seen = 0u32;

        for entry in list {
            if SemanticHash::of(entry) == fingerprint {
                if seen == occurrence {
                    return Some(serde_json::to_value(entry));
                }

                seen += 1;
            }
        }

        None
    }

    match family {
        RequirementFamily::Serialization => {
            pick(&draft.requirements.serialization, fingerprint, occurrence)
        }
        RequirementFamily::Ordering => pick(&draft.requirements.ordering, fingerprint, occurrence),
        RequirementFamily::Idempotency | RequirementFamily::ResultReplay => {
            pick(&draft.requirements.idempotency, fingerprint, occurrence)
        }
        RequirementFamily::Recoverability => {
            pick(&draft.requirements.recoverability, fingerprint, occurrence)
        }
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
