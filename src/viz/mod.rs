//! The model visualization: a system-level view-model extraction and a
//! self-contained interactive HTML renderer.
//!
//! Lives in the library so both the `conseqa-viz` binary and the
//! confluence MCP server can render a model — the binaries stay thin
//! wrappers (§4 of the confluence spec). The obligation-report format
//! the overlay consumes is the analyzer's own (`analyzer::report`).

pub mod graph;
pub mod render;

pub use render::{page_data_json, render};
