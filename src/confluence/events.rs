//! Engine events: the advisory channel.
//!
//! Events let a supervisor cancel obsolete work early and schedule
//! follow-ups; they are never the correctness mechanism. A lost event
//! cannot cause a stale commit — the commit gate revalidates
//! everything (§2.7 of the confluence spec).

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::spec::Revision;

use super::graph_query::GraphQuery;
use super::read_set::SearchSpec;
use super::symbol::SymbolKey;
use super::task::{DependencyRequest, TaskId, TaskState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineEvent {
    /// A commit published a new head.
    HeadPublished { revision: Revision },

    TaskCommitted {
        task: TaskId,
        revision: Revision,
    },

    /// The task's observed context no longer holds at the new head.
    /// The supervisor should cancel the agent session; the commit gate
    /// would reject it regardless.
    TaskInvalidated {
        task: TaskId,
        new_revision: Revision,
        causes: Vec<InvalidationCause>,
    },

    TaskStateChanged {
        task: TaskId,
        state: TaskState,
    },

    DependencyRequested { request: DependencyRequest },

    /// Background analysis finished for a revision (phase 4).
    AnalysisReady { revision: Revision },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidationCause {
    /// An observed symbol's fingerprint changed.
    ChangedSymbol { symbol: SymbolKey },

    /// An observed symbol no longer exists.
    RemovedSymbol { symbol: SymbolKey },

    /// An observed query's canonical result changed.
    ChangedQuery { query: GraphQuery },

    /// An observed search's canonical result changed.
    ChangedSearch { search: SearchSpec },

    /// The engine restarted; live read tracking was lost, so running
    /// tasks are conservatively invalidated (§84).
    EngineRestart,
}

/// The engine's broadcast hub. Slow subscribers may observe
/// `Lagged` — acceptable for an advisory channel.
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<EngineEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);

        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.sender.subscribe()
    }

    pub fn emit(&self, event: EngineEvent) {
        // No subscribers is fine; events are advisory.
        let _ = self.sender.send(event);
    }
}
