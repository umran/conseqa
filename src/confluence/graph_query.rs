//! Canonical typed graph queries.
//!
//! No general query language in V1 (§22 of the confluence spec): a
//! closed enum keeps parameters canonical, which makes caching,
//! fingerprinting, and phantom revalidation straightforward. Every
//! result is canonically ordered and fingerprinted; the commit gate
//! reruns observed queries against the head and compares fingerprints
//! to reject phantoms (§21).

use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::spec::{
    Derivation, FieldPath, Id, OperationStep, ResultVariant, Transaction, TransactionStep,
    ValueRef, ValueSource,
};

use super::fingerprint::SemanticHash;
use super::graph::{
    CallEdge, ConsumerRef, EdgeKind, ObjectAccess, ObjectPathKey, PublisherRef, SymbolGraph,
    TransactionRef, TransitionKey,
};
use super::symbol::SymbolKey;
use super::workspace::WorkspaceState;

/// One canonical semantic query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphQuery {
    Callers {
        operation: Id,
    },
    Callees {
        operation: Id,
    },

    Readers {
        data_model: Id,
        object: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<FieldPath>,
    },

    Writers {
        data_model: Id,
        object: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<FieldPath>,
    },

    Publishers {
        topic: Id,
    },
    Consumers {
        topic: Id,
    },

    TransitionUsers {
        machine: Id,
        transition: Id,
    },

    ReferencesTo {
        symbol: SymbolKey,
    },

    ImpactedBy {
        symbol: SymbolKey,
        depth: u8,
    },

    OperationNeighborhood {
        operation: Id,
        depth: u8,
    },

    ProvenanceRoots {
        operation: Id,
        binding: Id,
    },
}

/// Canonical identity of a query, for read-set bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueryKey(pub SemanticHash);

impl QueryKey {
    pub fn of(query: &GraphQuery) -> Self {
        Self(SemanticHash::of(query))
    }
}

/// A query's canonical result: rows in canonical order and the
/// fingerprint the commit gate compares at revalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryResult {
    pub query: GraphQuery,
    pub rows: Vec<QueryRow>,
    pub fingerprint: SemanticHash,
}

/// One row of a query result. The shape depends on the query kind;
/// every shape carries enough relationship data to explain itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum QueryRow {
    Call(CallEdge),
    Access(ObjectAccess),
    Publisher(PublisherRef),
    Consumer(ConsumerRef),
    TransitionUse(TransactionRef),

    Reference { from: SymbolKey, edge: EdgeKind },

    Impacted { symbol: SymbolKey, distance: u8 },

    Neighbor { symbol: SymbolKey, distance: u8 },

    Root(ProvenanceRoot),
}

/// An ultimate provenance root of a binding: where its value enters
/// the modeled world. The walk stops at operation boundaries — an
/// effect result is a root, not a door into the target operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProvenanceRoot {
    Input {
        input: Id,
        path: FieldPath,
    },

    StateMachineSubject {
        machine: Id,
        path: FieldPath,
    },

    /// A persistent object field observed by a transaction read.
    ObjectRead {
        transaction: Id,
        object: Id,
        path: FieldPath,
    },

    /// The synchronous result of an effect execution. `variant` is set
    /// when the reference names one arm's payload.
    EffectResult {
        effect: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<ResultVariant>,
        path: FieldPath,
    },

    /// The walk hit a declaration that states no provenance fact.
    Unspecified {
        at: String,
    },
}

/// Runs one query against a snapshot's workspace and graph, returning
/// the canonically ordered, fingerprinted result.
pub fn run(workspace: &WorkspaceState, graph: &SymbolGraph, query: &GraphQuery) -> QueryResult {
    let mut rows = match query {
        GraphQuery::Callers { operation } => graph
            .indexes
            .callers
            .get(operation)
            .into_iter()
            .flatten()
            .cloned()
            .map(QueryRow::Call)
            .collect(),

        GraphQuery::Callees { operation } => graph
            .indexes
            .callees
            .get(operation)
            .into_iter()
            .flatten()
            .cloned()
            .map(QueryRow::Call)
            .collect(),

        GraphQuery::Readers {
            data_model,
            object,
            field,
        } => object_accesses(&graph.indexes.object_readers, data_model, object, field),

        GraphQuery::Writers {
            data_model,
            object,
            field,
        } => object_accesses(&graph.indexes.object_writers, data_model, object, field),

        GraphQuery::Publishers { topic } => graph
            .indexes
            .topic_publishers
            .get(topic)
            .into_iter()
            .flatten()
            .cloned()
            .map(QueryRow::Publisher)
            .collect(),

        GraphQuery::Consumers { topic } => graph
            .indexes
            .topic_consumers
            .get(topic)
            .into_iter()
            .flatten()
            .cloned()
            .map(QueryRow::Consumer)
            .collect(),

        GraphQuery::TransitionUsers {
            machine,
            transition,
        } => graph
            .indexes
            .transition_users
            .get(&TransitionKey {
                machine: machine.clone(),
                transition: transition.clone(),
            })
            .into_iter()
            .flatten()
            .cloned()
            .map(QueryRow::TransitionUse)
            .collect(),

        GraphQuery::ReferencesTo { symbol } => graph
            .node_id(symbol)
            .map(|node| {
                graph
                    .incoming_of(node)
                    .iter()
                    .map(|edge| QueryRow::Reference {
                        from: graph.node_at(edge.to).key.clone(),
                        edge: edge.kind,
                    })
                    .collect()
            })
            .unwrap_or_default(),

        GraphQuery::ImpactedBy { symbol, depth } => traverse(graph, symbol, *depth, Direction::Incoming)
            .into_iter()
            .map(|(symbol, distance)| QueryRow::Impacted { symbol, distance })
            .collect(),

        GraphQuery::OperationNeighborhood { operation, depth } => traverse(
            graph,
            &SymbolKey::Operation(operation.clone()),
            *depth,
            Direction::Both,
        )
        .into_iter()
        .map(|(symbol, distance)| QueryRow::Neighbor { symbol, distance })
        .collect(),

        GraphQuery::ProvenanceRoots { operation, binding } => {
            provenance_roots(workspace, operation, binding)
                .into_iter()
                .map(QueryRow::Root)
                .collect()
        }
    };

    canonicalize(&mut rows);

    let fingerprint = SemanticHash::of(&rows);

    QueryResult {
        query: query.clone(),
        rows,
        fingerprint,
    }
}

/// Sorts rows by their canonical JSON serialization and drops
/// duplicates, so equal result sets always hash equally.
fn canonicalize(rows: &mut Vec<QueryRow>) {
    rows.sort_by_cached_key(|row| {
        serde_json::to_string(row).expect("query rows serialize to JSON")
    });

    rows.dedup();
}

fn object_accesses(
    index: &FxHashMap<ObjectPathKey, Vec<ObjectAccess>>,
    data_model: &Id,
    object: &Id,
    field: &Option<FieldPath>,
) -> Vec<QueryRow> {
    index
        .get(&ObjectPathKey {
            data_model: data_model.clone(),
            object: object.clone(),
        })
        .into_iter()
        .flatten()
        .filter(|access| match field {
            Some(field) => access.access.touches(field),
            None => true,
        })
        .cloned()
        .map(QueryRow::Access)
        .collect()
}

enum Direction {
    Incoming,
    Both,
}

/// Breadth-first traversal from `start`, returning every reached
/// symbol with its minimum distance, excluding the start itself.
fn traverse(
    graph: &SymbolGraph,
    start: &SymbolKey,
    depth: u8,
    direction: Direction,
) -> Vec<(SymbolKey, u8)> {
    let Some(start) = graph.node_id(start) else {
        return Vec::new();
    };

    let mut distances: FxHashMap<u32, u8> = FxHashMap::default();
    let mut frontier = VecDeque::new();

    distances.insert(start.0, 0);
    frontier.push_back(start);

    while let Some(node) = frontier.pop_front() {
        let distance = distances[&node.0];

        if distance >= depth {
            continue;
        }

        let next: Box<dyn Iterator<Item = u32>> = match direction {
            Direction::Incoming => Box::new(graph.incoming_of(node).iter().map(|edge| edge.to.0)),
            Direction::Both => Box::new(
                graph
                    .incoming_of(node)
                    .iter()
                    .chain(graph.outgoing_of(node).iter())
                    .map(|edge| edge.to.0),
            ),
        };

        for neighbor in next {
            if let std::collections::hash_map::Entry::Vacant(entry) = distances.entry(neighbor) {
                entry.insert(distance + 1);
                frontier.push_back(super::graph::NodeId(neighbor));
            }
        }
    }

    distances
        .into_iter()
        .filter(|(node, _)| *node != start.0)
        .map(|(node, distance)| {
            (
                graph.node_at(super::graph::NodeId(node)).key.clone(),
                distance,
            )
        })
        .collect()
}

/// The operation-visible binding sites of one program, with the
/// derivations and transaction contexts a provenance walk needs.
struct ProgramIndex<'a> {
    bindings: FxHashMap<&'a Id, BindingSite<'a>>,
}

enum BindingSite<'a> {
    /// A result observed from executing the effect directly.
    EffectResult { effect: &'a Id },

    /// A result observed from executing an established intent.
    IntentResult { intent: &'a Id },

    /// An intent artifact established inline by a transaction.
    Intent {
        effect: &'a Id,
        values: &'a Derivation,
        transaction: &'a Transaction,
    },

    /// An intent artifact established by a transition application.
    TransitionIntent {
        side_effect: &'a Id,
        values: &'a Derivation,
        transaction: &'a Transaction,
    },

    /// A transaction output artifact.
    Output {
        values: &'a Derivation,
        transaction: &'a Transaction,
    },
}

/// Effect-site derivations by effect id: what a `ValueSource::Effect`
/// reference resolves through.
struct EffectValues<'a> {
    values: &'a Derivation,
    transaction: Option<&'a Transaction>,
}

fn index_program(program: &crate::spec::OperationBlock) -> (ProgramIndex<'_>, FxHashMap<&Id, EffectValues<'_>>) {
    let mut bindings = FxHashMap::default();
    let mut effects: FxHashMap<&Id, EffectValues<'_>> = FxHashMap::default();

    // Async handle → its launch, so a binding produced at a
    // synchronization barrier resolves to the effect it observes.
    let mut handles: FxHashMap<&Id, AsyncHandleSite<'_>> = FxHashMap::default();

    for (_, step) in program.steps_with_locations() {
        match step {
            OperationStep::ExecuteEffectAsync(execute) => {
                handles.insert(&execute.handle, AsyncHandleSite::Direct {
                    effect: &execute.effect_id,
                });
            }

            OperationStep::ExecuteEffectIntentAsync(execute) => {
                handles.insert(&execute.handle, AsyncHandleSite::Intent {
                    intent: &execute.intent,
                });
            }

            _ => {}
        }
    }

    for (_, step) in program.steps_with_locations() {
        match step {
            OperationStep::Transaction(transaction) => {
                for inner in &transaction.steps {
                    match inner {
                        TransactionStep::EstablishEffectIntent(establish) => {
                            bindings.insert(
                                &establish.bind,
                                BindingSite::Intent {
                                    effect: &establish.effect_id,
                                    values: &establish.values,
                                    transaction,
                                },
                            );

                            effects.insert(
                                &establish.effect_id,
                                EffectValues {
                                    values: &establish.values,
                                    transaction: Some(transaction),
                                },
                            );
                        }

                        TransactionStep::EstablishTransactionOutput(establish) => {
                            bindings.insert(
                                &establish.bind,
                                BindingSite::Output {
                                    values: &establish.values,
                                    transaction,
                                },
                            );
                        }

                        TransactionStep::Transition(transition) => {
                            for (side_effect, intent) in &transition.effect_intents {
                                bindings.insert(
                                    &intent.bind,
                                    BindingSite::TransitionIntent {
                                        side_effect,
                                        values: &intent.values,
                                        transaction,
                                    },
                                );
                            }
                        }

                        _ => {}
                    }
                }
            }

            OperationStep::ExecuteEffect(execute) => {
                effects.insert(
                    &execute.effect_id,
                    EffectValues {
                        values: &execute.values,
                        transaction: None,
                    },
                );

                if let Some(bind) = &execute.bind {
                    bindings.insert(
                        bind,
                        BindingSite::EffectResult {
                            effect: &execute.effect_id,
                        },
                    );
                }
            }

            OperationStep::ExecuteEffectIntent(execute) => {
                if let Some(bind) = &execute.bind {
                    bindings.insert(
                        bind,
                        BindingSite::IntentResult {
                            intent: &execute.intent,
                        },
                    );
                }
            }

            OperationStep::ExecuteEffectAsync(execute) => {
                effects.insert(
                    &execute.effect_id,
                    EffectValues {
                        values: &execute.values,
                        transaction: None,
                    },
                );
            }

            OperationStep::JoinAll(join) => {
                for entry in &join.handles {
                    let (Some(bind), Some(site)) = (&entry.bind, handles.get(&entry.handle))
                    else {
                        continue;
                    };

                    bindings.insert(bind, site.binding());
                }
            }

            OperationStep::Race(race) => {
                // The race result's possible producers are the whole
                // candidate set; provenance walks through the first
                // resolvable candidate, whose contract every candidate
                // is validated to share.
                let (Some(bind), Some(site)) = (
                    &race.bind,
                    race.handles.iter().find_map(|handle| handles.get(handle)),
                ) else {
                    continue;
                };

                bindings.insert(bind, site.binding());
            }

            _ => {}
        }
    }

    (ProgramIndex { bindings }, effects)
}

/// The launch an async handle refers back to.
#[derive(Clone, Copy)]
enum AsyncHandleSite<'a> {
    Direct { effect: &'a Id },
    Intent { intent: &'a Id },
}

impl<'a> AsyncHandleSite<'a> {
    fn binding(self) -> BindingSite<'a> {
        match self {
            Self::Direct { effect } => BindingSite::EffectResult { effect },
            Self::Intent { intent } => BindingSite::IntentResult { intent },
        }
    }
}

fn provenance_roots(workspace: &WorkspaceState, operation: &Id, binding: &Id) -> Vec<ProvenanceRoot> {
    let Some(program) = workspace
        .operations
        .get(operation)
        .and_then(|draft| draft.program.as_ref())
    else {
        return vec![ProvenanceRoot::Unspecified {
            at: format!("operation {operation} has no program"),
        }];
    };

    let (index, effects) = index_program(program);

    let mut walker = ProvenanceWalker {
        index: &index,
        effects: &effects,
        visited: FxHashSet::default(),
        roots: Vec::new(),
    };

    walker.binding(binding, &FieldPath(Vec::new()));

    walker.roots
}

struct ProvenanceWalker<'a, 'p> {
    index: &'p ProgramIndex<'a>,
    effects: &'p FxHashMap<&'a Id, EffectValues<'a>>,
    visited: FxHashSet<String>,
    roots: Vec<ProvenanceRoot>,
}

impl ProvenanceWalker<'_, '_> {
    fn binding(&mut self, binding: &Id, path: &FieldPath) {
        if !self.visited.insert(format!("binding:{binding}")) {
            return;
        }

        match self.index.bindings.get(binding) {
            None => self.roots.push(ProvenanceRoot::Unspecified {
                at: format!("unresolved binding {binding}"),
            }),

            Some(BindingSite::EffectResult { effect }) => {
                self.roots.push(ProvenanceRoot::EffectResult {
                    effect: (*effect).clone(),
                    variant: None,
                    path: path.clone(),
                });
            }

            Some(BindingSite::IntentResult { intent }) => {
                // The result comes from the underlying effect the
                // intent captured.
                match self.index.bindings.get(*intent) {
                    Some(BindingSite::Intent { effect, .. }) => {
                        self.roots.push(ProvenanceRoot::EffectResult {
                            effect: (*effect).clone(),
                            variant: None,
                            path: path.clone(),
                        });
                    }

                    Some(BindingSite::TransitionIntent { side_effect, .. }) => {
                        self.roots.push(ProvenanceRoot::EffectResult {
                            effect: (*side_effect).clone(),
                            variant: None,
                            path: path.clone(),
                        });
                    }

                    _ => self.roots.push(ProvenanceRoot::Unspecified {
                        at: format!("unresolved intent {intent}"),
                    }),
                }
            }

            Some(BindingSite::Intent {
                values,
                transaction,
                effect,
            }) => {
                let at = format!("intent established at effect {effect}");

                self.derivation(values, Some(transaction), &at);
            }

            Some(BindingSite::TransitionIntent {
                values,
                transaction,
                side_effect,
            }) => {
                let at = format!("transition intent for side effect {side_effect}");

                self.derivation(values, Some(transaction), &at);
            }

            Some(BindingSite::Output {
                values,
                transaction,
            }) => {
                let at = format!("transaction output of {}", transaction.id);

                self.derivation(values, Some(transaction), &at);
            }
        }
    }

    fn derivation(&mut self, values: &Derivation, transaction: Option<&Transaction>, at: &str) {
        match values {
            Derivation::Unspecified => self.roots.push(ProvenanceRoot::Unspecified {
                at: at.to_string(),
            }),

            Derivation::Deterministic { from } => {
                for root in from {
                    self.value_ref(root, transaction);
                }
            }
        }
    }

    fn value_ref(&mut self, reference: &ValueRef, transaction: Option<&Transaction>) {
        match &reference.source {
            ValueSource::Input(input) => self.roots.push(ProvenanceRoot::Input {
                input: input.clone(),
                path: reference.path.clone(),
            }),

            ValueSource::StateMachineSubject(machine) => {
                self.roots.push(ProvenanceRoot::StateMachineSubject {
                    machine: machine.clone(),
                    path: reference.path.clone(),
                })
            }

            ValueSource::TransactionRead(bind) => {
                let resolved = transaction.and_then(|transaction| {
                    transaction.steps.iter().find_map(|step| match step {
                        TransactionStep::Read(read) if &read.bind == bind => Some((
                            transaction.id.clone(),
                            read.target.object.clone(),
                        )),
                        _ => None,
                    })
                });

                match resolved {
                    Some((transaction, object)) => self.roots.push(ProvenanceRoot::ObjectRead {
                        transaction,
                        object,
                        path: reference.path.clone(),
                    }),

                    None => self.roots.push(ProvenanceRoot::Unspecified {
                        at: format!("unresolved transaction read {bind}"),
                    }),
                }
            }

            ValueSource::TransactionOutput(binding) => {
                self.binding(binding, &reference.path);
            }

            ValueSource::Effect(effect) => {
                if !self.visited.insert(format!("effect:{effect}")) {
                    return;
                }

                match self.effects.get(effect) {
                    Some(site) => {
                        let at = format!("values of effect {effect}");

                        self.derivation(site.values, site.transaction, &at);
                    }

                    None => self.roots.push(ProvenanceRoot::Unspecified {
                        at: format!("unresolved effect {effect}"),
                    }),
                }
            }

            ValueSource::EffectResultOk(binding) => self.result_root(binding, reference, ResultVariant::Ok),

            ValueSource::EffectResultErr(binding) => {
                self.result_root(binding, reference, ResultVariant::Err)
            }
        }
    }

    fn result_root(&mut self, binding: &Id, reference: &ValueRef, variant: ResultVariant) {
        let effect = match self.index.bindings.get(binding) {
            Some(BindingSite::EffectResult { effect }) => Some((*effect).clone()),

            Some(BindingSite::IntentResult { intent }) => match self.index.bindings.get(*intent) {
                Some(BindingSite::Intent { effect, .. }) => Some((*effect).clone()),
                Some(BindingSite::TransitionIntent { side_effect, .. }) => {
                    Some((*side_effect).clone())
                }
                _ => None,
            },

            _ => None,
        };

        match effect {
            Some(effect) => self.roots.push(ProvenanceRoot::EffectResult {
                effect,
                variant: Some(variant),
                path: reference.path.clone(),
            }),

            None => self.roots.push(ProvenanceRoot::Unspecified {
                at: format!("unresolved result binding {binding}"),
            }),
        }
    }
}
