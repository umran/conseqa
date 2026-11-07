//! Catalogue entries, and the catalogues that hold them (§7, §8 of the
//! realization profiles revision).
//!
//! An entry states, once and with citations, what one technology,
//! configured a given way, provides in the capability vocabulary, what
//! it cannot provide and why, what it requires of the realized
//! declaration and its neighbours, where the evidence of each
//! requirement is found, and which deterministic checks can confirm
//! some of them. Every project and every agent session then reads the
//! same judgement, instead of rediscovering it by taste (§2.1).
//!
//! A **platform entry** realizes one realizable kind. An **idiom entry**
//! shows how to write one primitive with one framework on some
//! platforms, and provides nothing itself (§7.4).
//!
//! Two catalogues hold entries (§8.1): the **standard catalogue**,
//! embedded in the binary from `catalogue/` at the repository root and
//! versioned as a whole, and the **project catalogue**, committed at
//! `.conseqa/realization/catalogue/<id>.yaml`. An id is unique across
//! both: a project that disagrees with a standard entry copies it under
//! an id of its own and says why, and never silently overrides it.
//!
//! The reader is as strict as the store's and the conformance reader
//! (§8.2): unknown fields, capabilities, kinds, options, relations, and
//! requirement ids are refused by name, as is a checker assertion
//! outside its kind's shape. Every claim may carry `cite:`, the vendor
//! documentation it rests on; the standard catalogue's do, and a test
//! holds it to that (§8.3). Checkers are read here and run nowhere yet.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::design::store::ProjectDirectory;

use super::capability::{Capability, RealizableKind};
use super::stack::{VersionRange, technology_name};
use super::{EntryKind, Files, RealizationError, check_format};

/// The standard catalogue's name, as a stack names it with its version.
pub const STANDARD: &str = "conseqa-standard";

/// The standard catalogue's version, bumped with every change to any of
/// its entries (§8.1).
pub const STANDARD_VERSION: u32 = 1;

/// The standard catalogue as a stack names it: `conseqa-standard@<n>`.
pub fn standard_name() -> String {
    format!("{STANDARD}@{STANDARD_VERSION}")
}

/// The standard catalogue's entries, embedded at build time: each
/// file's name in `catalogue/`, which is its entry's id, and its text.
const STANDARD_ENTRIES: [(&str, &str); 12] = [
    (
        "debezium.outbox-relay",
        include_str!("../../catalogue/debezium.outbox-relay.yaml"),
    ),
    (
        "dynamodb.table",
        include_str!("../../catalogue/dynamodb.table.yaml"),
    ),
    (
        "envoy.ring-hash",
        include_str!("../../catalogue/envoy.ring-hash.yaml"),
    ),
    (
        "envoy.round-robin",
        include_str!("../../catalogue/envoy.round-robin.yaml"),
    ),
    (
        "kafka.consumer-group",
        include_str!("../../catalogue/kafka.consumer-group.yaml"),
    ),
    (
        "kafka.keyed-topic",
        include_str!("../../catalogue/kafka.keyed-topic.yaml"),
    ),
    (
        "kafka.topic",
        include_str!("../../catalogue/kafka.topic.yaml"),
    ),
    (
        "oracle.database",
        include_str!("../../catalogue/oracle.database.yaml"),
    ),
    (
        "postgres.database",
        include_str!("../../catalogue/postgres.database.yaml"),
    ),
    (
        "postgres.partitioned-table",
        include_str!("../../catalogue/postgres.partitioned-table.yaml"),
    ),
    ("sqs.fifo", include_str!("../../catalogue/sqs.fifo.yaml")),
    (
        "sqs.standard",
        include_str!("../../catalogue/sqs.standard.yaml"),
    ),
];

/// The names of the standard catalogue's files, for a test to compare
/// with the directory.
pub fn standard_files() -> impl Iterator<Item = &'static str> {
    STANDARD_ENTRIES.iter().map(|(name, _)| *name)
}

/// The project catalogue's directory, in the realization directory.
pub const DIR: &str = "catalogue";

/// The most options an entry may define. Choice enumerates every set of
/// a candidate's options, so the bound keeps a catalogue from making
/// every derivation slow.
pub const MAX_OPTIONS: usize = 16;

/// Whether a name is a plain one, as option, requirement, and checker
/// ids are: lowercase letters, digits, `-` and `_`, starting with a
/// letter or a digit.
fn plain(name: &str) -> bool {
    technology_name(name)
}

/// Whether an id is an entry's: plain names joined by `.`, such as
/// `kafka.keyed-topic`.
pub fn entry_id(id: &str) -> bool {
    !id.is_empty() && id.split('.').all(plain)
}

/// An entry at a version, written `id@version`: what a choice names,
/// and what a realization fact and a record name as the entry they were
/// checked against (§12, §14). The durable form of facts defines it.
pub use crate::analyzer::basis::EntryRef;

/// The entry at a version `text` names, if it is an entry's id and a
/// version counted from 1.
pub fn entry_ref(text: &str) -> Option<EntryRef> {
    EntryRef::parse(text).filter(|entry| entry_id(&entry.id) && entry.version > 0)
}

/// The product an entry describes, and the versions it is true of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Technology {
    pub name: String,
    pub versions: VersionRange,
}

/// The one kind a platform entry realizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Realizes {
    pub kind: RealizableKind,
}

/// An option a claim or requirement holds under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionRef {
    pub option: String,
}

/// A capability an entry provides, optionally only with an option.
///
/// Written bare, `- grouping: keyed`, or long, with `capability:` and
/// then `with:` or `cite:`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Provision {
    pub capability: Capability,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub with: Option<OptionRef>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cite: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LongProvision {
    capability: Capability,

    #[serde(default)]
    with: Option<OptionRef>,

    #[serde(default)]
    cite: Vec<String>,
}

impl<'de> Deserialize<'de> for Provision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_yaml::Value::deserialize(deserializer)?;

        let long = matches!(&value, serde_yaml::Value::Mapping(mapping) if mapping.contains_key("capability"));

        if long {
            let long: LongProvision = serde_yaml::from_value(value).map_err(de::Error::custom)?;

            Ok(Self {
                capability: long.capability,
                with: long.with,
                cite: long.cite,
            })
        } else {
            let capability: Capability =
                serde_yaml::from_value(value).map_err(de::Error::custom)?;

            Ok(Self {
                capability,
                with: None,
                cite: Vec::new(),
            })
        }
    }
}

/// A capability the technology does not provide, and why: what turns
/// "not listed" into an explanation (§7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    pub capability: Capability,

    /// The option that lifts the exclusion, if one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<OptionRef>,

    pub reason: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cite: Vec<String>,
}

/// A named configuration choice. Choosing it adds what it provides and
/// what it requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionSpec {
    pub description: String,

    /// The options it cannot be chosen with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
}

/// Who a realization requirement applies to (§11.2): the realized
/// declaration's own regions, or every declaration in one neighbour
/// relation of it, which a graph query of the design layer answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    #[serde(rename = "self")]
    Itself,
    Producers,
    Consumers,
    Callers,
    Writers,
    Transactions,
    SerializableTransactions,
    GuardedTransactions,
}

impl Relation {
    const ALL: [Self; 8] = [
        Self::Itself,
        Self::Producers,
        Self::Consumers,
        Self::Callers,
        Self::Writers,
        Self::Transactions,
        Self::SerializableTransactions,
        Self::GuardedTransactions,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Itself => "self",
            Self::Producers => "producers",
            Self::Consumers => "consumers",
            Self::Callers => "callers",
            Self::Writers => "writers",
            Self::Transactions => "transactions",
            Self::SerializableTransactions => "serializable_transactions",
            Self::GuardedTransactions => "guarded_transactions",
        }
    }

    /// Whether a declaration of the kind has neighbours in this
    /// relation (§11.2).
    pub fn of(self, kind: RealizableKind) -> bool {
        use RealizableKind as Kind;

        match self {
            Self::Itself => true,
            Self::Producers => matches!(
                kind,
                Kind::Topic | Kind::TopicRuntime | Kind::Outbox | Kind::OutboxRuntime
            ),
            Self::Consumers => matches!(kind, Kind::Topic | Kind::TopicRuntime),
            Self::Callers => kind == Kind::Router,
            Self::Writers
            | Self::Transactions
            | Self::SerializableTransactions
            | Self::GuardedTransactions => kind == Kind::DataModel,
        }
    }

    /// The relations a declaration of the kind has.
    pub fn of_kind(kind: RealizableKind) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|relation| relation.of(kind))
            .collect()
    }
}

impl fmt::Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A realization requirement (§11.1): configuration the entry demands,
/// stated in a sentence an implementing agent can act on and a reviewer
/// can check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub of: Relation,
    pub statement: String,

    /// The option that adds it, if it is an option's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<OptionRef>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cite: Vec<String>,
}

/// Where, in a region, the evidence for a requirement is found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub look_for: String,
}

/// The checker kinds Conseqa ships (§13.1). Entries configure them;
/// they never run code of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckerKind {
    Hcl,
    YamlPath,
    Properties,
    SqlDdl,
    Pattern,
}

/// A configuration value a checker compares with: a string, a boolean,
/// or an integer, compared as the text it is written as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scalar(pub String);

impl Serialize for Scalar {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ScalarVisitor)
    }
}

struct ScalarVisitor;

impl Visitor<'_> for ScalarVisitor {
    type Value = Scalar;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string, a boolean, or an integer")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
        Ok(Scalar(text.to_string()))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Scalar(value.to_string()))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Scalar(value.to_string()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Scalar(value.to_string()))
    }
}

/// An `hcl` assertion: a resource of a type exists, or an attribute of
/// it equals or contains a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HclAssert {
    pub resource: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Scalar>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<Scalar>,
}

/// A `yaml_path` assertion: a path exists, or its value equals one
/// value or is one of several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct YamlPathAssert {
    pub path: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Scalar>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<Scalar>>,
}

/// A `properties` assertion: a key equals a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropertiesAssert {
    pub key: String,
    pub equals: Scalar,
}

/// A `sql_ddl` assertion: a table has a primary key or a unique
/// constraint over columns, is partitioned by columns, or has a
/// trigger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlDdlAssert {
    pub table: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<bool>,
}

/// A `pattern` assertion: a regular expression matches. The weakest
/// kind, for idioms only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternAssert {
    pub regex: String,
}

/// What a checker asserts, in its kind's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Assertion {
    Hcl(HclAssert),
    YamlPath(YamlPathAssert),
    Properties(PropertiesAssert),
    SqlDdl(SqlDdlAssert),
    Pattern(PatternAssert),
}

impl Assertion {
    /// Reads an assertion in the shape of a checker kind, refusing one
    /// that asserts nothing or several things at once.
    fn of(kind: CheckerKind, value: serde_yaml::Value) -> Result<Self, String> {
        let read = |error: serde_yaml::Error| {
            format!(
                "its `assert` is not a `{}` assertion: {error}",
                kind_name(kind)
            )
        };

        let one = |count: usize, shapes: &str| {
            if count == 1 {
                Ok(())
            } else {
                Err(format!("its `assert` asserts exactly one of {shapes}"))
            }
        };

        match kind {
            CheckerKind::Hcl => {
                let assert: HclAssert = serde_yaml::from_value(value).map_err(read)?;

                one(
                    usize::from(assert.present.is_some())
                        + usize::from(assert.equals.is_some())
                        + usize::from(assert.contains.is_some()),
                    "`present`, `equals`, or `contains`",
                )?;

                if assert.present == Some(false) {
                    return Err(
                        "its `assert` says `present: false`; assert what exists".to_string()
                    );
                }

                if assert.present.is_some() != assert.attribute.is_none() {
                    return Err(
                        "its `assert` names an `attribute` exactly when it compares one with \
                         `equals` or `contains`"
                            .to_string(),
                    );
                }

                Ok(Self::Hcl(assert))
            }

            CheckerKind::YamlPath => {
                let assert: YamlPathAssert = serde_yaml::from_value(value).map_err(read)?;

                one(
                    usize::from(assert.exists.is_some())
                        + usize::from(assert.equals.is_some())
                        + usize::from(assert.one_of.is_some()),
                    "`exists`, `equals`, or `one_of`",
                )?;

                if assert.exists == Some(false) {
                    return Err("its `assert` says `exists: false`; assert what exists".to_string());
                }

                if assert.one_of.as_ref().is_some_and(Vec::is_empty) {
                    return Err("its `assert` has an empty `one_of`".to_string());
                }

                Ok(Self::YamlPath(assert))
            }

            CheckerKind::Properties => serde_yaml::from_value(value)
                .map(Self::Properties)
                .map_err(read),

            CheckerKind::SqlDdl => {
                let assert: SqlDdlAssert = serde_yaml::from_value(value).map_err(read)?;

                one(
                    usize::from(assert.primary_key.is_some())
                        + usize::from(assert.unique.is_some())
                        + usize::from(assert.partition_by.is_some())
                        + usize::from(assert.trigger.is_some()),
                    "`primary_key`, `unique`, `partition_by`, or `trigger`",
                )?;

                if [&assert.primary_key, &assert.unique, &assert.partition_by]
                    .into_iter()
                    .flatten()
                    .any(Vec::is_empty)
                {
                    return Err("its `assert` names no columns".to_string());
                }

                if assert.trigger == Some(false) {
                    return Err(
                        "its `assert` says `trigger: false`; assert what exists".to_string()
                    );
                }

                Ok(Self::SqlDdl(assert))
            }

            CheckerKind::Pattern => {
                let assert: PatternAssert = serde_yaml::from_value(value).map_err(read)?;

                regex::Regex::new(&assert.regex)
                    .map_err(|error| format!("its `regex` does not compile: {error}"))?;

                Ok(Self::Pattern(assert))
            }
        }
    }
}

fn kind_name(kind: CheckerKind) -> &'static str {
    match kind {
        CheckerKind::Hcl => "hcl",
        CheckerKind::YamlPath => "yaml_path",
        CheckerKind::Properties => "properties",
        CheckerKind::SqlDdl => "sql_ddl",
        CheckerKind::Pattern => "pattern",
    }
}

/// A deterministic check of some of a platform entry's requirements
/// (§13). It reads only the regions bound to the declaration its
/// requirement applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CheckerFile")]
pub struct Checker {
    pub id: String,
    pub kind: CheckerKind,

    /// The requirements it confirms or disputes.
    pub covers: Vec<String>,

    pub assert: Assertion,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckerFile {
    id: String,
    kind: CheckerKind,
    covers: Vec<String>,
    assert: serde_yaml::Value,
}

impl TryFrom<CheckerFile> for Checker {
    type Error = String;

    fn try_from(file: CheckerFile) -> Result<Self, Self::Error> {
        let assert = Assertion::of(file.kind, file.assert)
            .map_err(|message| format!("checker `{}`: {message}", file.id))?;

        Ok(Self {
            id: file.id,
            kind: file.kind,
            covers: file.covers,
            assert,
        })
    }
}

/// An idiom entry's checker: a check of the idiom's evidence shape. An
/// idiom states no requirements, so it covers none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IdiomCheckerFile")]
pub struct IdiomChecker {
    pub id: String,
    pub kind: CheckerKind,
    pub assert: Assertion,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdiomCheckerFile {
    id: String,
    kind: CheckerKind,
    assert: serde_yaml::Value,
}

impl TryFrom<IdiomCheckerFile> for IdiomChecker {
    type Error = String;

    fn try_from(file: IdiomCheckerFile) -> Result<Self, Self::Error> {
        let assert = Assertion::of(file.kind, file.assert)
            .map_err(|message| format!("checker `{}`: {message}", file.id))?;

        Ok(Self {
            id: file.id,
            kind: file.kind,
            assert,
        })
    }
}

/// An entry that realizes a declaration (§7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformEntry {
    pub format: u32,
    pub id: String,
    pub version: u32,
    pub technology: Technology,
    pub realizes: Realizes,

    #[serde(default)]
    pub provides: Vec<Provision>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cannot_provide: Vec<Exclusion>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, OptionSpec>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<Requirement>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, Evidence>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkers: Vec<Checker>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<String>,
}

impl PlatformEntry {
    pub fn entry_ref(&self) -> EntryRef {
        EntryRef {
            id: self.id.clone(),
            version: self.version,
        }
    }

    pub fn kind(&self) -> RealizableKind {
        self.realizes.kind
    }

    /// What the entry provides with a set of its options: everything it
    /// provides unconditionally, and everything each option adds.
    pub fn provides_with(&self, options: &BTreeSet<String>) -> BTreeSet<Capability> {
        self.provides
            .iter()
            .filter(|provision| {
                provision
                    .with
                    .as_ref()
                    .is_none_or(|with| options.contains(&with.option))
            })
            .map(|provision| provision.capability)
            .collect()
    }

    /// The realization requirements a set of the entry's options puts
    /// in force (§11.1): its own, and those of the options chosen.
    pub fn requirements_with(&self, options: &BTreeSet<String>) -> Vec<&Requirement> {
        self.requirements
            .iter()
            .filter(|requirement| {
                requirement
                    .with
                    .as_ref()
                    .is_none_or(|with| options.contains(&with.option))
            })
            .collect()
    }

    /// Why the entry cannot provide a capability, if it says.
    pub fn exclusion(&self, capability: Capability) -> Option<&Exclusion> {
        self.cannot_provide
            .iter()
            .find(|exclusion| exclusion.capability == capability)
    }

    /// The option an option set breaks the exclusions of, with the one
    /// it excludes, if the set breaks any.
    pub fn conflict<'a>(&'a self, options: &BTreeSet<String>) -> Option<(&'a str, &'a str)> {
        self.options.iter().find_map(|(option, spec)| {
            if !options.contains(option) {
                return None;
            }

            spec.excludes
                .iter()
                .find(|excluded| options.contains(*excluded))
                .map(|excluded| (option.as_str(), excluded.as_str()))
        })
    }

    /// The rules of §8.2 that the field types alone do not hold.
    fn check(&self) -> Result<(), String> {
        check_identity(&self.id, self.version, &self.technology)?;

        if self.options.len() > MAX_OPTIONS {
            return Err(format!(
                "it defines {} options; an entry defines at most {MAX_OPTIONS}",
                self.options.len()
            ));
        }

        let defined = |option: &OptionRef, what: &str| -> Result<(), String> {
            if self.options.contains_key(&option.option) {
                Ok(())
            } else {
                Err(format!(
                    "{what} names the option `{}`, which the entry does not define",
                    option.option
                ))
            }
        };

        for (option, spec) in &self.options {
            if !plain(option) {
                return Err(format!(
                    "`{option}` is not an option name: lowercase letters, digits, `-` and `_`"
                ));
            }

            for excluded in &spec.excludes {
                if excluded == option {
                    return Err(format!("the option `{option}` excludes itself"));
                }

                if !self.options.contains_key(excluded) {
                    return Err(format!(
                        "the option `{option}` excludes `{excluded}`, which the entry does not \
                         define"
                    ));
                }
            }
        }

        let mut provided = BTreeSet::new();

        for provision in &self.provides {
            if let Some(with) = &provision.with {
                defined(with, &format!("`{}`", provision.capability))?;
            }

            if !provided.insert((
                provision.capability,
                provision.with.clone().map(|with| with.option),
            )) {
                return Err(format!("it provides `{}` twice", provision.capability));
            }
        }

        let mut excluded = BTreeSet::new();

        for exclusion in &self.cannot_provide {
            let capability = exclusion.capability;

            if !excluded.insert(capability) {
                return Err(format!(
                    "it says twice that it cannot provide `{capability}`"
                ));
            }

            if let Some(unless) = &exclusion.unless {
                defined(unless, &format!("the exclusion of `{capability}`"))?;
            }

            for provision in self
                .provides
                .iter()
                .filter(|provision| provision.capability == capability)
            {
                let lifted = match (&provision.with, &exclusion.unless) {
                    (Some(with), Some(unless)) => with.option == unless.option,
                    _ => false,
                };

                if !lifted {
                    return Err(match &provision.with {
                        None => format!("it provides `{capability}` and says it cannot"),
                        Some(with) => format!(
                            "it provides `{capability}` with the option `{}`, which its \
                             exclusion of `{capability}` does not name",
                            with.option
                        ),
                    });
                }
            }

            if let Some(unless) = &exclusion.unless
                && !self.provides.iter().any(|provision| {
                    provision.capability == capability && provision.with.as_ref() == Some(unless)
                })
            {
                return Err(format!(
                    "it cannot provide `{capability}` unless `{}`, but that option provides no \
                     `{capability}`",
                    unless.option
                ));
            }
        }

        let mut requirements = BTreeSet::new();

        for requirement in &self.requirements {
            if !plain(&requirement.id) {
                return Err(format!(
                    "`{}` is not a requirement id: lowercase letters, digits, `-` and `_`",
                    requirement.id
                ));
            }

            if !requirements.insert(requirement.id.as_str()) {
                return Err(format!(
                    "the requirement `{}` is defined twice",
                    requirement.id
                ));
            }

            if !requirement.of.of(self.kind()) {
                return Err(format!(
                    "the requirement `{}` applies to its `{}`, but a `{}` has none; its \
                     relations are {}",
                    requirement.id,
                    requirement.of,
                    self.kind(),
                    Relation::of_kind(self.kind())
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }

            if let Some(with) = &requirement.with {
                defined(with, &format!("the requirement `{}`", requirement.id))?;
            }
        }

        for requirement in self.evidence.keys() {
            if !requirements.contains(requirement.as_str()) {
                return Err(format!(
                    "`evidence` names the requirement `{requirement}`, which the entry does not \
                     define"
                ));
            }
        }

        let mut checkers = BTreeSet::new();

        for checker in &self.checkers {
            if !plain(&checker.id) || !checkers.insert(checker.id.as_str()) {
                return Err(format!(
                    "`{}` is not a checker id, or names two checkers",
                    checker.id
                ));
            }

            if checker.covers.is_empty() {
                return Err(format!(
                    "the checker `{}` covers no requirement",
                    checker.id
                ));
            }

            for covered in &checker.covers {
                if !requirements.contains(covered.as_str()) {
                    return Err(format!(
                        "the checker `{}` covers `{covered}`, which the entry does not define",
                        checker.id
                    ));
                }
            }
        }

        check_citations(
            self.provides
                .iter()
                .map(|provision| &provision.cite)
                .chain(self.cannot_provide.iter().map(|exclusion| &exclusion.cite))
                .chain(
                    self.requirements
                        .iter()
                        .map(|requirement| &requirement.cite),
                ),
            &self.citations,
        )
    }
}

/// An entry that shows how to write one primitive with one framework,
/// on the platforms it names (§7.4). It provides no capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdiomEntry {
    pub format: u32,
    pub id: String,
    pub version: u32,
    pub technology: Technology,

    /// The platform entries the idiom is written for.
    pub platforms: Vec<String>,

    pub primitive: Capability,
    pub template: String,
    pub evidence: Evidence,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkers: Vec<IdiomChecker>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<String>,
}

impl IdiomEntry {
    fn check(&self) -> Result<(), String> {
        check_identity(&self.id, self.version, &self.technology)?;

        if self.platforms.is_empty() {
            return Err("it names no platform".to_string());
        }

        let mut checkers = BTreeSet::new();

        for checker in &self.checkers {
            if !plain(&checker.id) || !checkers.insert(checker.id.as_str()) {
                return Err(format!(
                    "`{}` is not a checker id, or names two checkers",
                    checker.id
                ));
            }
        }

        check_citations(std::iter::empty(), &self.citations)
    }
}

fn check_identity(id: &str, version: u32, technology: &Technology) -> Result<(), String> {
    if !entry_id(id) {
        return Err(format!(
            "`{id}` is not an entry id: plain names joined by `.`, such as `kafka.keyed-topic`"
        ));
    }

    if version == 0 {
        return Err("`version` counts from 1".to_string());
    }

    if !technology_name(&technology.name) {
        return Err(format!(
            "`{}` is not a technology name: lowercase letters, digits, `-` and `_`",
            technology.name
        ));
    }

    Ok(())
}

/// Refuses an empty citation, in a claim's `cite` or the entry's list.
fn check_citations<'a>(
    cites: impl Iterator<Item = &'a Vec<String>>,
    citations: &'a [String],
) -> Result<(), String> {
    if cites
        .flatten()
        .chain(citations)
        .any(|citation| citation.trim().is_empty())
    {
        return Err("a citation is empty".to_string());
    }

    Ok(())
}

/// A catalogue entry of either kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Entry {
    Platform(PlatformEntry),
    Idiom(IdiomEntry),
}

impl Entry {
    /// Reads an entry's text strictly, its format first.
    pub fn parse(path: &Path, text: &str) -> Result<Self, RealizationError> {
        check_format(path, text)?;

        let probe: serde_yaml::Value = serde_yaml::from_str(text)
            .map_err(|error| RealizationError::parse(path, error.to_string()))?;

        let has = |key: &str| matches!(&probe, serde_yaml::Value::Mapping(mapping) if mapping.contains_key(key));

        let entry = if has("realizes") {
            serde_yaml::from_str(text).map(Self::Platform)
        } else if has("primitive") {
            serde_yaml::from_str(text).map(Self::Idiom)
        } else {
            return Err(RealizationError::parse(
                path,
                "an entry is a platform entry, with `realizes`, or an idiom entry, with \
                 `primitive`",
            ));
        };

        let entry = entry.map_err(|error| RealizationError::parse(path, error.to_string()))?;

        match &entry {
            Self::Platform(platform) => platform.check(),
            Self::Idiom(idiom) => idiom.check(),
        }
        .map_err(|message| RealizationError::parse(path, message))?;

        Ok(entry)
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Platform(entry) => &entry.id,
            Self::Idiom(entry) => &entry.id,
        }
    }

    pub fn version(&self) -> u32 {
        match self {
            Self::Platform(entry) => entry.version,
            Self::Idiom(entry) => entry.version,
        }
    }

    pub fn technology(&self) -> &Technology {
        match self {
            Self::Platform(entry) => &entry.technology,
            Self::Idiom(entry) => &entry.technology,
        }
    }

    pub fn entry_ref(&self) -> EntryRef {
        EntryRef {
            id: self.id().to_string(),
            version: self.version(),
        }
    }
}

/// Which catalogue holds an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Standard,
    Project,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Standard => "standard",
            Self::Project => "project",
        })
    }
}

/// An entry and the catalogue holding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Catalogued {
    pub source: Source,
    pub entry: Entry,
}

/// The entries a project can choose from: the standard catalogue's and
/// its own, by id.
#[derive(Debug, Clone)]
pub struct Catalogue {
    entries: BTreeMap<String, Catalogued>,
}

impl Catalogue {
    /// The standard catalogue alone.
    pub fn standard() -> Result<Self, RealizationError> {
        let mut entries = BTreeMap::new();

        for (name, text) in STANDARD_ENTRIES {
            let path = PathBuf::from(DIR).join(format!("{name}.yaml"));
            let entry = Entry::parse(&path, text)?;

            if entry.id() != name {
                return Err(RealizationError::parse(
                    &path,
                    format!("it is `{}`, but its file names `{name}`", entry.id()),
                ));
            }

            entries.insert(
                name.to_string(),
                Catalogued {
                    source: Source::Standard,
                    entry,
                },
            );
        }

        let catalogue = Self { entries };

        catalogue.check_platforms(&PathBuf::from(DIR))?;

        Ok(catalogue)
    }

    /// The standard catalogue and the project's own, strictly.
    pub fn read(project_dir: &Path) -> Result<Self, RealizationError> {
        Self::read_from(&Files::new(&ProjectDirectory::of_project(project_dir)))
    }

    /// The standard catalogue and the project entries `files` hold.
    pub(crate) fn read_from(files: &Files<'_>) -> Result<Self, RealizationError> {
        let mut catalogue = Self::standard()?;

        for (name, kind) in files.entries(DIR)?.unwrap_or_default() {
            let file = format!("{DIR}/{name}");
            let path = files.locate(&file);

            let id = (kind == EntryKind::File)
                .then(|| name.strip_suffix(".yaml"))
                .flatten()
                .filter(|id| entry_id(id))
                .ok_or_else(|| RealizationError::Unexpected {
                    path: path.clone(),
                    expected: "the project catalogue holds `<entry id>.yaml` files".to_string(),
                })?;

            let text = files.read(&file)?.unwrap_or_default();
            let entry = Entry::parse(&path, &text)?;

            if entry.id() != id {
                return Err(RealizationError::parse(
                    &path,
                    format!("it is `{}`, but its file names `{id}`", entry.id()),
                ));
            }

            if catalogue.entries.contains_key(id) {
                return Err(RealizationError::DuplicateEntry {
                    path,
                    id: id.to_string(),
                });
            }

            catalogue.entries.insert(
                id.to_string(),
                Catalogued {
                    source: Source::Project,
                    entry,
                },
            );
        }

        catalogue.check_platforms(&files.locate(DIR))?;

        Ok(catalogue)
    }

    /// Refuses an idiom naming a platform no catalogue holds.
    fn check_platforms(&self, dir: &Path) -> Result<(), RealizationError> {
        for catalogued in self.entries.values() {
            let Entry::Idiom(idiom) = &catalogued.entry else {
                continue;
            };

            for platform in &idiom.platforms {
                let known = matches!(
                    self.entries
                        .get(platform)
                        .map(|catalogued| &catalogued.entry),
                    Some(Entry::Platform(_))
                );

                if !known {
                    return Err(RealizationError::parse(
                        &dir.join(format!("{}.yaml", idiom.id)),
                        format!("it names the platform `{platform}`, which no catalogue holds"),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Every entry, by id.
    pub fn entries(&self) -> impl Iterator<Item = &Catalogued> {
        self.entries.values()
    }

    pub fn get(&self, id: &str) -> Option<&Catalogued> {
        self.entries.get(id)
    }

    /// The platform entry an id names, if there is one.
    pub fn platform(&self, id: &str) -> Option<&PlatformEntry> {
        match self.entries.get(id).map(|catalogued| &catalogued.entry) {
            Some(Entry::Platform(entry)) => Some(entry),
            _ => None,
        }
    }

    /// Every platform entry realizing a kind, by id, with its source.
    pub fn platforms(
        &self,
        kind: RealizableKind,
    ) -> impl Iterator<Item = (&PlatformEntry, Source)> {
        self.entries
            .values()
            .filter_map(move |catalogued| match &catalogued.entry {
                Entry::Platform(entry) if entry.kind() == kind => Some((entry, catalogued.source)),
                _ => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_catalogue_loads() {
        let catalogue = Catalogue::standard().unwrap();

        assert_eq!(catalogue.entries().count(), STANDARD_ENTRIES.len());
    }
}
