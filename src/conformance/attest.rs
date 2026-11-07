//! Attest (§14.1–§14.2 of the conformance revision): recording what a
//! declaration's regions were checked to provide.
//!
//! The caller supplies only judgments: the facts the regions provide,
//! each with evidence, the structural claim, and any completeness
//! claims. The tool computes everything else, so a record can be wrong
//! only in its judgments, never in what code it describes (§14.5): the
//! regions and their digests from the files, the residual from the
//! spec, and the commit from git. Who attests is the surface's to say,
//! from the credential the call came with; the input never names it.
//!
//! Attest refuses, by name, unless all of these hold:
//!
//! 1. no file holding the declaration's regions or the evidence has a
//!    scan error;
//! 2. the declaration exists, is bindable, and is bound;
//! 3. every fact is required of it, as one of the fact's implementers;
//! 4. every evidence span lies in a region of the declaration or of one
//!    of the fact's anchors;
//! 5. every claim holds now;
//! 6. the read set, if given, is not stale;
//! 7. the record replaced is the one the caller read.
//!
//! An attest that would write the record already on disk succeeds
//! without writing, whatever record the caller read, so a retry after a
//! crash replays as a success. A refusal is kept on this machine,
//! outside version control, so that the declaration's next packet says
//! why the last attempt failed; the next attest of the declaration that
//! succeeds clears it.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analyzer::basis::{ConformanceClass, Fact, TextualKey};
use crate::design::change::partition::Partition;
use crate::design::change::side::Repository;
use crate::design::store::PROJECT_DIR;
use crate::design::workspace::WorkspaceState;

use super::completeness::{self, LISTED_MATCHES, Match};
use super::markers::{Region, ScanError};
use super::packet::{ReadSet, ReadSetChange};
use super::record::{
    Attestation, AttestedFact, CompletenessClaim, Party, Record, RecordedRegion, Records, Span,
    pattern_error,
};
use super::state::{self, Derivation};
use super::{ConformanceError, DIR, Digest, bindable};

/// Where this machine keeps the last refusal of each declaration's
/// attest: below `.conseqa/local/`, which the store keeps out of
/// version control, at the declaration's record path.
const REFUSALS: &str = "local/conformance/refusals";

/// What an attest supplies (§14.1): never the regions, the digests, the
/// residual, or who attests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestInput {
    pub declaration: TextualKey,

    /// The facts its regions provide, each with evidence.
    #[serde(default)]
    pub facts: Vec<AttestedFact>,

    /// That its regions implement its residual and do no modeled work
    /// beyond it.
    pub structural: bool,

    #[serde(default)]
    pub completeness: Vec<CompletenessClaim>,

    /// The read set of the packet the work was done against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_set: Option<ReadSet>,
}

/// What a successful attest did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attested {
    pub declaration: TextualKey,

    /// The record's path, relative to the project directory.
    pub record: String,

    /// The digest of the record file now.
    pub digest: Digest,

    /// Whether the attest wrote the record. An attest that would write
    /// the record already on disk writes nothing.
    pub written: bool,

    /// How many reviews the new record dropped: all of the old record's,
    /// unless it attests exactly what the old one did, at the same
    /// digests.
    pub dropped_reviews: usize,
}

/// Why an attest was refused (§14.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    /// A file holding the declaration's regions or the evidence has a
    /// scan error, so the regions it would contribute are unknown.
    ScanError { errors: Vec<ScanError> },

    /// The spec declares no such declaration.
    UnknownDeclaration,

    /// Its kind cannot be bound to code.
    NotBindable,

    /// It has no region.
    Unbound,

    /// No proof relies on the fact.
    NotRequired { fact: Fact },

    /// A proof relies on the fact, but the declaration is not one of
    /// its implementers.
    NotImplementer {
        fact: Fact,
        implementers: Vec<TextualKey>,
    },

    /// A record never attests a structural, model-wide, or internal
    /// fact: the structural claim and completeness claims provide those.
    NotAttestable { fact: Fact, class: ConformanceClass },

    /// An evidence span lies in no region of the declaration or of the
    /// fact's anchors.
    EvidenceOutside { fact: Fact, span: Span },

    /// A claim has matches outside the declared regions and its
    /// exceptions: the first twenty, and how many more.
    ClaimUnheld {
        index: usize,
        matches: Vec<Match>,
        more: usize,
    },

    /// The spec moved since the packet was built.
    Stale { changes: Vec<ReadSetChange> },

    /// The record was replaced since the caller read it.
    RecordChanged {
        expected: Option<Digest>,
        found: Option<Digest>,
    },

    /// The input breaks a rule of the record format.
    Malformed { message: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScanError { errors } => write!(
                f,
                "a file holding its regions or the evidence has a scan error, so its regions \
                 cannot be computed: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),

            Self::UnknownDeclaration => f.write_str("the spec declares no such declaration"),

            Self::NotBindable => {
                f.write_str("its kind cannot be bound to code, so it has no record")
            }

            Self::Unbound => f.write_str(
                "it has no region: mark its code with `conseqa:begin` and `conseqa:end` lines \
                 naming it first",
            ),

            Self::NotRequired { fact } => write!(
                f,
                "no proof relies on `{fact}`, so no record attests it; a record attests only \
                 what the current proofs need"
            ),

            Self::NotImplementer { fact, implementers } => write!(
                f,
                "`{fact}` is required of {}, not of this declaration",
                if implementers.is_empty() {
                    "no declaration".to_string()
                } else {
                    implementers
                        .iter()
                        .map(|implementer| format!("`{implementer}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),

            Self::NotAttestable { fact, class } => write!(
                f,
                "`{fact}` is a {} fact, which no record attests: {}",
                match class {
                    ConformanceClass::Attestable => "attestable",
                    ConformanceClass::Structural => "structural",
                    ConformanceClass::ModelWide => "model-wide",
                    ConformanceClass::Internal => "internal",
                },
                match class {
                    ConformanceClass::Structural => {
                        "its anchors provide it by claiming structural conformance"
                    }

                    ConformanceClass::ModelWide => "completeness claims provide it",

                    ConformanceClass::Attestable | ConformanceClass::Internal => {
                        "no code provides it"
                    }
                }
            ),

            Self::EvidenceOutside { fact, span } => write!(
                f,
                "the evidence {}:{}-{} for `{fact}` lies in no region of this declaration or of \
                 the fact's anchors; a span must lie strictly between a region's markers",
                span.path, span.lines[0], span.lines[1]
            ),

            Self::ClaimUnheld {
                index,
                matches,
                more,
            } => write!(
                f,
                "completeness[{index}] does not hold: {} match{} outside the declared regions \
                 and its exceptions, the first at {}",
                matches.len() + more,
                if matches.len() + more == 1 { "" } else { "es" },
                matches
                    .first()
                    .map(|found| format!("{}:{}-{}", found.path, found.lines[0], found.lines[1]))
                    .unwrap_or_default()
            ),

            Self::Stale { changes } => write!(
                f,
                "the spec moved since the packet was built ({}); fetch a fresh packet and check \
                 the work against it",
                changes
                    .iter()
                    .map(|change| match change {
                        ReadSetChange::Changed { declaration } => format!("{declaration} changed"),
                        ReadSetChange::Removed { declaration } => format!("{declaration} removed"),
                        ReadSetChange::Query { query } => format!(
                            "the result of {} changed",
                            serde_json::to_string(query).unwrap_or_default()
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            ),

            Self::RecordChanged { expected, found } => write!(
                f,
                "the record changed since it was read: the attest replaces {}, but the file \
                 holds {}; fetch a fresh packet",
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

/// An attest that did not succeed.
#[derive(Debug)]
pub enum AttestError {
    /// Refused, by name.
    Refused(Box<Refusal>),

    /// The derivation failed: the spec does not assemble and validate,
    /// the records or the policy do not load, or a file could not be
    /// read or written.
    Failed(ConformanceError),
}

impl fmt::Display for AttestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "attest refused: {refusal}"),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for AttestError {}

/// The last refusal of a declaration's attest, as this machine keeps
/// it: shown in the declaration's packet while its record is the one
/// the refused attest saw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastRefusal {
    pub by: Party,

    /// The digest of the record the refused attest saw, `None` when it
    /// had none.
    #[serde(default)]
    pub record: Option<Digest>,

    /// The refusal, by name, with what it found.
    pub refusal: Value,

    pub message: String,
}

/// Attests a declaration of the project at `project_dir` (§14), whose
/// spec is `workspace`, on behalf of `by`, which the surface takes from
/// the credential the call came with.
pub fn attest(
    project_dir: &Path,
    workspace: &WorkspaceState,
    input: &AttestInput,
    by: Party,
) -> Result<Attested, AttestError> {
    let result = run(project_dir, workspace, input, &by);
    let key = &input.declaration;

    match &result {
        Ok(_) => {
            let _ = fs::remove_file(refusal_path(project_dir, key));
        }

        // Kept only for a declaration of the spec, whose packet shows it.
        Err(AttestError::Refused(refusal))
            if Partition::of(workspace).members.contains_key(key.key()) =>
        {
            keep(
                project_dir,
                key,
                &LastRefusal {
                    by,
                    record: record_digest(project_dir, key),
                    refusal: serde_json::to_value(&**refusal).unwrap_or(Value::Null),
                    message: refusal.to_string(),
                },
            );
        }

        Err(_) => {}
    }

    result
}

fn run(
    project_dir: &Path,
    workspace: &WorkspaceState,
    input: &AttestInput,
    by: &Party,
) -> Result<Attested, AttestError> {
    let refused = |refusal: Refusal| AttestError::Refused(Box::new(refusal));
    let key = &input.declaration;
    let declaration = key.key();

    // The claims are matched as the scan reads the files, so their
    // patterns must compile first.
    for (index, claim) in input.completeness.iter().enumerate() {
        for (pattern_index, pattern) in claim.patterns.iter().enumerate() {
            if let Err(error) = regex::Regex::new(pattern) {
                return Err(refused(Refusal::Malformed {
                    message: format!(
                        "completeness[{index}].patterns[{pattern_index}] is not a regular \
                         expression: {}",
                        pattern_error(&error)
                    ),
                }));
            }
        }
    }

    let (derivation, matches) = Derivation::matching(project_dir, workspace, &input.completeness)
        .map_err(AttestError::Failed)?;

    // 1. Scan errors.
    let evidence: BTreeSet<&str> = input
        .facts
        .iter()
        .flat_map(|attested| &attested.evidence)
        .map(|span| span.path.as_str())
        .collect();

    let errors: Vec<ScanError> = derivation
        .scan
        .errors
        .iter()
        .filter(|error| {
            evidence.contains(error.path.as_str())
                || derivation.named.get(&error.path).is_some_and(|named| {
                    named
                        .iter()
                        .any(|text| text.parse::<TextualKey>().is_ok_and(|named| &named == key))
                })
        })
        .cloned()
        .collect();

    if !errors.is_empty() {
        return Err(refused(Refusal::ScanError { errors }));
    }

    // 2. The declaration.
    if !derivation.partition.members.contains_key(declaration) {
        return Err(refused(Refusal::UnknownDeclaration));
    }

    if !bindable(declaration) {
        return Err(refused(Refusal::NotBindable));
    }

    let regions: Vec<RecordedRegion> = derivation
        .regions_of(declaration)
        .map(RecordedRegion::from)
        .collect();

    if regions.is_empty() {
        return Err(refused(Refusal::Unbound));
    }

    // The record, as far as the tool computes it, under its format's
    // rules: one entry per fact, each with evidence, and claims only on
    // the kinds that carry them. They are checked as the caller wrote
    // them, so a refusal's indices are the caller's.
    let mut record = Record::new(
        key.clone(),
        derivation.partition.members[declaration].clone(),
        Attestation {
            by: by.clone(),
            commit: Repository::of(project_dir).and_then(|repository| repository.resolve("HEAD")),
        },
    );

    record.regions = regions;
    record.structural = input.structural;
    record.facts = input.facts.clone();
    record.completeness = input.completeness.clone();

    // The realization the code is checked against, when a catalogue
    // entry realizes the declaration (Part VI §18, item 3).
    record.realization = derivation
        .realization
        .as_ref()
        .and_then(|realization| realization.of(key));

    record
        .check()
        .map_err(|message| refused(Refusal::Malformed { message }))?;

    // Then held canonically: facts by canonical form, each span once.
    for attested in &mut record.facts {
        attested
            .evidence
            .sort_by(|a, b| (&a.path, a.lines).cmp(&(&b.path, b.lines)));
        attested.evidence.dedup();
    }

    record
        .facts
        .sort_by_key(|attested| attested.fact.canonical());

    // 3. Every fact is required of this declaration.
    let state = derivation
        .state
        .declaration(key)
        .expect("a bindable declaration of the spec has a state");

    for attested in &input.facts {
        let fact = &attested.fact;
        let class = fact.conformance_class();

        if class != ConformanceClass::Attestable {
            return Err(refused(Refusal::NotAttestable {
                fact: fact.clone(),
                class,
            }));
        }

        let canonical = fact.canonical();

        if state
            .required
            .iter()
            .any(|required| required.fact.canonical() == canonical)
        {
            continue;
        }

        return Err(refused(if derivation.anchors.contains_key(&canonical) {
            Refusal::NotImplementer {
                fact: fact.clone(),
                implementers: state::implementers(fact, workspace, &derivation.graph)
                    .into_iter()
                    .filter_map(TextualKey::new)
                    .collect(),
            }
        } else {
            Refusal::NotRequired { fact: fact.clone() }
        }));
    }

    // 4. Evidence lies in an allowed region.
    for attested in &input.facts {
        let anchors = derivation.anchors.get(&attested.fact.canonical());

        let allowed: Vec<&Region> = derivation
            .scan
            .regions
            .iter()
            .filter(|region| {
                let bound = region.key.key();

                bound == declaration || anchors.is_some_and(|anchors| anchors.contains(bound))
            })
            .collect();

        for span in &attested.evidence {
            let inside = allowed.iter().any(|region| {
                region.path == span.path
                    && region.lines[0] < span.lines[0]
                    && span.lines[1] < region.lines[1]
            });

            if !inside {
                return Err(refused(Refusal::EvidenceOutside {
                    fact: attested.fact.clone(),
                    span: span.clone(),
                }));
            }
        }
    }

    // 5. Every claim holds now.
    for (index, (claim, found)) in input.completeness.iter().zip(&matches).enumerate() {
        let declared =
            completeness::declared(claim.scope, declaration, workspace, &derivation.graph);

        let accepted: Vec<&Region> = derivation
            .scan
            .regions
            .iter()
            .filter(|region| declared.contains(region.key.key()))
            .collect();

        let outside = completeness::outside(claim, found, &accepted);

        if !outside.is_empty() {
            return Err(refused(Refusal::ClaimUnheld {
                index,
                more: outside.len().saturating_sub(LISTED_MATCHES),
                matches: outside.into_iter().take(LISTED_MATCHES).collect(),
            }));
        }
    }

    // 6. The read set.
    if let Some(read_set) = &input.read_set {
        let changes = read_set.changes(&derivation);

        if !changes.is_empty() {
            return Err(refused(Refusal::Stale { changes }));
        }
    }

    // A review covers the code it saw and the record it read. It
    // survives only a record attesting exactly what its own did, at the
    // same digests: an overall verdict would otherwise confirm facts it
    // never saw, and its items name facts by index.
    let file = derivation.records.get(key);
    let previous = derivation.record(key);
    let mut dropped_reviews = 0;

    if let Some(previous) = previous {
        if attests_the_same(previous, &record) {
            record.reviews = previous
                .reviews
                .iter()
                .filter(|review| record.reviewed_as_recorded(review))
                .cloned()
                .collect();
        }

        dropped_reviews = previous.reviews.len() - record.reviews.len();

        // Nothing to write: the record already holds this attest.
        if let Some(file) = file
            && same_but_commit(previous, &record)
        {
            return Ok(Attested {
                declaration: key.clone(),
                record: state::relative(project_dir, &file.path),
                digest: file.digest,
                written: false,
                dropped_reviews: 0,
            });
        }
    }

    // 7. The record replaced is the one the caller read.
    let replaces = match &input.read_set {
        Some(read_set) => read_set.record,
        None => file.map(|file| file.digest),
    };

    let records = Records::of_project(project_dir);

    let digest = records
        .write(&record, replaces)
        .map_err(|error| match error {
            ConformanceError::Changed {
                expected, found, ..
            } => refused(Refusal::RecordChanged { expected, found }),

            ConformanceError::Refused { message, .. } => refused(Refusal::Malformed { message }),

            other => AttestError::Failed(other),
        })?;

    Ok(Attested {
        declaration: key.clone(),
        record: state::relative(project_dir, &records.path(key)),
        digest,
        written: true,
        dropped_reviews,
    })
}

/// Whether two records attest the same things of the same code: the
/// residual, the structural claim, the realization, the facts in order,
/// the claims, and the region digests.
fn attests_the_same(previous: &Record, record: &Record) -> bool {
    let canonical = |record: &Record| -> Vec<String> {
        record
            .facts
            .iter()
            .map(|attested| attested.fact.canonical())
            .collect()
    };

    previous.residual == record.residual
        && previous.structural == record.structural
        && previous.realization == record.realization
        && canonical(previous) == canonical(record)
        && previous.completeness == record.completeness
        && previous.digests() == record.digests()
}

/// Whether two records are equal but for the commit the attester's tree
/// was based on, which is informational.
fn same_but_commit(previous: &Record, record: &Record) -> bool {
    let mut previous = previous.clone();

    previous.attested.commit = record.attested.commit.clone();

    previous == *record
}

/// The digest of a declaration's record file now.
fn record_digest(project_dir: &Path, key: &TextualKey) -> Option<Digest> {
    fs::read(Records::of_project(project_dir).path(key))
        .ok()
        .map(|bytes| Digest::of(&bytes))
}

/// Where this machine keeps a declaration's last refusal.
fn refusal_path(project_dir: &Path, key: &TextualKey) -> PathBuf {
    let conformance = project_dir.join(PROJECT_DIR).join(DIR);
    let record = Records::of_project(project_dir).path(key);

    let relative = record
        .strip_prefix(&conformance)
        .expect("a record lives in the conformance directory");

    project_dir
        .join(PROJECT_DIR)
        .join(REFUSALS)
        .join(relative)
        .with_extension("json")
}

/// The last refusal of a declaration's attest on this machine, if one
/// is kept. Unreadable state reads as none: it is guidance, and no
/// condition rests on it.
pub fn last_refusal(project_dir: &Path, key: &TextualKey) -> Option<LastRefusal> {
    let bytes = fs::read(refusal_path(project_dir, key)).ok()?;

    serde_json::from_slice(&bytes).ok()
}

/// Keeps a refusal, through a temporary sibling and a rename. A failure
/// to keep it is ignored: it is guidance, and the refusal itself still
/// reaches the caller.
fn keep(project_dir: &Path, key: &TextualKey, refusal: &LastRefusal) {
    let path = refusal_path(project_dir, key);

    let Some(dir) = path.parent() else {
        return;
    };

    let Ok(text) = serde_json::to_vec_pretty(refusal) else {
        return;
    };

    let temporary = dir.join(format!(".refusal.tmp-{}", std::process::id()));

    let kept = fs::create_dir_all(dir)
        .and_then(|()| fs::write(&temporary, text))
        .and_then(|()| fs::rename(&temporary, &path));

    if kept.is_err() {
        let _ = fs::remove_file(&temporary);
    }
}
