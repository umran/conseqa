//! Conformance records (§8 of the conformance revision).
//!
//! A record is one committed file per declaration, at the declaration's
//! textual key under `.conseqa/conformance/` (§8.1). It states what the
//! declaration's regions were checked to provide (§8.2):
//!
//! - the residual they were checked against;
//! - the regions and digests the tool computed;
//! - the structural claim;
//! - each attested fact, with its evidence;
//! - completeness claims;
//! - who attested;
//! - the reviews.
//!
//! The record holds the residual itself, not its digest (§8.3), so
//! "what changed in this declaration since its code was checked" has an
//! answer however the history was squashed, rebased, or shallowly
//! cloned. One file per declaration means two branches conflict only
//! where both checked the same declaration (§8.5).
//!
//! The reader is as strict as the store's. Any of these is refused by
//! name, and the read fails:
//!
//! - a directory that is not a bindable kind;
//! - a file name that is not an id's canonical encoding;
//! - a stray entry;
//! - an unknown field;
//! - a fact not in its canonical form.
//!
//! The one exception is a record written on another version axis:
//! another conformance format, DSL, or fact format. It is set aside
//! unread, and its declaration reads as `unattested`, with the reason
//! (§8.6). A DSL bump therefore leaves every declaration unattested
//! instead of failing every derivation. The declarations' meaning
//! changed, so every check is owed again.
//!
//! Writes are atomic and optimistic (§8.4). Each one names the digest
//! of the record it replaces, or its absence, and is refused if the file
//! changed meanwhile. A short lock in `.conseqa/local/` makes the
//! comparison and the rename one step across processes; it is never the
//! spec writer lock. Hand edits are authoritative on read, as in the
//! spec: a record with wrong digests simply reads as drifted.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::analyzer::basis::{
    FACT_FORMAT, Fact, FactFormat, FactVersionError, TextualKey, check_versions, read_canonical,
};
use crate::design::store::PROJECT_DIR;
use crate::spec::{DSL_VERSION, DslVersion, Id};

use super::markers::Region;
use super::{ConformanceError, DIR, Digest, FORMAT, bindable, bindable_kinds, policy};

/// The lock a record write holds while it compares and renames: in the
/// machine-local directory the store's `.gitignore` already excludes.
const LOCK: &str = "local/conformance.lock";

/// One declaration's conformance record (§8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The conformance format.
    pub format: u32,

    pub dsl: DslVersion,
    pub fact_format: FactFormat,

    /// The declaration the record is about, which its path also names.
    pub declaration: TextualKey,

    /// The declaration's residual when its code was checked, as Part
    /// I's partition projects it (§7.2 there).
    pub residual: Value,

    /// The regions bound to the declaration when it was checked, as the
    /// tool computed them from the files (§14.2).
    pub regions: Vec<RecordedRegion>,

    /// The structural claim (§7.1): the regions implement the residual
    /// and do no modeled work beyond it.
    pub structural: bool,

    pub facts: Vec<AttestedFact>,

    /// Claims that no unbound code touches what a model-wide fact
    /// depends on (§9). Only `data_object`, `topic`, and `outbox`
    /// records carry them.
    #[serde(default)]
    pub completeness: Vec<CompletenessClaim>,

    pub attested: Attestation,

    #[serde(default)]
    pub reviews: Vec<Review>,
}

/// A region as a record holds it (§8.2): its line range is
/// presentation, and its digest is what the record's validity rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedRegion {
    pub path: String,

    /// The lines of its begin and end markers.
    pub lines: [usize; 2],

    pub digest: Digest,
}

impl From<&Region> for RecordedRegion {
    fn from(region: &Region) -> Self {
        Self {
            path: region.path.clone(),
            lines: region.lines,
            digest: region.digest,
        }
    }
}

/// An attested fact and the spans evidencing it (§7.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestedFact {
    /// In its durable form (Part I §10.9), read only when canonical.
    #[serde(deserialize_with = "canonical_fact")]
    pub fact: Fact,

    pub evidence: Vec<Span>,
}

fn canonical_fact<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Fact, D::Error> {
    let value = Value::deserialize(deserializer)?;

    read_canonical(&value).map_err(D::Error::custom)
}

/// A file and a line range within it. Spans are presentation: their
/// lines move as the file changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub path: String,

    /// The first and last line, counted from 1.
    pub lines: [usize; 2],
}

/// A completeness claim (§9.2): every match of the patterns in the
/// scanned files lies in a region of a declared writer or publisher, or
/// in an excepted path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletenessClaim {
    pub scope: ClaimScope,
    pub patterns: Vec<String>,

    #[serde(default)]
    pub exceptions: Vec<Exception>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimScope {
    /// The object's writers: a `data_object` record's claim.
    Writers,

    /// The channel's publishers: a `topic` or `outbox` record's claim.
    Publishers,
}

impl fmt::Display for ClaimScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Writers => "writers",
            Self::Publishers => "publishers",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exception {
    pub path: String,
    pub reason: String,
}

/// Who attested, and from where (§14.5): a claim, recorded and never
/// authenticated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub by: Party,

    /// The commit the attester's tree was based on: informational, and
    /// absent outside git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// An attester or a reviewer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Party {
    pub kind: PartyKind,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyKind {
    Agent,
    Person,

    /// A catalogue entry's deterministic checker (§14.3).
    Checker,
}

/// A second party's check of the record against the code (§14.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub by: Party,

    /// The region digests the review saw. It applies while they are the
    /// record's.
    pub digests: Vec<Digest>,

    /// The verdict on the record as a whole.
    pub verdict: Verdict,

    #[serde(default)]
    pub items: Vec<ReviewItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Confirmed,
    Disputed,
}

/// A verdict on one thing the record carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ItemFile", into = "ItemFile")]
pub struct ReviewItem {
    pub target: ReviewTarget,
    pub verdict: Verdict,
    pub note: Option<String>,
}

/// What a review item is about: an attested fact or a completeness
/// claim, by its index in the record, or the structural claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTarget {
    Fact(usize),
    Completeness(usize),
    Structural,
}

/// A review item as written: exactly one of `fact`, `completeness`, and
/// `structural: true` names its target.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fact: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    completeness: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    structural: Option<bool>,

    verdict: Verdict,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

const ONE_TARGET: &str = "a review item names exactly one of `fact: <index>`, \
                          `completeness: <index>`, and `structural: true`";

impl TryFrom<ItemFile> for ReviewItem {
    type Error = String;

    fn try_from(file: ItemFile) -> Result<Self, String> {
        let target = match (file.fact, file.completeness, file.structural) {
            (Some(index), None, None) => ReviewTarget::Fact(index),
            (None, Some(index), None) => ReviewTarget::Completeness(index),
            (None, None, Some(true)) => ReviewTarget::Structural,

            _ => return Err(ONE_TARGET.to_string()),
        };

        Ok(Self {
            target,
            verdict: file.verdict,
            note: file.note,
        })
    }
}

impl From<ReviewItem> for ItemFile {
    fn from(item: ReviewItem) -> Self {
        let (fact, completeness, structural) = match item.target {
            ReviewTarget::Fact(index) => (Some(index), None, None),
            ReviewTarget::Completeness(index) => (None, Some(index), None),
            ReviewTarget::Structural => (None, None, Some(true)),
        };

        Self {
            fact,
            completeness,
            structural,
            verdict: item.verdict,
            note: item.note,
        }
    }
}

impl Record {
    /// A record of this build's versions, with nothing attested yet.
    pub fn new(declaration: TextualKey, residual: Value, attested: Attestation) -> Self {
        Self {
            format: FORMAT,
            dsl: DSL_VERSION,
            fact_format: FACT_FORMAT,
            declaration,
            residual,
            regions: Vec::new(),
            structural: false,
            facts: Vec::new(),
            completeness: Vec::new(),
            attested,
            reviews: Vec::new(),
        }
    }

    /// The record's region digests, sorted.
    pub fn digests(&self) -> Vec<Digest> {
        let mut digests: Vec<Digest> = self.regions.iter().map(|region| region.digest).collect();

        digests.sort();

        digests
    }

    /// Whether a review saw exactly the record's regions: its digests,
    /// counted as a multiset, are the record's (§14.3). A review covers
    /// the code it saw, so a region added since lapses it as surely as
    /// one edited.
    pub fn reviewed_as_recorded(&self, review: &Review) -> bool {
        let mut digests = review.digests.clone();

        digests.sort();

        digests == self.digests()
    }

    /// Replaces the regions, dropping every review that saw others
    /// (§14.2).
    pub fn rebind(&mut self, regions: Vec<RecordedRegion>) {
        self.regions = regions;

        let reviews = std::mem::take(&mut self.reviews);

        self.reviews = reviews
            .into_iter()
            .filter(|review| self.reviewed_as_recorded(review))
            .collect();
    }

    /// The format's rules beyond its shape.
    fn check(&self) -> Result<(), String> {
        let declaration = self.declaration.key();
        let kind = declaration.kind();

        if !bindable(declaration) {
            return Err(format!(
                "`{}` is not bindable, so it has no record",
                self.declaration
            ));
        }

        for (index, region) in self.regions.iter().enumerate() {
            check_path(&region.path, &format!("regions[{index}]"))?;

            let [begin, end] = region.lines;

            if begin == 0 || end <= begin {
                return Err(format!(
                    "regions[{index}]: `lines` are a region's begin and end marker lines, \
                     counted from 1, the begin first"
                ));
            }
        }

        let mut seen = BTreeSet::new();

        for (index, attested) in self.facts.iter().enumerate() {
            if !seen.insert(attested.fact.canonical()) {
                return Err(format!("facts[{index}] repeats an earlier fact"));
            }

            if attested.evidence.is_empty() {
                return Err(format!(
                    "facts[{index}] cites no evidence; an attested fact cites at least one span"
                ));
            }

            for (span_index, span) in attested.evidence.iter().enumerate() {
                let at = format!("facts[{index}].evidence[{span_index}]");

                check_path(&span.path, &at)?;

                let [first, last] = span.lines;

                if first == 0 || last < first {
                    return Err(format!(
                        "{at}: `lines` are a first and a last line, counted from 1"
                    ));
                }
            }
        }

        for (index, claim) in self.completeness.iter().enumerate() {
            let carriers: &[&str] = match claim.scope {
                ClaimScope::Writers => &["data_object"],
                ClaimScope::Publishers => &["topic", "outbox"],
            };

            if !carriers.contains(&kind) {
                return Err(format!(
                    "completeness[{index}]: a `{}` claim belongs on a {} record, not a \
                     `{kind}` one",
                    claim.scope,
                    carriers
                        .iter()
                        .map(|carrier| format!("`{carrier}`"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ));
            }

            if claim.patterns.is_empty() {
                return Err(format!("completeness[{index}] names no pattern"));
            }

            for (exception_index, exception) in claim.exceptions.iter().enumerate() {
                check_path(
                    &exception.path,
                    &format!("completeness[{index}].exceptions[{exception_index}]"),
                )?;
            }
        }

        check_party(&self.attested.by, "attested.by")?;

        for (index, review) in self.reviews.iter().enumerate() {
            check_party(&review.by, &format!("reviews[{index}].by"))?;

            for (item_index, item) in review.items.iter().enumerate() {
                let (named, count) = match item.target {
                    ReviewTarget::Fact(target) => ("facts", Some((target, self.facts.len()))),
                    ReviewTarget::Completeness(target) => {
                        ("completeness", Some((target, self.completeness.len())))
                    }
                    ReviewTarget::Structural => ("structural", None),
                };

                if let Some((target, count)) = count
                    && target >= count
                {
                    return Err(format!(
                        "reviews[{index}].items[{item_index}] names {named}[{target}], but the \
                         record has {count}"
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Refuses a path that is not a file relative to the project directory,
/// as a scan names one: `/`-separated, with no empty, `.`, or `..`
/// segment.
fn check_path(path: &str, at: &str) -> Result<(), String> {
    let relative = !path.is_empty()
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");

    if relative {
        Ok(())
    } else {
        Err(format!(
            "{at}: `{path}` is not a path relative to the project directory"
        ))
    }
}

fn check_party(party: &Party, at: &str) -> Result<(), String> {
    if party.name.trim().is_empty() {
        Err(format!("{at} has no name"))
    } else {
        Ok(())
    }
}

/// One record file, as read.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordFile {
    pub path: PathBuf,

    /// The digest of the file's bytes: what a write replacing it names
    /// (§8.4).
    pub digest: Digest,

    pub content: RecordContent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecordContent {
    Read(Box<Record>),

    /// A record written on another version axis, set aside unread. Its
    /// declaration reads as `unattested`, for this reason (§8.6).
    OtherVersion(VersionMismatch),
}

/// The version axis a record was written on that this build does not
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionMismatch {
    Format { found: u64 },
    Dsl { found: DslVersion },
    FactFormat { found: FactFormat },
}

impl fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format { found } => write!(
                f,
                "the record is conformance format {found}, but this build reads format {FORMAT}"
            ),

            Self::Dsl { found } => write!(
                f,
                "the record is dsl {found}, but this build reads dsl {DSL_VERSION}: the \
                 declarations it checked meant something else"
            ),

            Self::FactFormat { found } => write!(
                f,
                "the record is fact format {found}, but this build reads fact format \
                 {FACT_FORMAT}"
            ),
        }
    }
}

/// A project's conformance records.
#[derive(Debug, Clone)]
pub struct Records {
    /// The project's `.conseqa/` directory.
    root: PathBuf,
}

impl Records {
    /// The records of the project at `project_dir`.
    pub fn of_project(project_dir: &Path) -> Self {
        Self {
            root: project_dir.join(PROJECT_DIR),
        }
    }

    /// Where a declaration's record lives (§8.1): its kind as a
    /// directory, then each escaped id as a path segment, the last with
    /// `.yaml` appended.
    pub fn path(&self, key: &TextualKey) -> PathBuf {
        let declaration = key.key();
        let ids = declaration.ids().expect("a textual key names ids");
        let (last, containing) = ids.split_last().expect("a textual key names an id");

        let mut path = self.root.join(DIR).join(declaration.kind());

        for id in containing {
            path.push(id.encoded());
        }

        path.push(format!("{}.yaml", last.encoded()));

        path
    }

    /// Every record, strictly (§8.1).
    pub fn read_all(&self) -> Result<BTreeMap<TextualKey, RecordFile>, ConformanceError> {
        let dir = self.root.join(DIR);
        let mut records = BTreeMap::new();

        let Some(entries) = entries(&dir)? else {
            return Ok(records);
        };

        for (name, kind) in entries {
            let path = dir.join(&name);

            let arity = bindable_kinds()
                .find(|(bindable, _)| *bindable == name)
                .map(|(_, arity)| arity);

            match (kind, arity) {
                (EntryKind::File, _) if name == policy::FILE => {}

                (EntryKind::Directory, Some(arity)) => {
                    read_level(&name, arity, &path, &mut Vec::new(), &mut records)?;
                }

                _ => {
                    return Err(ConformanceError::Unexpected {
                        path,
                        expected: format!(
                            "it holds only `{}` and one directory per bindable kind: {}",
                            policy::FILE,
                            bindable_kinds()
                                .map(|(kind, _)| format!("{kind}/"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    });
                }
            }
        }

        Ok(records)
    }

    /// One declaration's record, if it has one.
    pub fn read(&self, key: &TextualKey) -> Result<Option<RecordFile>, ConformanceError> {
        let path = self.path(key);

        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => read_record(&path, key).map(Some),

            Ok(_) => Err(ConformanceError::Unexpected {
                path,
                expected: "a record is a file".to_string(),
            }),

            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),

            Err(error) => Err(ConformanceError::Io { path, error }),
        }
    }

    /// Writes a record in place of the one with digest `replaces`, or
    /// where there is none when `replaces` is `None`, and returns the new
    /// file's digest (§8.4). Refused if the file on disk is not the one
    /// named, or if the record breaks a rule its reader would refuse it
    /// for.
    pub fn write(
        &self,
        record: &Record,
        replaces: Option<Digest>,
    ) -> Result<Digest, ConformanceError> {
        let path = self.path(&record.declaration);

        let refused = |message: String| ConformanceError::Refused {
            path: path.clone(),
            message,
        };

        if (record.format, record.dsl, record.fact_format) != (FORMAT, DSL_VERSION, FACT_FORMAT) {
            return Err(refused(format!(
                "a record is written at this build's versions: conformance format {FORMAT}, \
                 dsl {DSL_VERSION}, fact format {FACT_FORMAT}"
            )));
        }

        record.check().map_err(refused)?;

        let text = to_yaml(record).map_err(refused)?;

        let _lock = self.lock()?;

        let found = match fs::read(&path) {
            Ok(bytes) => Some(Digest::of(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(ConformanceError::Io { path, error }),
        };

        if found != replaces {
            return Err(ConformanceError::Changed {
                path,
                expected: replaces,
                found,
            });
        }

        write_atomic(&path, text.as_bytes())?;

        Ok(Digest::of(text.as_bytes()))
    }

    /// Takes the record lock, waiting for another writer to finish.
    fn lock(&self) -> Result<File, ConformanceError> {
        let path = self.root.join(LOCK);

        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(ConformanceError::io(dir))?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(ConformanceError::io(&path))?;

        file.lock().map_err(ConformanceError::io(&path))?;

        Ok(file)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,

    /// A symbolic link, or anything else no reader opens.
    Other,
}

/// A directory's entries, sorted by name, dotfiles left out: they are
/// an editor's or the OS's, and pass (§8.1). `None` when there is no
/// directory.
fn entries(dir: &Path) -> Result<Option<Vec<(String, EntryKind)>>, ConformanceError> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ConformanceError::Io {
                path: dir.to_path_buf(),
                error,
            });
        }
    };

    let mut entries = Vec::new();

    for entry in read {
        let entry = entry.map_err(ConformanceError::io(dir))?;

        let name = entry.file_name().to_string_lossy().into_owned();

        if name.starts_with('.') {
            continue;
        }

        let kind = entry
            .file_type()
            .map_err(ConformanceError::io(entry.path()))?;

        entries.push((
            name,
            if kind.is_dir() {
                EntryKind::Directory
            } else if kind.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            },
        ));
    }

    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    Ok(Some(entries))
}

/// Reads one level of a kind's directory: directories named by the
/// containing declarations' ids, then `<id>.yaml` records.
fn read_level(
    kind: &str,
    arity: usize,
    dir: &Path,
    ids: &mut Vec<Id>,
    records: &mut BTreeMap<TextualKey, RecordFile>,
) -> Result<(), ConformanceError> {
    let last = ids.len() + 1 == arity;

    for (name, entry) in entries(dir)?.unwrap_or_default() {
        let path = dir.join(&name);

        let unexpected = || ConformanceError::Unexpected {
            path: path.clone(),
            expected: if last {
                "a `{kind}` record is a `<id>.yaml` file, named by the id's canonical encoding"
                    .replace("{kind}", kind)
            } else {
                "a directory of `{kind}` records is named by the containing declaration's \
                 id, in its canonical encoding"
                    .replace("{kind}", kind)
            },
        };

        if last {
            let id = (entry == EntryKind::File)
                .then(|| name.strip_suffix(".yaml").and_then(Id::decode))
                .flatten()
                .ok_or_else(unexpected)?;

            ids.push(id);

            let key: TextualKey = format!(
                "{kind} {}",
                ids.iter().map(Id::encoded).collect::<Vec<_>>().join("/")
            )
            .parse()
            .expect("a bindable kind and its ids in canonical encoding are a key");

            ids.pop();

            let record = read_record(&path, &key)?;

            records.insert(key, record);
        } else {
            let id = (entry == EntryKind::Directory)
                .then(|| Id::decode(&name))
                .flatten()
                .ok_or_else(unexpected)?;

            ids.push(id);
            read_level(kind, arity, &path, ids, records)?;
            ids.pop();
        }
    }

    Ok(())
}

/// Reads the record of `key` at `path`.
fn read_record(path: &Path, key: &TextualKey) -> Result<RecordFile, ConformanceError> {
    let bytes = fs::read(path).map_err(ConformanceError::io(path))?;
    let digest = Digest::of(&bytes);

    let content = String::from_utf8(bytes)
        .map_err(|_| "the file is not UTF-8".to_string())
        .and_then(|text| parse(&text, key))
        .map_err(|message| ConformanceError::Parse {
            path: path.to_path_buf(),
            message,
        })?;

    Ok(RecordFile {
        path: path.to_path_buf(),
        digest,
        content,
    })
}

/// Parses a record's text in two phases: its version axes, leniently,
/// so that a record of another version is set aside rather than refused
/// for a field it does not share with this build's format; then the
/// record, strictly.
fn parse(text: &str, key: &TextualKey) -> Result<RecordContent, String> {
    let probe: Value = serde_yaml::from_str(text).map_err(|error| error.to_string())?;

    if !probe.is_object() {
        return Err("a record is a mapping".to_string());
    }

    match probe.get("format") {
        None => return Err("missing `format`".to_string()),

        Some(format) => match format.as_u64() {
            Some(found) if found == u64::from(FORMAT) => {}
            Some(found) => {
                return Ok(RecordContent::OtherVersion(VersionMismatch::Format {
                    found,
                }));
            }
            None => return Err("`format` is not a number".to_string()),
        },
    }

    match check_versions(&probe) {
        Ok(()) => {}

        Err(FactVersionError::DslMismatch { found }) => {
            return Ok(RecordContent::OtherVersion(VersionMismatch::Dsl { found }));
        }

        Err(FactVersionError::FactFormatMismatch { found }) => {
            return Ok(RecordContent::OtherVersion(VersionMismatch::FactFormat {
                found,
            }));
        }

        Err(error @ (FactVersionError::DslMissing | FactVersionError::FactFormatMissing)) => {
            return Err(error.to_string());
        }
    }

    let record: Record = serde_yaml::from_str(text).map_err(|error| error.to_string())?;

    if record.declaration != *key {
        return Err(format!(
            "it declares `{}`, but its path names `{key}`",
            record.declaration
        ));
    }

    record.check()?;

    Ok(RecordContent::Read(Box::new(record)))
}

/// A record's canonical text: its fields in the format's order, with
/// every enum a fact holds as a one-key map, as the store writes the
/// spec.
fn to_yaml(record: &Record) -> Result<String, String> {
    let mut buffer = Vec::new();
    let mut serializer = serde_yaml::Serializer::new(&mut buffer);

    serde_yaml::with::singleton_map_recursive::serialize(record, &mut serializer)
        .map_err(|error| error.to_string())?;

    String::from_utf8(buffer).map_err(|error| error.to_string())
}

/// Writes through a temporary sibling and a rename, so a reader sees
/// the old record or the new one, never part of either. The temporary
/// is a dotfile, which the reader passes over.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), ConformanceError> {
    let dir = path.parent().unwrap_or(Path::new("."));

    fs::create_dir_all(dir).map_err(ConformanceError::io(dir))?;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let temporary = dir.join(format!(".{name}.tmp-{}", std::process::id()));

    fs::write(&temporary, contents).map_err(ConformanceError::io(&temporary))?;

    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);

        ConformanceError::Io {
            path: path.to_path_buf(),
            error,
        }
    })
}
