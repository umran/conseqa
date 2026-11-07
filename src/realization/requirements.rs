//! Realization requirements (§11 of the realization profiles revision):
//! what a chosen entry demands of the declaration it realizes, and of
//! that declaration's neighbours.
//!
//! The requirements of a chosen entry, and of its chosen options, are
//! in force wherever the entry realizes a declaration (§11.1). Each one
//! applies to the realized declaration itself, `of: self`, or to every
//! declaration in one neighbour relation of it (§11.2), which the
//! design layer's graph queries answer: Kafka's record key lands on
//! every effect site publishing to the topic, Envoy's routing header on
//! every caller through the router, PostgreSQL's serialization retry on
//! every serializable transaction.
//!
//! Each requirement on each declaration it applies to is one durable
//! `realization` fact (§12.2), whose implementer is that declaration.
//! Part IV requires it of that declaration's code, and attests,
//! evidences, reviews, and reports it like any other required fact
//! (§11.3). No basis holds one, and no verdict depends on one.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::analyzer::basis::{DeclarationKey, Fact, TextualKey};
use crate::conformance::completeness::site_declaration;
use crate::design::graph::SymbolGraph;
use crate::design::graph_query::{self, GraphQuery, QueryRow};
use crate::design::workspace::WorkspaceState;
use crate::spec::{Id, Input, TransactionIsolation};

use super::catalogue::{Catalogue, EntryRef, Relation};
use super::choice::Status;

/// One realization requirement on one declaration: the fact Part IV
/// requires of the declaration's code, and what the entry says of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Placed {
    /// The `realization` fact: the declaration, the entry, the
    /// declaration the entry realizes, and the requirement.
    pub fact: Fact,

    /// How the requirement reaches the declaration from the one the
    /// entry realizes.
    pub relation: Relation,

    /// The requirement, in the entry's words.
    pub statement: String,

    /// Where, in a region, its evidence is found, when the entry says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_for: Option<String>,

    /// The entry's checkers covering it, by id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checkers: Vec<String>,
}

impl Placed {
    /// The declaration the requirement is on.
    pub fn declaration(&self) -> &TextualKey {
        match &self.fact {
            Fact::Realization { declaration, .. } => declaration,
            _ => unreachable!("a placed requirement is a realization fact"),
        }
    }

    /// The declaration whose chosen entry demands it.
    pub fn realizes(&self) -> &TextualKey {
        match &self.fact {
            Fact::Realization { realizes, .. } => realizes,
            _ => unreachable!("a placed requirement is a realization fact"),
        }
    }

    pub fn entry(&self) -> &EntryRef {
        match &self.fact {
            Fact::Realization { entry, .. } => entry,
            _ => unreachable!("a placed requirement is a realization fact"),
        }
    }

    pub fn requirement(&self) -> &str {
        match &self.fact {
            Fact::Realization { requirement, .. } => requirement,
            _ => unreachable!("a placed requirement is a realization fact"),
        }
    }
}

/// Every realization requirement in force under a status, on every
/// declaration it applies to, in the realized declarations' order and
/// then each entry's.
pub fn placed(
    status: &Status,
    catalogue: &Catalogue,
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
) -> Vec<Placed> {
    let mut placed = Vec::new();

    for realized in &status.declarations {
        let Some((chosen, options)) = realized.state.realization() else {
            continue;
        };

        let Some(entry) = catalogue
            .platform(&chosen.id)
            .filter(|entry| entry.version == chosen.version)
        else {
            continue;
        };

        let options = options.iter().cloned().collect();

        for requirement in entry.requirements_with(&options) {
            let checkers: Vec<String> = entry
                .checkers
                .iter()
                .filter(|checker| checker.covers.contains(&requirement.id))
                .map(|checker| checker.id.clone())
                .collect();

            for declaration in neighbours(requirement.of, &realized.declaration, workspace, graph) {
                let Some(declaration) = TextualKey::new(declaration) else {
                    continue;
                };

                placed.push(Placed {
                    fact: Fact::Realization {
                        declaration,
                        entry: chosen.clone(),
                        realizes: realized.declaration.clone(),
                        requirement: requirement.id.clone(),
                    },
                    relation: requirement.of,
                    statement: requirement.statement.clone(),
                    look_for: entry
                        .evidence
                        .get(&requirement.id)
                        .map(|evidence| evidence.look_for.clone()),
                    checkers: checkers.clone(),
                });
            }
        }
    }

    placed
}

/// The realization facts in force, by canonical form.
pub fn facts(placed: &[Placed]) -> BTreeMap<String, &Placed> {
    placed
        .iter()
        .map(|placed| (placed.fact.canonical(), placed))
        .collect()
}

/// The declarations in one neighbour relation of a realized declaration
/// (§11.2), resolved against the model.
pub fn neighbours(
    relation: Relation,
    realized: &TextualKey,
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
) -> BTreeSet<DeclarationKey> {
    let declaration = realized.key();
    let run = |query: GraphQuery| graph_query::run(workspace, graph, &query).rows;

    match relation {
        Relation::Itself => BTreeSet::from([declaration.clone()]),

        Relation::Producers => {
            let channel = match declaration {
                DeclarationKey::Topic(topic) | DeclarationKey::TopicRuntime(topic) => {
                    Some(topic.clone())
                }
                DeclarationKey::Outbox { outbox, .. } => Some(outbox.clone()),
                DeclarationKey::OutboxRuntime { operation, input } => {
                    consumed_outbox(workspace, operation, input)
                }
                _ => None,
            };

            // The publishers index is keyed by the channel's id, a
            // topic's or an outbox's alike.
            channel
                .map(|topic| {
                    run(GraphQuery::Publishers { topic })
                        .into_iter()
                        .filter_map(|row| match row {
                            QueryRow::Publisher(publisher) => {
                                Some(site_declaration(publisher.site))
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }

        Relation::Consumers => match declaration {
            DeclarationKey::Topic(topic) | DeclarationKey::TopicRuntime(topic) => {
                run(GraphQuery::Consumers {
                    topic: topic.clone(),
                })
                .into_iter()
                .filter_map(|row| match row {
                    QueryRow::Consumer(consumer) => Some(DeclarationKey::Input {
                        operation: consumer.operation,
                        input: consumer.input,
                    }),
                    _ => None,
                })
                .collect()
            }

            _ => BTreeSet::new(),
        },

        Relation::Callers => {
            let DeclarationKey::Router(router) = declaration else {
                return BTreeSet::new();
            };

            let Some(boundary) = workspace
                .runtime
                .routers
                .get(router)
                .map(|router| &router.boundary)
            else {
                return BTreeSet::new();
            };

            run(GraphQuery::Callers {
                operation: boundary.operation.clone(),
            })
            .into_iter()
            .filter_map(|row| match row {
                QueryRow::Call(call) if call.target_input == boundary.input => {
                    Some(site_declaration(call.site))
                }
                _ => None,
            })
            .collect()
        }

        Relation::Writers => {
            let DeclarationKey::DataModel(data_model) = declaration else {
                return BTreeSet::new();
            };

            let objects: Vec<&Id> = workspace
                .data_models
                .get(data_model)
                .map(|declared| declared.objects.keys().collect())
                .unwrap_or_default();

            objects
                .into_iter()
                .flat_map(|object| {
                    run(GraphQuery::Writers {
                        data_model: data_model.clone(),
                        object: object.clone(),
                        field: None,
                    })
                })
                .filter_map(|row| match row {
                    QueryRow::Access(access) => Some(DeclarationKey::Transaction {
                        operation: access.transaction.operation,
                        transaction: access.transaction.transaction,
                    }),
                    _ => None,
                })
                .collect()
        }

        Relation::Transactions
        | Relation::SerializableTransactions
        | Relation::GuardedTransactions => {
            let DeclarationKey::DataModel(data_model) = declaration else {
                return BTreeSet::new();
            };

            let mut transactions = BTreeSet::new();

            for (operation, draft) in &workspace.operations {
                let Some(program) = &draft.program else {
                    continue;
                };

                for (_, transaction) in program.transactions() {
                    let selected = transaction.data_model.as_ref() == Some(data_model)
                        && match relation {
                            Relation::SerializableTransactions => {
                                transaction.isolation == TransactionIsolation::Serializable
                            }
                            Relation::GuardedTransactions => transaction.rejects(),
                            _ => true,
                        };

                    if selected {
                        transactions.insert(DeclarationKey::Transaction {
                            operation: operation.clone(),
                            transaction: transaction.id.clone(),
                        });
                    }
                }
            }

            transactions
        }
    }
}

/// The outbox an outbox runtime's input consumes.
fn consumed_outbox(workspace: &WorkspaceState, operation: &Id, input: &Id) -> Option<Id> {
    match workspace.operations.get(operation)?.inputs.get(input)? {
        Input::Outbox(outbox) => Some(outbox.outbox.clone()),
        Input::Request(_) | Input::Subscription(_) => None,
    }
}
