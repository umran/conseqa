use std::fmt;

use serde::Deserialize;

use crate::spec::{DSL_VERSION, DslVersion, Model};

/// Why a specification document was refused before or during parsing.
#[derive(Debug)]
pub enum ParseError {
    /// The document declares a DSL contract version this build does
    /// not speak. The contract changed and specifications are not
    /// migrated automatically — re-author against the current DSL.
    DslVersionMismatch { found: DslVersion },

    /// The document declares no DSL contract version at all: it
    /// predates versioning, which began at dsl 1.
    DslVersionMissing,

    Yaml(serde_yaml::Error),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DslVersionMismatch { found } => write!(
                f,
                "specification declares dsl {found}, but this build reads dsl {DSL_VERSION}; \
                 the contract changed and specifications are not migrated automatically — \
                 re-author against the current DSL"
            ),

            Self::DslVersionMissing => write!(
                f,
                "specification declares no dsl version and predates versioning; this build \
                 reads dsl {DSL_VERSION} — re-author against the current DSL and declare \
                 `dsl: {DSL_VERSION}`"
            ),

            Self::Yaml(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Yaml(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_yaml::Error> for ParseError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Yaml(error)
    }
}

/// The lenient version probe: reads nothing but the declared `dsl`
/// field, tolerating every other field, so the refusal names the
/// version break instead of surfacing it as a shape error.
#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default)]
    dsl: Option<DslVersion>,
}

/// Two-phase read of a whole specification document: probe the
/// declared DSL contract version, dispatch, then parse strictly.
pub fn parse(source: &str) -> Result<Model, ParseError> {
    let probe: VersionProbe = serde_yaml::from_str(source)?;

    match probe.dsl {
        Some(found) if found != DSL_VERSION => Err(ParseError::DslVersionMismatch { found }),
        None => Err(ParseError::DslVersionMissing),
        Some(_) => Ok(serde_yaml::from_str(source)?),
    }
}

pub fn serialize(model: &Model) -> Result<String, serde_yaml::Error> {
    serde_yaml::to_string(model)
}
