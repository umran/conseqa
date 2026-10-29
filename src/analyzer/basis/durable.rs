//! The durable form of facts: one canonical spelling per fact, read
//! strictly, on version axes a reader probes first (§10.9 of the spec
//! change analysis revision).
//!
//! A conformance record may be written by one build and read by another
//! months later, so a fact is held to the standard of the spec files.
//! Serialization emits the canonical form, and a reader accepts nothing
//! else: a shorthand, an unsorted set, an unknown kind, or an unknown
//! field is refused by name. Strictness is what makes a fact's identity
//! its canonical form; were two spellings accepted, "the same fact"
//! would depend on which one a writer chose.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spec::{DSL_READS, DSL_VERSION, DslVersion};

use super::{FACT_FORMAT, FACT_READS, FactFormat};

/// Why a value was refused as a canonical durable form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    /// The value does not parse: an unknown kind or field, a missing
    /// field, a value of the wrong type. The message names it.
    Invalid { message: String },

    /// The value carries a field the canonical form does not.
    UnknownField { path: String },

    /// The value parses, but the part at `path` is not spelled
    /// canonically: a shorthand, an unsorted or repeated set member, or
    /// an omitted field.
    NotCanonical { path: String },
}

impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { message } => f.write_str(message),

            Self::UnknownField { path } => write!(f, "unknown field `{path}`"),

            Self::NotCanonical { path } => write!(
                f,
                "`{path}` is not in canonical form: a durable fact writes ids as strings, \
                 field paths as component lists, value references and selectors as tagged \
                 maps, and sets as sorted lists, with no shorthand and no field omitted"
            ),
        }
    }
}

impl std::error::Error for CanonicalError {}

/// Reads a value that must be in its canonical form: it parses, and
/// serializing what it parsed to gives back exactly the value read.
pub fn read_canonical<T: Serialize + DeserializeOwned>(value: &Value) -> Result<T, CanonicalError> {
    let parsed: T =
        serde_json::from_value(value.clone()).map_err(|error| CanonicalError::Invalid {
            message: error.to_string(),
        })?;

    let canonical = serde_json::to_value(&parsed).map_err(|error| CanonicalError::Invalid {
        message: error.to_string(),
    })?;

    match first_difference(value, &canonical, "") {
        Some(error) => Err(error),
        None => Ok(parsed),
    }
}

/// A value as compact JSON with every map's keys in sorted order: one
/// text per value, whatever order its fields were serialized in.
pub(crate) fn canonical_text(value: &Value) -> String {
    let mut text = String::new();

    write_canonical(value, &mut text);

    text
}

fn write_canonical(value: &Value, text: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();

            entries.sort_by(|(a, _), (b, _)| a.cmp(b));

            text.push('{');

            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }

                text.push_str(&Value::String(key.clone()).to_string());
                text.push(':');
                write_canonical(value, text);
            }

            text.push('}');
        }

        Value::Array(items) => {
            text.push('[');

            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }

                write_canonical(item, text);
            }

            text.push(']');
        }

        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            text.push_str(&value.to_string());
        }
    }
}

/// Where a read value first departs from its canonical form.
fn first_difference(read: &Value, canonical: &Value, path: &str) -> Option<CanonicalError> {
    let at = |segment: &str| {
        if path.is_empty() {
            segment.to_string()
        } else {
            format!("{path}.{segment}")
        }
    };

    match (read, canonical) {
        (Value::Object(read), Value::Object(canonical)) => {
            if let Some(field) = read.keys().find(|field| !canonical.contains_key(*field)) {
                return Some(CanonicalError::UnknownField { path: at(field) });
            }

            canonical
                .iter()
                .find_map(|(field, expected)| match read.get(field) {
                    Some(found) => first_difference(found, expected, &at(field)),
                    None => Some(CanonicalError::NotCanonical { path: at(field) }),
                })
        }

        (Value::Array(read), Value::Array(canonical)) if read.len() == canonical.len() => read
            .iter()
            .zip(canonical)
            .enumerate()
            .find_map(|(index, (found, expected))| {
                first_difference(found, expected, &format!("{path}[{index}]"))
            }),

        _ if read == canonical => None,

        _ => Some(CanonicalError::NotCanonical {
            path: if path.is_empty() {
                "the value".to_string()
            } else {
                path.to_string()
            },
        }),
    }
}

/// Why a durable record carrying facts was refused on its version
/// axes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactVersionError {
    /// The record declares no DSL contract version.
    DslMissing,

    /// The record declares a DSL contract version this build does not
    /// read.
    DslMismatch { found: DslVersion },

    /// The record declares no fact format.
    FactFormatMissing,

    /// The record declares a fact format this build does not read.
    FactFormatMismatch { found: FactFormat },
}

impl fmt::Display for FactVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DslMissing => write!(
                f,
                "the record declares no dsl; this build reads dsl {DSL_VERSION}"
            ),

            Self::DslMismatch { found } => write!(
                f,
                "the record declares dsl {found}, but this build reads dsl {DSL_VERSION}: its \
                 facts are stated in another contract's vocabulary"
            ),

            Self::FactFormatMissing => write!(
                f,
                "the record declares no fact_format; this build reads fact format {FACT_FORMAT}"
            ),

            Self::FactFormatMismatch { found } => write!(
                f,
                "the record declares fact format {found}, but this build reads fact format \
                 {FACT_FORMAT}"
            ),
        }
    }
}

impl std::error::Error for FactVersionError {}

/// The lenient probe: only the two version axes, every other field
/// tolerated, so a version break is refused by name rather than
/// surfacing as a shape error.
#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default)]
    dsl: Option<DslVersion>,

    #[serde(default)]
    fact_format: Option<FactFormat>,
}

/// The first phase of reading a durable record that carries facts:
/// probe its `dsl` and `fact_format`, and refuse any pair this build
/// does not read, before parsing the record strictly. A format in
/// [`FACT_READS`] is read as the current one, as a DSL version in
/// `DSL_READS` is.
pub fn check_versions(record: &Value) -> Result<(), FactVersionError> {
    let probe = serde_json::from_value::<VersionProbe>(record.clone()).unwrap_or(VersionProbe {
        dsl: None,
        fact_format: None,
    });

    match probe.dsl {
        None => return Err(FactVersionError::DslMissing),
        Some(found) if !DSL_READS.contains(&found) => {
            return Err(FactVersionError::DslMismatch { found });
        }
        Some(_) => {}
    }

    match probe.fact_format {
        None => Err(FactVersionError::FactFormatMissing),
        Some(found) if !FACT_READS.contains(&found) => {
            Err(FactVersionError::FactFormatMismatch { found })
        }
        Some(_) => Ok(()),
    }
}
