//! The agent backend abstraction (§55 of the harness spec).
//!
//! A backend maps one logical design task to one coding-agent
//! session, exposing the Conseqa tool surface through MCP and nothing
//! else. The workflow never knows which backend is active; provider
//! specifics — command lines, event framing, cancellation — live
//! behind this trait.

use std::net::SocketAddr;
use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::design::{AgentBackendMetadata, TaskId, TaskKind};

/// Everything a backend needs to launch one session. The design
/// engine's MCP endpoint and the task's capability token are injected
/// here; the agent reaches shared state only through them.
#[derive(Debug, Clone)]
pub struct AgentInvocation {
    pub task: TaskId,
    pub kind: TaskKind,

    /// The task prompt: objective, invariant contract, and the initial
    /// tracked context bundle rendered for the model.
    pub prompt: String,

    /// The design MCP endpoint, e.g. `http://127.0.0.1:43127/mcp`.
    pub mcp_url: String,

    /// The task's bearer capability. Injected into the agent's MCP
    /// configuration, never into the prompt text (§57.1).
    pub task_token: String,

    /// The application source repository, mounted read-only. Agents
    /// synthesizing architecture read it as evidence, never as
    /// authority (§56, §86).
    pub repo: Option<PathBuf>,

    /// A scratch directory the backend may use for generated config.
    pub work_dir: PathBuf,

    pub budget: InvocationBudget,
}

/// What one agent session needs, whatever it was launched for: a design
/// task's invocation, or a conformance review's (§14.3 of the
/// conformance revision), which is no design task.
#[derive(Debug, Clone)]
pub struct SessionLaunch {
    /// Names the session's generated files in the work directory.
    pub name: String,

    pub prompt: String,

    /// The MCP endpoint the session reaches.
    pub mcp_url: String,

    /// The session's bearer credential, injected through the
    /// environment, never the prompt (§57.1).
    pub token: String,

    /// The directory the agent works in, read-only.
    pub repo: Option<PathBuf>,

    /// A scratch directory for generated configuration.
    pub work_dir: PathBuf,

    pub budget: InvocationBudget,
}

impl From<&AgentInvocation> for SessionLaunch {
    fn from(invocation: &AgentInvocation) -> Self {
        Self {
            name: invocation.task.0.to_string(),
            prompt: invocation.prompt.clone(),
            mcp_url: invocation.mcp_url.clone(),
            token: invocation.task_token.clone(),
            repo: invocation.repo.clone(),
            work_dir: invocation.work_dir.clone(),
            budget: invocation.budget,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InvocationBudget {
    pub max_wall_time_secs: Option<u64>,
    pub max_turns: Option<u64>,
}

/// A backend's structured observations during a session, forwarded to
/// the supervisor for diagnostics and correlation. These never carry
/// authority over task outcome — the design engine owns that.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// The session started; the string is the provider's session id
    /// when exposed.
    SessionStarted { session: Option<String> },

    /// The agent invoked a tool (name only; arguments are not
    /// surfaced).
    ToolCall { name: String },

    /// A human-readable log line from the backend.
    Log { message: String },

    /// Cost/usage the provider reported.
    Usage {
        tokens: Option<u64>,
        usd_cents: Option<u64>,
    },
}

/// The sink a backend pushes [`AgentEvent`]s into.
pub type AgentEventSink = mpsc::UnboundedSender<AgentEvent>;

/// How one agent session ended, as the backend observed it. This is
/// the *process* outcome, not the architectural one: whether the task
/// committed, filed a dependency request, or is unresolved is the
/// design engine's authoritative record (§91), which the
/// supervisor reads separately.
#[derive(Debug, Clone)]
pub struct AgentExit {
    pub status: AgentExitStatus,

    /// The provider session id, when known.
    pub session: Option<String>,

    /// The agent's final natural-language message, for diagnostics
    /// only — never parsed to determine whether state changed (§91).
    pub final_message: Option<String>,

    pub usage: AgentUsage,

    pub backend: AgentBackendMetadata,

    /// Set when an in-process executor attempted the task first and
    /// handed it to this session (§16 of the System One orchestration
    /// revision). Telemetry only.
    pub escalation: Option<Escalation>,
}

/// The name an in-process System One executor reports as its backend,
/// which is how a run's records tell a task a builder settled from one
/// an agent session did.
pub const SYSTEM_ONE_EXECUTOR: &str = "system_one";

/// Why an in-process executor handed its task to an agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    /// The executor that abstained.
    pub from: String,

    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentExitStatus {
    /// The process exited 0.
    Completed,

    /// The process exited non-zero.
    Failed { code: Option<i32> },

    /// The harness cancelled the session (typically on invalidation).
    Cancelled,

    /// The session exceeded its wall-clock budget.
    TimedOut,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AgentUsage {
    pub tokens: Option<u64>,
    pub usd_cents: Option<u64>,
    pub turns: Option<u64>,
}

/// A running session's cancellation handle.
#[derive(Debug, Clone)]
pub struct AgentHandle {
    pub task: TaskId,
    pub cancel: tokio_util::sync::CancellationToken,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentBackendError {
    #[error("cannot launch agent: {0}")]
    Launch(String),

    #[error("agent process i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("agent event stream error: {0}")]
    Stream(String),
}

/// One coding-agent backend. Implementations map a logical task to a
/// provider session and surface events; they never interpret Conseqa
/// semantics.
#[async_trait]
pub trait AgentBackend: Send + Sync {
    /// A stable backend name for manifests and metadata.
    fn name(&self) -> &str;

    /// Runs one session to completion (or cancellation), forwarding
    /// events. The returned [`AgentExit`] reports the process outcome.
    async fn run(
        &self,
        invocation: AgentInvocation,
        handle: AgentHandle,
        events: AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError>;
}

/// The MCP config a backend writes so its agent reaches only the
/// design server, with the task token injected through the
/// environment rather than the file (§57.1).
pub fn mcp_config_json(mcp_url: &str, token_env_var: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "conseqa": {
                "type": "http",
                "url": mcp_url,
                "headers": {
                    "Authorization": format!("Bearer ${{{token_env_var}}}"),
                },
            }
        }
    })
}

/// The loopback socket an MCP URL points at, for adapters that need
/// the address rather than the URL.
pub fn mcp_socket(url: &str) -> Option<SocketAddr> {
    url.strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .and_then(|authority| authority.parse().ok())
}
