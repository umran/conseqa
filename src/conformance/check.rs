//! Gates (§15.3 of the conformance revision): what `conseqa conform
//! check` fails on.
//!
//! A check always fails on a scan error, since a file with one binds
//! nothing and no record of its code can be written. Beyond that, each
//! branch chooses its policy: `--fail-on` names conditions, by their
//! names in §10.1, and `not_conformant`, which fails when a proven
//! obligation is not conformant. A non-empty work list is a normal
//! state, so nothing else fails (§11.6): a feature branch can let the
//! spec lead, and a deploy branch refuse what is not conformant.
//!
//! Given a base or a spec range, a check gates only on what that diff
//! scopes (§11.5): the conditions of the items in scope, and the
//! conformance of the obligations the diff moved.

use std::collections::BTreeSet;
use std::fmt;
use std::fmt::Write as _;
use std::str::FromStr;

use serde::{Serialize, Serializer};

use crate::analyzer::basis::{ObligationKey, TextualKey};

use super::markers::ScanError;
use super::plan::{self, Scope};
use super::state::{ConditionKind, Derivation};

/// What a check can fail on, besides scan errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Gate {
    /// Any item in scope has the condition.
    Condition(ConditionKind),

    /// Any proven obligation in scope is not conformant.
    NotConformant,
}

impl fmt::Display for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Condition(kind) => kind.fmt(f),
            Self::NotConformant => f.write_str("not_conformant"),
        }
    }
}

impl Serialize for Gate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl FromStr for Gate {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        if name == "not_conformant" {
            return Ok(Self::NotConformant);
        }

        ConditionKind::ALL
            .into_iter()
            .find(|kind| kind.to_string() == name)
            .map(Self::Condition)
            .ok_or_else(|| {
                format!(
                    "`{name}` is not a gate; a check fails on {}, or not_conformant",
                    ConditionKind::ALL
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// A check's outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    /// No scan error, and no gate failed.
    pub passed: bool,

    /// Every scan error, which fails a check whatever its scope.
    pub scan_errors: Vec<ScanError>,

    /// Each gate that failed, with what failed it, in the order given.
    pub failed: Vec<FailedGate>,
}

/// A gate that failed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailedGate {
    pub gate: Gate,

    /// The items in scope with the condition: declarations, or
    /// orphaned keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<TextualKey>,

    /// The proven obligations in scope that are not conformant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<ObligationKey>,
}

/// Checks a derivation against `gates`, within `scope` when one is
/// given (§15.3).
pub fn check(derivation: &Derivation, scope: Option<&Scope>, gates: &[Gate]) -> Check {
    let state = &derivation.state;
    let work = plan::plan(derivation);

    let items: BTreeSet<TextualKey> = match scope {
        Some(scope) => work
            .scoped(scope, derivation)
            .items
            .into_iter()
            .map(|item| item.key)
            .collect(),

        None => work.items.into_iter().map(|item| item.key).collect(),
    };

    let mut failed: Vec<FailedGate> = Vec::new();
    let mut seen: BTreeSet<Gate> = BTreeSet::new();

    for gate in gates {
        if !seen.insert(*gate) {
            continue;
        }

        let failure = match gate {
            Gate::Condition(ConditionKind::Orphaned) => FailedGate {
                gate: *gate,
                items: state
                    .orphaned
                    .iter()
                    .filter(|orphan| items.contains(&orphan.key))
                    .map(|orphan| orphan.key.clone())
                    .collect(),
                obligations: Vec::new(),
            },

            Gate::Condition(kind) => FailedGate {
                gate: *gate,
                items: state
                    .declarations
                    .iter()
                    .filter(|declaration| items.contains(&declaration.declaration))
                    .filter(|declaration| declaration.has(*kind))
                    .map(|declaration| declaration.declaration.clone())
                    .collect(),
                obligations: Vec::new(),
            },

            Gate::NotConformant => FailedGate {
                gate: *gate,
                items: Vec::new(),
                obligations: state
                    .obligations
                    .iter()
                    .filter(|conformance| !conformance.conformant)
                    .filter(|conformance| {
                        scope.is_none_or(|scope| scope.moved.contains(&conformance.obligation))
                    })
                    .map(|conformance| conformance.obligation.clone())
                    .collect(),
            },
        };

        if !failure.items.is_empty() || !failure.obligations.is_empty() {
            failed.push(failure);
        }
    }

    Check {
        passed: state.scan_errors.is_empty() && failed.is_empty(),
        scan_errors: state.scan_errors.clone(),
        failed,
    }
}

impl Check {
    /// The check's outcome in text: each scan error, then each failed
    /// gate with what failed it.
    pub fn render(&self) -> String {
        let mut out = String::new();

        for error in &self.scan_errors {
            let _ = writeln!(out, "scan error  {error}");
        }

        for failure in &self.failed {
            let _ = writeln!(out, "gate failed: {}", failure.gate);

            for item in &failure.items {
                let _ = writeln!(out, "  {item}");
            }

            for obligation in &failure.obligations {
                let _ = writeln!(out, "  {obligation}");
            }
        }

        if self.passed {
            out.push_str("passed\n");
        }

        out
    }
}
