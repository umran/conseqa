//! Change analysis: a change to a spec as an object of its own (the spec
//! change analysis revision,
//! `Conseqa_Spec_Change_Analysis_Revision_Specification.md`).
//!
//! The subject is an ordered pair of spec trees, base and head, never a
//! single model or a revision number (§3.1). Git versions the text; this
//! module says what changed in the architecture the text declares, and
//! in what it proves:
//!
//! - [`side`] reads each tree, from a commit or the working tree,
//!   through the store's own loader (§6);
//! - [`partition`] attributes every semantic fact of a workspace to
//!   exactly one declaration, and classifies the authoring content
//!   apart (§7);
//! - [`delta`] gives a modified declaration a readable account of what
//!   moved inside it (§8.2);
//! - [`diff`] lists the declarations added, removed, and modified (§8),
//!   compares the two sides' obligations by key, explains every changed
//!   verdict, and states the change in reliance (§11–§14);
//! - [`render`] writes the diff for people (§15.2);
//! - [`mutation`] holds the fixed catalogue of model mutations the
//!   explanation corpus applies (§19.2), and the fact withdrawals a
//!   work item's load-bearing check applies (Part IV §12.2).

use std::path::Path;

pub mod delta;
pub mod diff;
pub mod mutation;
pub mod partition;
pub mod render;
pub mod side;

use diff::DiffDocument;
use side::{SideError, SideName, Source};

/// Reads both sides of the project at `project_dir` and diffs them
/// (§15.1): `base` is a commit expression, read at its merge base with
/// the head when `merge_base` is set.
pub fn run(
    project_dir: &Path,
    base: &str,
    head: &Source,
    merge_base: bool,
) -> Result<DiffDocument, SideError> {
    let base = if merge_base {
        side::read_merge_base(project_dir, base, head)?
    } else {
        side::read(
            project_dir,
            SideName::Base,
            &Source::Commit(base.to_string()),
        )?
    };

    let head = side::read(project_dir, SideName::Head, head)?;

    Ok(diff::diff(&base, &head))
}
