//! The conformance policy (§14.4 of the conformance revision): which
//! kinds must be bound, and what a record needs reviewed.
//!
//! A project states its own in `.conseqa/conformance/policy.yaml`,
//! committed beside the records. Where the file is absent, or omits a
//! field, the defaults hold. The file is read as strictly as a record,
//! and a policy that does not load fails the derivation rather than
//! being set aside, since every condition depends on it.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::design::store::PROJECT_DIR;

use super::{ConformanceError, DIR, FORMAT, bindable_kind, bindable_kinds};

/// The policy's file name, in the conformance directory.
pub const FILE: &str = "policy.yaml";

/// The kinds bound by default: the declarations that do the modeled
/// work, and the runtime declarations whose code a proof relies on.
const REQUIRE_BINDING: [&str; 8] = [
    "program",
    "transaction",
    "effect_site",
    "data_object",
    "outbox",
    "topic",
    "subscription_runtime",
    "outbox_runtime",
];

/// The fact kinds a confirming checker review may satisfy. No catalogue
/// checker covers a proof fact: a checker can confirm that a Terraform
/// topic exists, never that a conditional update guards what a proof
/// needs.
const CHECKABLE: [&str; 1] = ["realization"];

/// A project's conformance policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The kinds whose declarations are `unbound` without a region
    /// (§10.1).
    pub require_binding: BTreeSet<String>,

    pub review: ReviewPolicy,
}

/// What a record needs reviewed by another party (§14.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPolicy {
    /// The facts a proof relies on.
    pub relied_on_facts: ReviewRule,

    /// Completeness claims.
    pub completeness: ReviewRule,

    /// The structural claim.
    pub structural: ReviewRule,

    /// Realization facts: relied on by no proof, but demanded by a
    /// chosen realization (Part VI §11).
    pub realization: ReviewRule,

    /// The fact kinds whose required review a confirming checker review
    /// satisfies.
    pub checker_satisfies: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRule {
    Required,
    Optional,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            require_binding: REQUIRE_BINDING
                .iter()
                .map(|kind| kind.to_string())
                .collect(),
            review: ReviewPolicy::default(),
        }
    }
}

impl Default for ReviewPolicy {
    fn default() -> Self {
        Self {
            relied_on_facts: ReviewRule::Required,
            completeness: ReviewRule::Required,
            structural: ReviewRule::Optional,
            realization: ReviewRule::Required,
            checker_satisfies: CHECKABLE.iter().map(|kind| kind.to_string()).collect(),
        }
    }
}

impl Policy {
    /// The policy of the project at `project_dir`: its file, or the
    /// defaults when it has none.
    pub fn read(project_dir: &Path) -> Result<Self, ConformanceError> {
        let path = project_dir.join(PROJECT_DIR).join(DIR).join(FILE);

        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(ConformanceError::Io { path, error }),
        };

        let parse = |message: String| ConformanceError::Parse {
            path: path.clone(),
            message,
        };

        // The format first, so that a policy of another format is
        // refused for its format rather than for a field this one lacks.
        let probe: Value = serde_yaml::from_str(&text).map_err(|error| parse(error.to_string()))?;

        match probe.get("format").and_then(Value::as_u64) {
            Some(found) if found == u64::from(FORMAT) => {}
            Some(found) => return Err(ConformanceError::FormatMismatch { path, found }),
            None => return Err(parse("missing `format`".to_string())),
        }

        let PolicyFile {
            format,
            require_binding,
            review,
        } = serde_yaml::from_str(&text).map_err(|error| parse(error.to_string()))?;

        debug_assert_eq!(format, u64::from(FORMAT), "the probe read the same field");

        if let Some(kind) = require_binding.iter().find(|kind| !bindable_kind(kind)) {
            return Err(parse(format!(
                "`require_binding` lists `{kind}`, which is not a bindable kind; the bindable \
                 kinds are {}",
                bindable_kinds()
                    .map(|(kind, _)| format!("`{kind}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        if let Some(kind) = review
            .checker_satisfies
            .iter()
            .find(|kind| !CHECKABLE.contains(&kind.as_str()))
        {
            return Err(parse(format!(
                "`review.checker_satisfies` lists `{kind}`, but a checker review satisfies \
                 only `realization` facts: no catalogue checker covers a proof fact"
            )));
        }

        Ok(Self {
            require_binding: require_binding.into_iter().collect(),
            review: ReviewPolicy {
                relied_on_facts: review.relied_on_facts,
                completeness: review.completeness,
                structural: review.structural,
                realization: review.realization,
                checker_satisfies: review.checker_satisfies.into_iter().collect(),
            },
        })
    }

    /// Whether declarations of `kind` must be bound.
    pub fn requires_binding(&self, kind: &str) -> bool {
        self.require_binding.contains(kind)
    }
}

/// The policy file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    format: u64,

    #[serde(default = "require_binding")]
    require_binding: Vec<String>,

    #[serde(default)]
    review: ReviewFile,
}

fn require_binding() -> Vec<String> {
    REQUIRE_BINDING
        .iter()
        .map(|kind| kind.to_string())
        .collect()
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ReviewFile {
    relied_on_facts: ReviewRule,
    completeness: ReviewRule,
    structural: ReviewRule,
    realization: ReviewRule,
    checker_satisfies: Vec<String>,
}

impl Default for ReviewFile {
    fn default() -> Self {
        let ReviewPolicy {
            relied_on_facts,
            completeness,
            structural,
            realization,
            checker_satisfies,
        } = ReviewPolicy::default();

        Self {
            relied_on_facts,
            completeness,
            structural,
            realization,
            checker_satisfies: checker_satisfies.into_iter().collect(),
        }
    }
}
