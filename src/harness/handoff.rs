//! The hand-off backend: a design run with no agent sessions.
//!
//! Every task the in-process builders cannot settle — a sketch that
//! does not compile, an unsure discovery, a repair outside the remedy
//! catalogue, or a kind with no builder at all — lands here instead of
//! on a coding agent. Nothing is written: the task is recorded with what
//! the builder established, the run is told to halt at its next phase
//! boundary, and the caller resolves the recorded work itself and runs
//! the design again. The interactive coordinator already holds the whole
//! model, so it is better placed to finish what code could not than a
//! fresh worker session would be.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Serialize;

use crate::confluence::{AgentBackendMetadata, ConfluenceEngine, TaskKind};

use super::backend::{
    AgentBackend, AgentBackendError, AgentEventSink, AgentExit, AgentExitStatus, AgentHandle,
    AgentInvocation,
};

/// The name the hand-off backend reports, in run records and manifests.
pub const HAND_OFF: &str = "hand_off";

/// The heading a System One builder's abstention is appended under.
const BUILDER_SECTION: &str = "## Hand-off from the System One executor";

/// One task handed back to the caller.
#[derive(Debug, Clone, Serialize)]
pub struct HandOff {
    pub kind: TaskKind,

    /// What the task was asked to do.
    pub objective: String,

    /// Why the in-process builder abstained and what it had
    /// established, when one attempted the task first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builder: Option<String>,
}

/// Records every task it is given and asks the run to halt.
#[derive(Clone)]
pub struct HandOffBackend {
    engine: ConfluenceEngine,
    halt: Arc<AtomicBool>,
    handed: Arc<Mutex<Vec<HandOff>>>,
}

impl HandOffBackend {
    pub fn new(engine: ConfluenceEngine) -> Self {
        Self {
            engine,
            halt: Arc::new(AtomicBool::new(false)),
            handed: Default::default(),
        }
    }

    /// Set once a task has been handed off; the workflow stops at its
    /// next phase boundary when given this flag.
    pub fn halt(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.halt)
    }

    /// Every task handed off so far, in order.
    pub fn handed(&self) -> Vec<HandOff> {
        self.handed.lock().clone()
    }
}

#[async_trait]
impl AgentBackend for HandOffBackend {
    fn name(&self) -> &str {
        HAND_OFF
    }

    async fn run(
        &self,
        invocation: AgentInvocation,
        _handle: AgentHandle,
        _events: AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError> {
        let objective = self
            .engine
            .task_context(invocation.task)
            .map(|task| task.objective)
            .unwrap_or_default();

        let builder = invocation
            .prompt
            .split_once(BUILDER_SECTION)
            .map(|(_, established)| established.trim().to_string());

        // A retried task is handed off again; once is enough.
        let mut handed = self.handed.lock();

        if !handed
            .iter()
            .any(|known| known.kind == invocation.kind && known.objective == objective)
        {
            handed.push(HandOff {
                kind: invocation.kind,
                objective,
                builder,
            });
        }

        drop(handed);

        self.halt.store(true, Ordering::SeqCst);

        Ok(AgentExit {
            status: AgentExitStatus::Completed,
            session: None,
            final_message: None,
            usage: Default::default(),
            backend: AgentBackendMetadata {
                name: HAND_OFF.to_string(),
                version: None,
                session: None,
            },
            escalation: None,
        })
    }
}
