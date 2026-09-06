//! Session supervision: run one confluence task to a terminal outcome
//! through a backend, cancelling on invalidation and reporting the
//! authoritative result.
//!
//! The supervisor is where the advisory event channel meets the
//! backend: a `TaskInvalidated` event cancels the agent process early
//! to save tokens (§34, §57.2), but the outcome the supervisor
//! reports comes from the confluence engine's own task state, never
//! from parsing the agent's final message (§91).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::confluence::{
    ConfluenceEngine, EngineEvent, TaskHandle, TaskId, TaskKind, TaskState,
};

use super::backend::{
    AgentBackend, AgentEvent, AgentExit, AgentExitStatus, AgentHandle, AgentInvocation,
    InvocationBudget,
};

/// How one supervised session ended, combining the agent's process
/// outcome with the confluence engine's authoritative task state.
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub task: TaskId,

    /// The confluence task state after the session — the authoritative
    /// architectural outcome (§91).
    pub task_state: TaskState,

    /// The agent process's own exit, for diagnostics.
    pub agent_exit: AgentExit,

    /// Whether the session was cancelled because the task was
    /// invalidated while running.
    pub invalidated: bool,
}

impl SessionOutcome {
    /// Whether the task committed its patch.
    pub fn committed(&self) -> bool {
        self.task_state == TaskState::Committed
    }

    /// Whether a fresh replacement task should be created: the task
    /// was invalidated, or the agent process failed without reaching a
    /// terminal architectural outcome.
    pub fn needs_replacement(&self) -> bool {
        self.invalidated
            || (self.task_state.is_active()
                && matches!(
                    self.agent_exit.status,
                    AgentExitStatus::Failed { .. } | AgentExitStatus::TimedOut
                ))
    }
}

/// Drives backends and confluence tasks together.
#[derive(Clone)]
pub struct Supervisor {
    engine: ConfluenceEngine,
    backend: Arc<dyn AgentBackend>,
    mcp_url: String,
    repo: Option<PathBuf>,
    work_dir: PathBuf,
}

impl Supervisor {
    pub fn new(
        engine: ConfluenceEngine,
        backend: Arc<dyn AgentBackend>,
        mcp_url: impl Into<String>,
        repo: Option<PathBuf>,
        work_dir: PathBuf,
    ) -> Self {
        Self {
            engine,
            backend,
            mcp_url: mcp_url.into(),
            repo,
            work_dir,
        }
    }

    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    /// Runs one already-created task to a terminal outcome. The
    /// session is cancelled the moment the engine invalidates the
    /// task; the outcome reflects the engine's authoritative state.
    pub async fn run_task(
        &self,
        handle: &TaskHandle,
        prompt: String,
        budget: InvocationBudget,
    ) -> SessionOutcome {
        let invocation = AgentInvocation {
            task: handle.id,
            kind: self.task_kind(handle.id),
            prompt,
            mcp_url: self.mcp_url.clone(),
            task_token: handle.token.0.clone(),
            repo: self.repo.clone(),
            work_dir: self.work_dir.clone(),
            budget,
        };

        let cancel = CancellationToken::new();

        let agent_handle = AgentHandle {
            task: handle.id,
            cancel: cancel.clone(),
        };

        // Watch for this task's invalidation; cancel the child when it
        // arrives. The commit gate is the safety net regardless (§2.7).
        let invalidation = self.spawn_invalidation_watch(handle.id, cancel.clone());

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let forwarder = spawn_event_logger(handle.id, event_rx);

        let exit = match self
            .backend
            .run(invocation, agent_handle, event_tx)
            .await
        {
            Ok(exit) => exit,

            Err(error) => {
                // A launch/stream failure is a process failure; the
                // task keeps whatever architectural state it reached.
                AgentExit {
                    status: AgentExitStatus::Failed { code: None },
                    session: None,
                    final_message: Some(format!("backend error: {error}")),
                    usage: Default::default(),
                    backend: crate::confluence::AgentBackendMetadata {
                        name: self.backend.name().to_string(),
                        version: None,
                        session: None,
                    },
                }
            }
        };

        let invalidated = invalidation.invalidated();
        invalidation.stop();
        forwarder.abort();

        let task_state = self
            .engine
            .task_status(handle.id)
            .unwrap_or(TaskState::Failed);

        // A process that ended while its task is still active, without
        // being invalidated, is a failed session; mark the task so the
        // scheduler can retry it.
        if task_state.is_active()
            && !invalidated
            && matches!(
                exit.status,
                AgentExitStatus::Failed { .. } | AgentExitStatus::TimedOut
            )
        {
            let _ = self.engine.fail_task(handle.id);
        }

        let task_state = self
            .engine
            .task_status(handle.id)
            .unwrap_or(TaskState::Failed);

        SessionOutcome {
            task: handle.id,
            task_state,
            agent_exit: exit,
            invalidated,
        }
    }

    fn task_kind(&self, task: TaskId) -> TaskKind {
        self.engine
            .task_context(task)
            .map(|context| context.kind)
            .unwrap_or(TaskKind::OperationSynthesis)
    }

    fn spawn_invalidation_watch(&self, task: TaskId, cancel: CancellationToken) -> InvalidationWatch {
        let mut events = self.engine.subscribe();
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = CancellationToken::new();

        let watch_flag = Arc::clone(&flag);
        let watch_stop = stop.clone();

        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = watch_stop.cancelled() => break,

                    event = events.recv() => match event {
                        Ok(EngineEvent::TaskInvalidated { task: invalidated, .. })
                            if invalidated == task =>
                        {
                            watch_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                            cancel.cancel();

                            break;
                        }

                        Ok(_) => continue,

                        // Lagged or closed: the commit gate still
                        // protects correctness, so stop watching.
                        Err(_) => break,
                    },
                }
            }
        });

        InvalidationWatch {
            flag,
            stop,
            join: Some(join),
        }
    }
}

struct InvalidationWatch {
    flag: Arc<std::sync::atomic::AtomicBool>,
    stop: CancellationToken,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl InvalidationWatch {
    fn invalidated(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn stop(mut self) {
        self.stop.cancel();

        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

/// Forwards backend events to `tracing` so a run is observable without
/// coupling the supervisor to any particular sink.
fn spawn_event_logger(
    task: TaskId,
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                AgentEvent::SessionStarted { session } => {
                    tracing::info!(%task, ?session, "agent session started");
                }
                AgentEvent::ToolCall { name } => {
                    tracing::debug!(%task, tool = %name, "agent tool call");
                }
                AgentEvent::Log { message } => {
                    tracing::debug!(%task, "agent: {message}");
                }
                AgentEvent::Usage { tokens, usd_cents } => {
                    tracing::info!(%task, ?tokens, ?usd_cents, "agent usage");
                }
            }
        }
    })
}
