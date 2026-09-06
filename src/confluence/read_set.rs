//! Server-owned read tracking.
//!
//! The agent never supplies its own dependency list. Every shared
//! semantic fact delivered to a task through engine tools — symbol
//! reads, operation slices, graph queries, summaries, context bundles
//! — is recorded here, and the commit gate revalidates the whole set
//! against the head (the read-set invariant, §110).

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::spec::Id;

use super::fingerprint::SemanticHash;
use super::graph::SymbolGraph;
use super::graph_query::{GraphQuery, QueryKey};
use super::symbol::{SymbolKey, SymbolVersion};
use super::workspace::WorkspaceState;

/// Everything one task has observed.
#[derive(Debug, Clone, Default)]
pub struct TaskReadSet {
    pub symbols: FxHashMap<SymbolKey, SymbolObservation>,
    pub queries: FxHashMap<QueryKey, QueryObservation>,
    pub summaries: FxHashMap<Id, SummaryObservation>,
}

impl TaskReadSet {
    pub fn record_symbol(&mut self, key: SymbolKey, observation: SymbolObservation) {
        self.symbols.insert(key, observation);
    }

    pub fn record_query(&mut self, observation: QueryObservation) {
        self.queries
            .insert(QueryKey::of_tracked(&observation.query), observation);
    }

    pub fn record_summary(&mut self, operation: Id, observation: SummaryObservation) {
        self.summaries.insert(operation, observation);
    }

    /// The observed symbols in canonical order, for deterministic
    /// validation and diagnostics.
    pub fn symbols_ordered(&self) -> Vec<(&SymbolKey, &SymbolObservation)> {
        let mut entries: Vec<_> = self.symbols.iter().collect();

        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        entries
    }

    /// The observed queries in canonical order.
    pub fn queries_ordered(&self) -> Vec<&QueryObservation> {
        let mut entries: Vec<_> = self.queries.iter().collect();

        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        entries.into_iter().map(|(_, observation)| observation).collect()
    }
}

/// What was observed about one symbol: its version for diagnostics and
/// fast short-circuiting, its fingerprint as the authoritative
/// comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolObservation {
    pub version: SymbolVersion,
    pub fingerprint: SemanticHash,
}

/// One observed set-valued query and the fingerprint of its canonical
/// result — the phantom guard (§21): at commit the query reruns
/// against the head and a changed fingerprint means the answer the
/// agent reasoned from is stale, even if no individually observed
/// symbol changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryObservation {
    pub query: TrackedQuery,
    pub result_fingerprint: SemanticHash,
}

/// A query whose result membership a task may rely on: a canonical
/// graph query, or a symbol search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TrackedQuery {
    Graph(GraphQuery),
    Search(SearchSpec),
}

impl QueryKey {
    pub fn of_tracked(query: &TrackedQuery) -> Self {
        Self(SemanticHash::of(query))
    }
}

impl QueryObservation {
    pub fn graph(query: GraphQuery, result_fingerprint: SemanticHash) -> Self {
        Self {
            query: TrackedQuery::Graph(query),
            result_fingerprint,
        }
    }

    pub fn search(spec: SearchSpec, result_fingerprint: SemanticHash) -> Self {
        Self {
            query: TrackedQuery::Search(spec),
            result_fingerprint,
        }
    }
}

/// Canonical symbol-search parameters. Search is for navigation; the
/// observation exists because an agent may rely on membership of the
/// returned set (§47).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSpec {
    /// Restrict to one symbol kind, named as its snake_case tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<super::symbol::SymbolKind>,

    /// Id prefix, matched against the display form of the key's ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,

    /// Restrict operations to one service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<Id>,
}

/// Runs a symbol search against one snapshot's graph, in canonical
/// order.
pub fn run_search(
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
    spec: &SearchSpec,
) -> Vec<SymbolKey> {
    let mut rows: Vec<SymbolKey> = graph
        .nodes
        .iter()
        .filter(|node| match spec.kind {
            Some(kind) => node.kind == kind,
            None => true,
        })
        .filter(|node| match &spec.prefix {
            Some(prefix) => node.key.to_string().contains(prefix.as_str()),
            None => true,
        })
        .filter(|node| match &spec.service {
            None => true,
            Some(service) => match node.key.operation() {
                Some(operation) => workspace
                    .operations
                    .get(operation)
                    .is_some_and(|draft| &draft.service == service),
                None => matches!(&node.key, SymbolKey::Service(id) if id == service),
            },
        })
        .map(|node| node.key.clone())
        .collect();

    rows.sort();

    rows
}

/// The fingerprint of a search result set.
pub fn search_fingerprint(rows: &[SymbolKey]) -> SemanticHash {
    SemanticHash::of(rows)
}

/// What was observed when a task read an operation's proof summary.
/// V1 validates summaries conservatively: reading one also records the
/// summary's input symbols (interface, program, requirements,
/// execution) as ordinary symbol observations, so any input change
/// invalidates the reader even if the recomputed summary would have
/// been identical (§41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryObservation {
    pub summary_hash: SemanticHash,
}
