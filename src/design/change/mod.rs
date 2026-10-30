//! Change analysis: a change to a spec as an object of its own (the spec
//! change analysis revision,
//! `Conseqa_Spec_Change_Analysis_Revision_Specification.md`).
//!
//! The subject is an ordered pair of spec trees, base and head, never a
//! single model or a revision number (§3.1). Git versions the text; this
//! module says what changed in the architecture the text declares:
//!
//! - [`side`] reads each tree, from a commit or the working tree,
//!   through the store's own loader (§6);
//! - [`partition`] attributes every semantic fact of a workspace to
//!   exactly one declaration, and classifies the authoring content
//!   apart (§7);
//! - [`delta`] gives a modified declaration a readable account of what
//!   moved inside it (§8.2);
//! - [`diff`] lists the declarations added, removed, and modified (§8).

pub mod delta;
pub mod diff;
pub mod partition;
pub mod side;
