//! Checkers (§13 of the realization profiles revision): deterministic
//! checks a catalogue entry configures for the evidence of some of its
//! requirements.
//!
//! Conseqa ships a closed set of kinds, and an entry only configures
//! one, so a catalogue never runs code of its own (§22, decision 6). A
//! checker reads only the regions bound to the declaration its
//! requirement is on, each region's own text with nested regions left
//! out, and never searches the repository (§13.1). It applies to a
//! region by its file's type, and finds:
//!
//! - **confirmed** when the assertion holds of what it finds;
//! - **disputed** when the value it asserts is absent, or a value found
//!   fails it, since a requirement the configuration does not state is
//!   not shown to hold;
//! - **not applicable** when no region bound to the declaration is a
//!   file of its kind.
//!
//! Each finding is a review of the requirement's fact, by the checker,
//! at the digests it read (§13.2). It is derived on every derivation,
//! never written, so it holds of the regions as they are now.

mod hcl;
mod pattern;
mod properties;
mod sql_ddl;
mod yaml_path;

use serde::Serialize;

use super::catalogue::{Assertion, CheckerKind};

/// A region a checker reads: its file, and its own text.
#[derive(Debug, Clone, Copy)]
pub struct Read<'a> {
    pub path: &'a str,
    pub text: &'a str,
}

/// What a checker found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Confirmed,
    Disputed,
    NotApplicable,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Disputed => "disputed",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// A checker's outcome, and what it saw, in words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub outcome: Outcome,
    pub detail: String,
}

impl Finding {
    fn confirmed(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Confirmed,
            detail: detail.into(),
        }
    }

    fn disputed(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Disputed,
            detail: detail.into(),
        }
    }
}

/// The file types a checker kind reads.
fn applies(kind: CheckerKind, path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let ends = |suffixes: &[&str]| suffixes.iter().any(|suffix| path.ends_with(suffix));

    match kind {
        CheckerKind::Hcl => ends(&[".tf", ".hcl"]),
        CheckerKind::YamlPath => ends(&[".yaml", ".yml", ".json"]),
        CheckerKind::Properties => ends(&[".properties", ".conf", ".cfg"]),
        CheckerKind::SqlDdl => ends(&[".sql"]),
        CheckerKind::Pattern => true,
    }
}

/// Runs one checker over the regions bound to the declaration its
/// requirement is on.
pub fn run(kind: CheckerKind, assert: &Assertion, regions: &[Read<'_>]) -> Finding {
    let read: Vec<&Read<'_>> = regions
        .iter()
        .filter(|region| applies(kind, region.path))
        .collect();

    if read.is_empty() {
        return Finding {
            outcome: Outcome::NotApplicable,
            detail: "no region bound to the declaration is a file the checker reads".to_string(),
        };
    }

    match assert {
        Assertion::Hcl(assert) => hcl::check(assert, &read),
        Assertion::YamlPath(assert) => yaml_path::check(assert, &read),
        Assertion::Properties(assert) => properties::check(assert, &read),
        Assertion::SqlDdl(assert) => sql_ddl::check(assert, &read),
        Assertion::Pattern(assert) => pattern::check(assert, &read),
    }
}

/// Removes the indentation every non-blank line shares, so that a region
/// cut from inside a nested document reads as a document of its own.
fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);

    text.lines()
        .map(|line| line.get(indent..).unwrap_or(line.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::super::catalogue::{
        HclAssert, PatternAssert, PropertiesAssert, Scalar, SqlDdlAssert, YamlPathAssert,
    };
    use super::*;

    fn scalar(text: &str) -> Scalar {
        Scalar(text.to_string())
    }

    fn outcome(kind: CheckerKind, assert: &Assertion, path: &str, text: &str) -> Outcome {
        run(kind, assert, &[Read { path, text }]).outcome
    }

    /// Each kind confirms a matching region, disputes a mismatching one,
    /// and finds nothing to read outside its kind (§20.6).
    #[test]
    fn each_kind_confirms_disputes_and_reads_only_its_files() {
        let cases: Vec<(CheckerKind, Assertion, &str, &str, &str)> = vec![
            (
                CheckerKind::Hcl,
                Assertion::Hcl(HclAssert {
                    resource: "kafka_topic".to_string(),
                    attribute: Some("partitions".to_string()),
                    present: None,
                    equals: Some(scalar("1")),
                    contains: None,
                }),
                "infra/topics.tf",
                "resource \"kafka_topic\" \"orders\" {\n  partitions = 1\n}\n",
                "resource \"kafka_topic\" \"orders\" {\n  partitions = 12\n}\n",
            ),
            (
                CheckerKind::YamlPath,
                Assertion::YamlPath(YamlPathAssert {
                    path: "lb_policy".to_string(),
                    exists: None,
                    equals: None,
                    one_of: Some(vec![scalar("RING_HASH"), scalar("MAGLEV")]),
                }),
                "envoy/clusters.yaml",
                "  - name: checkout\n    lb_policy: RING_HASH\n",
                "  - name: checkout\n    lb_policy: ROUND_ROBIN\n",
            ),
            (
                CheckerKind::Properties,
                Assertion::Properties(PropertiesAssert {
                    key: "enable.idempotence".to_string(),
                    equals: scalar("true"),
                }),
                "config/producer.properties",
                "acks=all\nenable.idempotence=true\n",
                "acks=all\nenable.idempotence=false\n",
            ),
            (
                CheckerKind::SqlDdl,
                Assertion::SqlDdl(SqlDdlAssert {
                    table: "orders".to_string(),
                    primary_key: Some(vec!["order_id".to_string()]),
                    unique: None,
                    partition_by: None,
                    trigger: None,
                }),
                "migrations/0001_orders.sql",
                "CREATE TABLE orders (order_id uuid PRIMARY KEY, total bigint);\n",
                "CREATE TABLE orders (order_id uuid, total bigint);\n",
            ),
            (
                CheckerKind::Pattern,
                Assertion::Pattern(PatternAssert {
                    regex: r"WHERE\s+version\s*=".to_string(),
                }),
                "src/orders.rs",
                "UPDATE orders SET paid = true WHERE version = $1\n",
                "UPDATE orders SET paid = true\n",
            ),
        ];

        for (kind, assert, path, matching, mismatching) in &cases {
            assert_eq!(
                outcome(*kind, assert, path, matching),
                Outcome::Confirmed,
                "{kind:?}"
            );
            assert_eq!(
                outcome(*kind, assert, path, mismatching),
                Outcome::Disputed,
                "{kind:?}"
            );

            // A region of another file type is not this checker's to read.
            if *kind != CheckerKind::Pattern {
                assert_eq!(
                    outcome(*kind, assert, "README.md", matching),
                    Outcome::NotApplicable,
                    "{kind:?}"
                );
            }
        }

        // Pattern reads any file, and no region at all is nothing to read.
        let (kind, assert, ..) = &cases[4];

        assert_eq!(run(*kind, assert, &[]).outcome, Outcome::NotApplicable);
    }

    #[test]
    fn an_absent_value_disputes_and_a_region_cut_from_a_document_reads() {
        let policy = Assertion::YamlPath(YamlPathAssert {
            path: "clusters.lb_policy".to_string(),
            exists: None,
            equals: Some(scalar("MAGLEV")),
            one_of: None,
        });

        // Envoy's default is round robin: a cluster naming no policy does
        // not show the one the entry needs.
        let finding = run(
            CheckerKind::YamlPath,
            &policy,
            &[Read {
                path: "envoy.yaml",
                text: "    clusters:\n    - name: checkout\n",
            }],
        );

        assert_eq!(finding.outcome, Outcome::Disputed);
        assert_eq!(finding.detail, "`clusters.lb_policy` is not set");

        let finding = run(
            CheckerKind::YamlPath,
            &policy,
            &[Read {
                path: "envoy.yaml",
                text: "    clusters:\n    - name: checkout\n      lb_policy: MAGLEV\n",
            }],
        );

        assert_eq!(finding.outcome, Outcome::Confirmed, "{}", finding.detail);
    }
}
