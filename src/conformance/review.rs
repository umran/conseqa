//! Reviews (§14.3 of the conformance revision): a second party's check
//! of a record against the code.
//!
//! A review gives a verdict, `confirmed` or `disputed`, on the record as
//! a whole and optionally per fact and per claim, with notes. The tool
//! records the region digests it saw, so the review applies while they
//! are the record's and lapses when the code changes. Who reviewed is
//! the surface's to say, from the credential the call came with; the
//! input never names it (§14.5).
//!
//! A review is refused, by name, when the record cannot be reviewed as
//! it stands: the spec declares no such declaration (`unknown_declaration`),
//! it has no readable record (`no_record`), its code changed since the
//! record (`drifted`), or the spec did (`stale`). The record a reviewer
//! read, when it names one, must be the one on disk (`record_changed`),
//! and the review must fit the record's format (`malformed`).
//!
//! One party keeps one review at one set of digests: a new review
//! replaces its earlier one there. A review that would leave the record
//! as it is writes nothing and succeeds, so a retry after a crash
//! replays as a success.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analyzer::basis::TextualKey;
use crate::design::workspace::WorkspaceState;

use super::record::{Party, RecordContent, Records, Review, ReviewItem, Verdict};
use super::state::{self, ConditionKind, Derivation};
use super::{ConformanceError, Digest};

/// What a review supplies: never the digests, which the tool reads from
/// the record, or who reviewed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    pub declaration: TextualKey,

    /// The verdict on the record as a whole.
    pub verdict: Verdict,

    /// Verdicts on single facts and claims, each overriding the overall
    /// verdict for its target.
    #[serde(default)]
    pub items: Vec<ReviewItem>,

    /// The digest of the record file the reviewer read. Given, the
    /// review is refused if the record changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<Digest>,
}

/// What a recorded review did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reviewed {
    pub declaration: TextualKey,

    /// The record's path, relative to the project directory.
    pub record: String,

    /// The digest of the record file now.
    pub digest: Digest,

    /// Whether the review wrote the record. A review the record already
    /// holds writes nothing.
    pub written: bool,

    /// Whether it replaced the party's earlier review at these digests.
    pub replaced: bool,
}

/// Why a review was refused.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum ReviewRefusal {
    /// The spec declares no such declaration.
    UnknownDeclaration,

    /// It has no readable record: none, or one of another version, set
    /// aside with the reason given.
    NoRecord {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        set_aside: Option<String>,
    },

    /// Its code changed since the record: a review would describe code
    /// the record does not.
    Drifted,

    /// The spec changed since the record, which is to be attested again.
    Stale,

    /// The record changed since the reviewer read it.
    RecordChanged {
        expected: Option<Digest>,
        found: Option<Digest>,
    },

    /// The review breaks a rule of the record format.
    Malformed { message: String },
}

impl fmt::Display for ReviewRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDeclaration => f.write_str("the spec declares no such declaration"),

            Self::NoRecord { set_aside: None } => {
                f.write_str("it has no record to review; attest it first")
            }

            Self::NoRecord {
                set_aside: Some(reason),
            } => write!(f, "its record was set aside: {reason}"),

            Self::Drifted => f.write_str(
                "its code changed since its record was attested, so the record no longer \
                 describes it; the record is to be attested again first",
            ),

            Self::Stale => f.write_str(
                "the spec changed since its record was attested; the record is to be attested \
                 again first",
            ),

            Self::RecordChanged { expected, found } => write!(
                f,
                "the record changed since it was read: the review was of {}, but the file \
                 holds {}; read it again",
                describe(expected),
                describe(found)
            ),

            Self::Malformed { message } => f.write_str(message),
        }
    }
}

fn describe(digest: &Option<Digest>) -> String {
    match digest {
        Some(digest) => format!("the record with digest {digest}"),
        None => "no record".to_string(),
    }
}

/// A review that was not recorded.
#[derive(Debug)]
pub enum ReviewError {
    /// Refused, by name.
    Refused(Box<ReviewRefusal>),

    /// The derivation failed, or a file could not be read or written.
    Failed(ConformanceError),
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "review refused: {refusal}"),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ReviewError {}

/// Records one review of a declaration's record in the project at
/// `project_dir`, whose spec is `workspace`, on behalf of `by`, which
/// the surface takes from the credential the call came with.
pub fn record_review(
    project_dir: &Path,
    workspace: &WorkspaceState,
    input: &ReviewInput,
    by: Party,
) -> Result<Reviewed, ReviewError> {
    let refused = |refusal: ReviewRefusal| ReviewError::Refused(Box::new(refusal));
    let key = &input.declaration;

    let derivation = Derivation::of(project_dir, workspace).map_err(ReviewError::Failed)?;

    if !derivation.partition.members.contains_key(key.key()) {
        return Err(refused(ReviewRefusal::UnknownDeclaration));
    }

    let Some(file) = derivation.records.get(key) else {
        return Err(refused(ReviewRefusal::NoRecord { set_aside: None }));
    };

    let previous = match &file.content {
        RecordContent::Read(record) => record.as_ref(),

        RecordContent::OtherVersion(mismatch) => {
            return Err(refused(ReviewRefusal::NoRecord {
                set_aside: Some(mismatch.to_string()),
            }));
        }
    };

    let state = derivation
        .state
        .declaration(key)
        .expect("a declaration with a record is a bindable declaration of the spec");

    if state.has(ConditionKind::Drifted) {
        return Err(refused(ReviewRefusal::Drifted));
    }

    if state.has(ConditionKind::Stale) {
        return Err(refused(ReviewRefusal::Stale));
    }

    // The record's digests are the current regions', since it has not
    // drifted: they are what the reviewer read.
    let review = Review {
        by,
        digests: previous.digests(),
        verdict: input.verdict,
        items: input.items.clone(),
    };

    let mut record = previous.clone();
    let current = state::sorted(&review.digests);

    // Lapsed reviews go with the rewrite, as they do with an attest's,
    // and the party's earlier review at these digests is replaced where
    // it stands, so an identical review leaves the record as it is.
    record.rebind(record.regions.clone());

    let earlier = record.reviews.iter().position(|earlier| {
        earlier.by.same_as(&review.by) && state::sorted(&earlier.digests) == current
    });

    let replaced = match earlier {
        Some(index) => {
            let identical = record.reviews[index] == review;

            record.reviews[index] = review;

            !identical
        }

        None => {
            record.reviews.push(review);

            false
        }
    };

    record
        .check()
        .map_err(|message| refused(ReviewRefusal::Malformed { message }))?;

    let path = state::relative(project_dir, &file.path);

    // Nothing to write: the record already holds this review.
    if record == *previous {
        return Ok(Reviewed {
            declaration: key.clone(),
            record: path,
            digest: file.digest,
            written: false,
            replaced: false,
        });
    }

    let replaces = input.record.or(Some(file.digest));

    let digest = Records::of_project(project_dir)
        .write(&record, replaces)
        .map_err(|error| match error {
            ConformanceError::Changed {
                expected, found, ..
            } => refused(ReviewRefusal::RecordChanged { expected, found }),

            ConformanceError::Refused { message, .. } => {
                refused(ReviewRefusal::Malformed { message })
            }

            other => ReviewError::Failed(other),
        })?;

    Ok(Reviewed {
        declaration: key.clone(),
        record: path,
        digest,
        written: true,
        replaced,
    })
}
