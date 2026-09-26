//! Questions are reviewed data (§12 of the System One orchestration
//! revision).
//!
//! Every question this layer asks is written here and nowhere else, so
//! the entire decision surface can be read in one place. What needs
//! review is the wording and the thresholds that consume the answers —
//! not the plumbing around them.
//!
//! Authoring rules:
//!
//! - one question asks one narrow, coherent judgment. Independently
//!   useful dimensions are split and combined in code, without
//!   splitting apart the relationship being judged; narrow does not
//!   mean literal fact extraction;
//! - a question id is for code and is never seen by the model: the
//!   instructions and criteria carry the complete meaning;
//! - the question states its exact condition and its boundary cases; it
//!   is answered as written, not as meant;
//! - nothing code can compute — a count, an ordering, a comparison of
//!   versions, positions or dates — is asked of the model;
//! - an optional decision is preceded by a `stated` noul, and when the
//!   answer is no, the default stands;
//! - a choice whose right answer may be missing carries an explicit
//!   no-match option, and code checks candidate coverage;
//! - a speculative question states its premise explicitly;
//! - the state contains only what the questions need.
//!
//! Changing a question's wording bumps its [`QuestionSpec::version`].
//! The wording is part of every replay key, so a stale recording can
//! never answer a reworded question; the version is what makes the
//! change visible in the decision log.

pub mod conformance;
pub mod discovery;
pub mod repair;

/// The identity of one reviewed question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionSpec {
    /// Stable across rewordings, and unique within the layer.
    pub id: &'static str,

    /// Bumped whenever the wording or the criteria change.
    pub version: u32,

    /// What the answer is used to decide, for a reviewer.
    pub decides: &'static str,
}

impl QuestionSpec {
    /// The form the decision log tags an ask with: `id@version`.
    pub fn tag(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}
