//! Post-commit invalidation: which active tasks' observed contexts no
//! longer hold at the new head.
//!
//! Symbol observations are compared against the changed-symbol set;
//! query observations are conservatively rerun in full against the new
//! head — architecture graphs are small and reruns are cheap (§34).

use super::events::InvalidationCause;
use super::graph_query;
use super::read_set::{TaskReadSet, TrackedQuery, run_search, search_fingerprint};
use super::snapshot::WorkspaceSnapshot;
use super::symbol::SymbolKey;

/// Why `read_set` is stale at `head`, empty when it is not.
pub fn stale_causes(
    read_set: &TaskReadSet,
    changed: &[SymbolKey],
    head: &WorkspaceSnapshot,
) -> Vec<InvalidationCause> {
    let mut causes = Vec::new();

    for (key, observation) in read_set.symbols_ordered() {
        if !changed.contains(key) {
            continue;
        }

        match head.graph.fingerprint(key) {
            None => causes.push(InvalidationCause::RemovedSymbol {
                symbol: key.clone(),
            }),

            Some(current) => {
                if current != observation.fingerprint {
                    causes.push(InvalidationCause::ChangedSymbol {
                        symbol: key.clone(),
                    });
                }
            }
        }
    }

    for observation in read_set.queries_ordered() {
        match &observation.query {
            TrackedQuery::Graph(query) => {
                let result = graph_query::run(&head.workspace, &head.graph, query);

                if result.fingerprint != observation.result_fingerprint {
                    causes.push(InvalidationCause::ChangedQuery {
                        query: query.clone(),
                    });
                }
            }

            TrackedQuery::Search(spec) => {
                let rows = run_search(&head.workspace, &head.graph, spec);

                if search_fingerprint(&rows) != observation.result_fingerprint {
                    causes.push(InvalidationCause::ChangedSearch {
                        search: spec.clone(),
                    });
                }
            }
        }
    }

    causes
}
