//! The agent-confluence layer: concurrent multi-agent authoring of one
//! shared architecture model.
//!
//! Agents reason against immutable semantic snapshots
//! ([`snapshot::WorkspaceSnapshot`]) of a draft workspace
//! ([`workspace::WorkspaceState`]). Every shared semantic fact an agent
//! observes is tracked server-side, and every mutation commits through
//! a serializable optimistic-concurrency gate that revalidates those
//! observations before publishing. Notifications are an optimization
//! for cancelling obsolete work early; the commit gate is the
//! correctness mechanism.
//!
//! The workspace is deliberately not `spec::Model`: during synthesis
//! the model may be incomplete — planned operations without programs,
//! requirements not yet discovered — and that drafting state lives in
//! an auxiliary authoring representation rather than a weakened DSL.
//! A structurally coherent `Model` is assembled from the workspace
//! only for validation and verification.

pub mod fingerprint;
pub mod graph;
pub mod graph_build;
pub mod graph_query;
pub mod snapshot;
pub mod symbol;
pub mod workspace;

pub use fingerprint::*;
pub use graph::*;
pub use graph_query::*;
pub use snapshot::*;
pub use symbol::*;
pub use workspace::*;
