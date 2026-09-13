use std::collections::BTreeMap;
use std::fmt;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::spec::Id;

/// A first-class `Result<Ok, Err>` contract: a tagged sum holding
/// exactly one of an `Ok` payload shaped by `ok` or an `Err` payload
/// belonging to one of the named logical error classes in `errors`,
/// shaped by that class's schema.
///
/// Mutual exclusivity is structural. Conseqa models the algebraic
/// outcome, not any language's API around it.
///
/// `Err` is a *logical* returned outcome — a synchronous interaction
/// completed and reported a modeled failure, such as a declined card.
/// It is not an interrupted execution: a crash, a timeout, or a lost
/// connection is an idempotency and recoverability question, not an
/// `Err` payload. Execution interruption is never synthesized as an
/// `Err`.
///
/// `Ok` is terminal by definition: it resolves the logical interaction.
/// Whether an `Err` does the same is each class's declared
/// [`ErrorDisposition`], which belongs to this contract, not to the
/// schema — one error schema may be terminal in one class and
/// retryable in another.
///
/// ```yaml
/// result:
///   ok: schema.Payment
///   errors:
///     already_processed:
///       schema: schema.AlreadyProcessed
///       disposition: terminal
///     conflict:
///       schema: schema.ConcurrentConflict
///       disposition: retryable
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultType {
    pub ok: Id,

    /// The logical error classes, keyed by class id. A contract with
    /// no error classes returns `Ok` alone.
    #[serde(default)]
    pub errors: BTreeMap<Id, ErrorResultType>,
}

impl ResultType {
    /// The declared error class, if the contract names it.
    pub fn error(&self, class: &Id) -> Option<&ErrorResultType> {
        self.errors.get(class)
    }

    /// The schema of one arm's payload: the `ok` schema, or the schema
    /// of the named error class when the contract declares it.
    pub fn schema_of(&self, arm: &ResultArm) -> Option<&Id> {
        match arm {
            ResultArm::Ok => Some(&self.ok),
            ResultArm::Err { error } => self.errors.get(error).map(|class| &class.schema),
        }
    }
}

/// Which arm of a `Result` an outcome, a match arm, or a decision
/// refers to: the `ok` arm, or the arm of one named error class.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultArm {
    Ok,
    Err { error: Id },
}

impl ResultArm {
    pub fn err(error: &Id) -> Self {
        Self::Err {
            error: error.clone(),
        }
    }

    pub fn variant(&self) -> ResultVariant {
        match self {
            Self::Ok => ResultVariant::Ok,
            Self::Err { .. } => ResultVariant::Err,
        }
    }

    /// The error class this arm selects, if it is an error arm.
    pub fn error(&self) -> Option<&Id> {
        match self {
            Self::Ok => None,
            Self::Err { error } => Some(error),
        }
    }
}

impl fmt::Display for ResultArm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::Err { error } => write!(f, "err:{error}"),
        }
    }
}

/// The `Err` half of a result contract: the payload schema and the
/// declared disposition of observing that error.
///
/// The disposition is part of the result contract rather than the
/// schema, so one error schema may be terminal in one contract and
/// retryable in another.
///
/// Canonical form is the map with both fields. The shorthand — a bare
/// schema id — declares `disposition: unspecified`; no shorthand may
/// silently declare `terminal` or `retryable`, because `unspecified`
/// is epistemic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResultType {
    pub schema: Id,
    pub disposition: ErrorDisposition,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename = "ErrorResultType")]
struct ErrorResultTypeLong {
    schema: Id,

    #[serde(default)]
    disposition: ErrorDisposition,
}

impl<'de> Deserialize<'de> for ErrorResultType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ErrorResultTypeVisitor)
    }
}

struct ErrorResultTypeVisitor;

impl<'de> Visitor<'de> for ErrorResultTypeVisitor {
    type Value = ErrorResultType;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "an error result contract: a schema id such as `schema.CardDeclined`, \
             or a map with `schema` and `disposition`",
        )
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let schema = Id(text.trim().to_string());

        if schema.0.is_empty() {
            return Err(E::custom("expected a schema id"));
        }

        Ok(ErrorResultType {
            schema,
            disposition: ErrorDisposition::Unspecified,
        })
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let long = ErrorResultTypeLong::deserialize(MapAccessDeserializer::new(map))?;

        Ok(ErrorResultType {
            schema: long.schema,
            disposition: long.disposition,
        })
    }
}

/// Whether observing the contract's `Err` terminally resolves the
/// logical interaction, or conclusively ends one attempt while
/// semantically admitting another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorDisposition {
    /// No usable fact: the model does not say whether observing this
    /// `Err` terminally resolves the logical interaction or admits
    /// another attempt. Nothing may be inferred.
    #[default]
    Unspecified,

    /// Observing this `Err` terminally resolves the logical
    /// interaction with the declared error payload.
    Terminal,

    /// Observing this `Err` conclusively ends the current attempt but
    /// does not terminally resolve the logical interaction; another
    /// attempt is semantically admitted. It does not say a retry
    /// occurs, succeeds, or returns the same error — those are
    /// execution semantics Conseqa does not model here.
    Retryable,
}

impl fmt::Display for ErrorDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unspecified => "unspecified",
            Self::Terminal => "terminal",
            Self::Retryable => "retryable",
        })
    }
}

/// Which arm of a `Result` an outcome, a match arm, or a value source
/// refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultVariant {
    Ok,
    Err,
}

impl fmt::Display for ResultVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::Err => "err",
        })
    }
}
