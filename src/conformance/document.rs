//! Conformance documents: the JSON each `conseqa conform` subcommand
//! prints with `--json`, and each conformance tool of the design MCP
//! server returns.
//!
//! A tool that drives Conseqa without a person reading its output, such
//! as a delivery loop, routes on fields, never on text. So every
//! document is one JSON object that names its format and what it is,
//! before its payload:
//!
//! - `format`: this module's [`FORMAT`], counted apart from the records'
//!   and the policy's, since a document and a record change for
//!   different reasons;
//! - `document`: `status`, `plan`, `item`, `attest`, `review`, `scan`,
//!   or `check`;
//! - `dsl` and `fact_format`: the contract the facts in it are written
//!   in.
//!
//! The same inputs give a byte-identical document on any machine
//! (§18.10): every list in one has a defined order, and nothing in one
//! records when or where it was made.

use serde::Serialize;

use crate::analyzer::basis::{FACT_FORMAT, FactFormat, TextualKey};
use crate::spec::{DSL_VERSION, DslVersion};

use super::attest::{Attested, Refusal};
use super::check::Check;
use super::markers::{Region, ScanError};
use super::packet::Packet;
use super::plan::{Item, Scope};
use super::record::Party;
use super::review::{ReviewRefusal, Reviewed};
use super::state::State;

/// The conformance document format. It counts every change to a
/// document: a field added, removed, renamed, or retyped.
pub const FORMAT: u32 = 1;

/// One conformance document: its format and kind, then its payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Document<T> {
    pub format: u32,
    pub document: &'static str,
    pub dsl: DslVersion,
    pub fact_format: FactFormat,

    #[serde(flatten)]
    pub payload: T,
}

impl<T: Serialize> Document<T> {
    fn new(document: &'static str, payload: T) -> Self {
        Self {
            format: FORMAT,
            document,
            dsl: DSL_VERSION,
            fact_format: FACT_FORMAT,
            payload,
        }
    }

    /// The document as compact JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a conformance document serializes")
    }

    /// The document as indented JSON, for a person or a report file.
    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a conformance document serializes")
    }
}

/// `status`: every bindable declaration with its conditions, the
/// orphaned keys, every proven obligation's conformance, and the scan
/// errors (§10).
pub fn status(state: &State) -> Document<&State> {
    Document::new("status", state)
}

/// `plan`: the work list, and the diff that scoped it, if one did
/// (§11).
pub fn plan<'a>(items: &'a [Item], scope: Option<&'a Scope>) -> Document<PlanPayload<'a>> {
    Document::new("plan", PlanPayload { scope, items })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanPayload<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<&'a Scope>,

    pub items: &'a [Item],
}

/// `item`: one work item's packet (§12).
pub fn item(packet: &Packet) -> Document<&Packet> {
    Document::new("item", packet)
}

/// `attest`: what an attest did, or why it was refused (§14.2).
pub fn attest(outcome: AttestOutcome<'_>) -> Document<AttestOutcome<'_>> {
    Document::new("attest", outcome)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AttestOutcome<'a> {
    /// The record was written.
    Written {
        #[serde(flatten)]
        attested: &'a Attested,
    },

    /// The record already held what the attest would write.
    Unchanged {
        #[serde(flatten)]
        attested: &'a Attested,
    },

    /// Refused, by name, with what the refusal found.
    Refused {
        #[serde(flatten)]
        refusal: &'a Refusal,

        message: String,
    },
}

impl<'a> AttestOutcome<'a> {
    pub fn of(result: Result<&'a Attested, &'a Refusal>) -> Self {
        match result {
            Ok(attested) if attested.written => Self::Written { attested },
            Ok(attested) => Self::Unchanged { attested },
            Err(refusal) => Self::Refused {
                refusal,
                message: refusal.to_string(),
            },
        }
    }
}

/// `review`: what became of each review asked for: one recorded
/// through the design MCP server, or each record `conform review` gave
/// an agent (§14.3).
pub fn review(reviews: Vec<ReviewOutcome>) -> Document<ReviewPayload> {
    Document::new("review", ReviewPayload { reviews })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewPayload {
    pub reviews: Vec<ReviewOutcome>,
}

/// What became of one review.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewOutcome {
    pub declaration: TextualKey,

    /// Who reviewed, or was asked to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Party>,

    #[serde(flatten)]
    pub result: ReviewResult,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReviewResult {
    /// The review was written into the record.
    Recorded {
        #[serde(flatten)]
        reviewed: Reviewed,
    },

    /// The record already held the review.
    Unchanged {
        #[serde(flatten)]
        reviewed: Reviewed,
    },

    /// Refused, by name, with what the refusal found.
    Refused {
        #[serde(flatten)]
        refusal: ReviewRefusal,

        message: String,
    },

    /// The agent ended without recording a review.
    NotReported { message: String },
}

impl ReviewResult {
    pub fn of(result: Result<Reviewed, ReviewRefusal>) -> Self {
        match result {
            Ok(reviewed) if reviewed.written => Self::Recorded { reviewed },
            Ok(reviewed) => Self::Unchanged { reviewed },
            Err(refusal) => Self::Refused {
                message: refusal.to_string(),
                refusal,
            },
        }
    }
}

/// `scan`: the regions and scan errors of the project's files, read
/// without the spec (§6).
pub fn scan<'a>(
    files: usize,
    regions: &'a [Region],
    errors: &'a [ScanError],
) -> Document<ScanPayload<'a>> {
    Document::new(
        "scan",
        ScanPayload {
            files,
            regions,
            errors,
        },
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScanPayload<'a> {
    /// How many files the scan read.
    pub files: usize,

    pub regions: &'a [Region],
    pub errors: &'a [ScanError],
}

/// `check`: whether the check passed, and what failed it, within the
/// diff that scoped it, if one did (§15.3).
pub fn check<'a>(check: &'a Check, scope: Option<&'a Scope>) -> Document<CheckPayload<'a>> {
    Document::new("check", CheckPayload { scope, check })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckPayload<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<&'a Scope>,

    #[serde(flatten)]
    pub check: &'a Check,
}
