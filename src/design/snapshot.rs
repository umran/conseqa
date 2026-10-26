//! Immutable published snapshots and the lock-free head.
//!
//! Every agent task reasons against exactly one snapshot for its whole
//! life (the snapshot invariant, §110). Reads never take a global
//! lock: the head is an atomically swapped `Arc`, and a task holds its
//! pinned snapshot directly.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::spec::Revision;

use super::graph::SymbolGraph;
use super::graph_build;
use super::workspace::WorkspaceState;

/// One immutable revision of the shared architecture state: the
/// workspace and its derived semantic graph.
#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub revision: Revision,
    pub workspace: Arc<WorkspaceState>,
    pub graph: Arc<SymbolGraph>,
}

impl WorkspaceSnapshot {
    /// Builds the snapshot for `workspace`, deriving its graph with
    /// symbol versions carried forward from `previous`.
    pub fn build(workspace: WorkspaceState, previous: Option<&SymbolGraph>) -> Self {
        let graph = graph_build::build(&workspace, previous);

        Self {
            revision: workspace.revision,
            workspace: Arc::new(workspace),
            graph: Arc::new(graph),
        }
    }
}

/// The current head, swapped atomically on commit. Readers load an
/// `Arc` and keep reading their loaded revision even as new heads
/// publish.
pub struct Head {
    current: ArcSwap<WorkspaceSnapshot>,
}

impl Head {
    pub fn new(snapshot: WorkspaceSnapshot) -> Self {
        Self {
            current: ArcSwap::from_pointee(snapshot),
        }
    }

    pub fn load(&self) -> Arc<WorkspaceSnapshot> {
        self.current.load_full()
    }

    pub fn publish(&self, snapshot: Arc<WorkspaceSnapshot>) {
        self.current.store(snapshot);
    }
}
