//! Realization documents: the JSON each `conseqa realize` subcommand
//! prints with `--json`, and each realization tool of the design MCP
//! server returns (T1 of the delivery loop orchestrator design).
//!
//! As the conformance documents do, every document is one JSON object
//! that names its format and what it is, before its payload:
//!
//! - `format`: this module's [`FORMAT`], counted apart from
//!   [`REALIZATION_FORMAT`], since a document and a profile file change
//!   for different reasons;
//! - `document`: `realization_status`, `realization_explain`,
//!   `realization_choice`, `catalogue`, or `catalogue_entry`, named so
//!   that a tool routing on the field never confuses one with a
//!   conformance document;
//! - `dsl` and `realization_format`: the contract the needs and the
//!   capabilities in it are written in.
//!
//! The same inputs give a byte-identical document on any machine:
//! every list has a defined order, and nothing records when or where
//! it was made.

use serde::Serialize;

use crate::spec::{DSL_VERSION, DslVersion};

use super::REALIZATION_FORMAT;
use super::capability::{Capability, RealizableKind};
use super::catalogue::{Catalogue, Catalogued, Entry, EntryRef, Source, standard_name};
use super::choice::{Chosen, Explanation, Refusal, Status};

/// The realization document format. It counts every change to a
/// document: a field added, removed, renamed, or retyped.
pub const FORMAT: u32 = 1;

/// One realization document: its format and kind, then its payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Document<T> {
    pub format: u32,
    pub document: &'static str,
    pub dsl: DslVersion,
    pub realization_format: u32,

    #[serde(flatten)]
    pub payload: T,
}

impl<T: Serialize> Document<T> {
    fn new(document: &'static str, payload: T) -> Self {
        Self {
            format: FORMAT,
            document,
            dsl: DSL_VERSION,
            realization_format: REALIZATION_FORMAT,
            payload,
        }
    }

    /// The document as compact JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a realization document serializes")
    }

    /// The document as indented JSON, for a person or a report file.
    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a realization document serializes")
    }
}

/// `realization_status`: every realizable declaration's needs and
/// state, and the orphaned choices (§15.1).
pub fn status(status: &Status) -> Document<&Status> {
    Document::new("realization_status", status)
}

/// `realization_explain`: one declaration's candidates (§15.1).
pub fn explain(explanation: &Explanation) -> Document<&Explanation> {
    Document::new("realization_explain", explanation)
}

/// What became of a choice.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ChoiceOutcome<'a> {
    /// The choice file was written.
    Written {
        #[serde(flatten)]
        chosen: &'a Chosen,
    },

    /// An identical choice was already written, so nothing was.
    Unchanged {
        #[serde(flatten)]
        chosen: &'a Chosen,
    },

    /// The choice was refused, naming why.
    Refused {
        #[serde(flatten)]
        refusal: &'a Refusal,
        message: String,
    },
}

impl<'a> ChoiceOutcome<'a> {
    pub fn of(outcome: Result<&'a Chosen, &'a Refusal>) -> Self {
        match outcome {
            Ok(chosen) if chosen.written => Self::Written { chosen },
            Ok(chosen) => Self::Unchanged { chosen },
            Err(refusal) => Self::Refused {
                refusal,
                message: refusal.to_string(),
            },
        }
    }
}

/// `realization_choice`: what `choose` did (§15.1).
pub fn choice(outcome: ChoiceOutcome<'_>) -> Document<ChoiceOutcome<'_>> {
    Document::new("realization_choice", outcome)
}

/// One entry, as the catalogue list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listed {
    pub entry: EntryRef,
    pub source: Source,
    pub technology: String,
    pub versions: String,

    /// The kind a platform entry realizes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realizes: Option<RealizableKind>,

    /// The primitive an idiom entry shows how to write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primitive: Option<Capability>,
}

/// The catalogue list: the standard catalogue's name, and every entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub catalogue: String,
    pub entries: Vec<Listed>,
}

impl Listing {
    pub fn of(catalogue: &Catalogue) -> Self {
        Self {
            catalogue: standard_name(),
            entries: catalogue
                .entries()
                .map(|catalogued| Listed {
                    entry: catalogued.entry.entry_ref(),
                    source: catalogued.source,
                    technology: catalogued.entry.technology().name.clone(),
                    versions: catalogued.entry.technology().versions.to_string(),
                    realizes: match &catalogued.entry {
                        Entry::Platform(entry) => Some(entry.kind()),
                        Entry::Idiom(_) => None,
                    },
                    primitive: match &catalogued.entry {
                        Entry::Platform(_) => None,
                        Entry::Idiom(entry) => Some(entry.primitive),
                    },
                })
                .collect(),
        }
    }

    pub fn render(&self) -> String {
        let mut text = format!("{} and the project's entries\n", self.catalogue);

        for listed in &self.entries {
            let what = match (&listed.realizes, &listed.primitive) {
                (Some(kind), _) => format!("realizes {kind}"),
                (None, Some(primitive)) => format!("idiom for {primitive}"),
                (None, None) => String::new(),
            };

            text.push_str(&format!(
                "{:<34} {:<8} {} {:<14} {what}\n",
                listed.entry.to_string(),
                listed.source.to_string(),
                listed.technology,
                listed.versions,
            ));
        }

        text
    }
}

/// `catalogue`: every entry the project can choose from (§15.1).
pub fn catalogue(listing: &Listing) -> Document<&Listing> {
    Document::new("catalogue", listing)
}

/// `catalogue_entry`: one entry in full, with its source (§15.1).
pub fn catalogue_entry(catalogued: &Catalogued) -> Document<&Catalogued> {
    Document::new("catalogue_entry", catalogued)
}
