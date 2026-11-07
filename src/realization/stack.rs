//! The stack profile (§9 of the realization profiles revision): the
//! technologies one project uses, each at a version.
//!
//! The stack is a project-wide architectural decision, committed at
//! `.conseqa/realization/stack.yaml` and changed through the spec's
//! pull request like any other. Its presence is what turns realization
//! on: without it, nothing of the revision applies (§9).
//!
//! Every technology names its version, because entries are true of
//! version ranges: PostgreSQL before 9.1 called snapshot isolation
//! `SERIALIZABLE`. A version is dotted numbers, compared part by part
//! with missing parts read as zero, so `9.1` and `9.1.0` are one
//! version; a range is one or more comparisons, all of which must hold.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::catalogue::standard_name;
use super::{Files, RealizationError};

/// The stack profile's file name, in the realization directory.
pub const FILE: &str = "stack.yaml";

/// A version: dotted numbers, as written.
#[derive(Debug, Clone)]
pub struct Version {
    parts: Vec<u64>,
    text: String,
}

impl Version {
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Option<Vec<u64>> = text
            .split('.')
            .map(|part| {
                (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| part.parse().ok())
                    .flatten()
            })
            .collect();

        parts
            .map(|parts| Self {
                parts,
                text: text.to_string(),
            })
            .ok_or_else(|| {
                format!("`{text}` is not a version: write dotted numbers, such as `16` or `3.7.1`")
            })
    }

    fn part(&self, index: usize) -> u64 {
        self.parts.get(index).copied().unwrap_or(0)
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (0..self.parts.len().max(other.parts.len()))
            .map(|index| self.part(index).cmp(&other.part(index)))
            .find(|ordering| ordering.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    AtLeast,
    Above,
    AtMost,
    Below,
    Exactly,
}

impl Operator {
    const SPELLINGS: [(&'static str, Self); 5] = [
        (">=", Self::AtLeast),
        ("<=", Self::AtMost),
        (">", Self::Above),
        ("<", Self::Below),
        ("=", Self::Exactly),
    ];

    fn spelling(self) -> &'static str {
        Self::SPELLINGS
            .iter()
            .find(|(_, operator)| *operator == self)
            .map(|(spelling, _)| *spelling)
            .expect("every operator has a spelling")
    }

    fn holds(self, version: &Version, bound: &Version) -> bool {
        match self {
            Self::AtLeast => version >= bound,
            Self::Above => version > bound,
            Self::AtMost => version <= bound,
            Self::Below => version < bound,
            Self::Exactly => version == bound,
        }
    }
}

/// The versions an entry is true of: comparisons that must all hold,
/// written comma-separated, such as `>=9.1, <13`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRange(Vec<(Operator, Version)>);

impl VersionRange {
    pub fn contains(&self, version: &Version) -> bool {
        self.0
            .iter()
            .all(|(operator, bound)| operator.holds(version, bound))
    }
}

impl FromStr for VersionRange {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut comparisons = Vec::new();

        for comparison in text.split(',') {
            let comparison = comparison.trim();

            let Some((spelling, operator)) = Operator::SPELLINGS
                .iter()
                .find(|(spelling, _)| comparison.starts_with(spelling))
            else {
                return Err(format!(
                    "`{comparison}` is not a comparison: write `>=`, `>`, `<=`, `<`, or `=` \
                     and a version, such as `>=9.1`"
                ));
            };

            let version = Version::parse(comparison[spelling.len()..].trim())?;

            comparisons.push((*operator, version));
        }

        Ok(Self(comparisons))
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let comparisons: Vec<String> = self
            .0
            .iter()
            .map(|(operator, version)| format!("{}{version}", operator.spelling()))
            .collect();

        f.write_str(&comparisons.join(", "))
    }
}

impl Serialize for VersionRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VersionRange {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(RangeVisitor)
    }
}

struct RangeVisitor;

impl Visitor<'_> for RangeVisitor {
    type Value = VersionRange;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a version range, such as `>=9.1` or `>=2.8, <4`")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
        text.parse().map_err(E::custom)
    }
}

/// Whether a name is a technology's: lowercase letters, digits, `-` and
/// `_`, starting with a letter or a digit.
pub fn technology_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

/// One project's stack profile.
#[derive(Debug, Clone)]
pub struct Stack {
    /// Each technology the project uses, at its version.
    pub technologies: BTreeMap<String, Version>,
}

/// The stack profile as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StackFile {
    #[allow(dead_code)]
    format: u32,
    catalogue: String,
    technologies: Vec<String>,
}

impl Stack {
    /// The stack profile `files` holds, or `None` when they hold none.
    pub(crate) fn read_from(files: &Files<'_>) -> Result<Option<Self>, RealizationError> {
        let path = files.locate(FILE);

        let Some(text) = files.read_versioned(FILE)? else {
            return Ok(None);
        };

        let file: StackFile = serde_yaml::from_str(&text)
            .map_err(|error| RealizationError::parse(&path, error.to_string()))?;

        if file.catalogue != standard_name() {
            return Err(RealizationError::CatalogueMismatch {
                path,
                found: file.catalogue,
            });
        }

        let mut technologies = BTreeMap::new();

        for named in &file.technologies {
            let Some((name, version)) = named.split_once('@') else {
                return Err(RealizationError::parse(
                    &path,
                    format!(
                        "`{named}` names no version; write it as `{named}@<version>`, since \
                         entries are true of version ranges"
                    ),
                ));
            };

            if !technology_name(name) {
                return Err(RealizationError::parse(
                    &path,
                    format!(
                        "`{name}` is not a technology name: lowercase letters, digits, `-` and \
                         `_`"
                    ),
                ));
            }

            let version = Version::parse(version)
                .map_err(|message| RealizationError::parse(&path, message))?;

            if technologies.insert(name.to_string(), version).is_some() {
                return Err(RealizationError::parse(
                    &path,
                    format!("`{name}` is named twice; a stack names each technology once"),
                ));
            }
        }

        Ok(Some(Self { technologies }))
    }

    /// The version the stack names a technology at, if it names it.
    pub fn version(&self, technology: &str) -> Option<&Version> {
        self.technologies.get(technology)
    }

    /// Each technology as the stack writes it, `name@version`.
    pub fn named(&self) -> Vec<String> {
        self.technologies
            .iter()
            .map(|(name, version)| format!("{name}@{version}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_part_by_part_with_missing_parts_as_zero() {
        assert!(version("9.1") > version("9.0.24"));
        assert!(version("10") > version("9.6"));
        assert_eq!(version("9.1"), version("9.1.0"));
        assert!(version("3.10") > version("3.9"));

        for text in ["", "9.", ".1", "9.x", "v9", "9.1-rc1"] {
            assert!(Version::parse(text).is_err(), "`{text}` is refused");
        }
    }

    #[test]
    fn a_range_holds_only_where_every_comparison_holds() {
        let range: VersionRange = ">=9.1, <13".parse().unwrap();

        assert!(range.contains(&version("9.1")));
        assert!(range.contains(&version("12.4")));
        assert!(!range.contains(&version("9.0")));
        assert!(!range.contains(&version("13")));
        assert_eq!(range.to_string(), ">=9.1, <13");

        let exact: VersionRange = "=2.6".parse().unwrap();
        assert!(exact.contains(&version("2.6.0")));
        assert!(!exact.contains(&version("2.6.1")));

        for text in ["", "9.1", "~9.1", ">=", ">=9.1,"] {
            assert!(text.parse::<VersionRange>().is_err(), "`{text}` is refused");
        }
    }
}
