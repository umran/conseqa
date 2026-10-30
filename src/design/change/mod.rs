//! Change analysis: a change to a spec as an object of its own (the spec
//! change analysis revision,
//! `Conseqa_Spec_Change_Analysis_Revision_Specification.md`).
//!
//! The subject is an ordered pair of spec trees, base and head, never a
//! single model or a revision number (§3.1). [`side`] reads each tree,
//! from a commit or the working tree, through the store's own loader
//! (§6).

pub mod side;
