//! Full semantic-graph construction from one workspace revision.
//!
//! The whole graph is rebuilt after every accepted commit (§14 of the
//! confluence spec): architecture graphs are small, traversal is
//! cheap, and a full rebuild cannot hold stale-index bugs. The
//! entry point is [`build`]; an incremental builder can replace the
//! implementation later without changing callers.
//!
//! Construction is total over drafts: duplicate IDs and references to
//! symbols that are planned but not yet declared never panic. A
//! dangling reference simply produces no graph edge — the id-keyed
//! indexes still record it — and structural coherence remains the
//! validator's judgment, not the graph's.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::spec::{
    Effect, Id, MessageSelector, OperationStep, Schema, StateMachineSubject, Transaction,
    TransactionStep, TransitionSideEffect, TypeRef, ValueRef, ValueSource,
};

use super::fingerprint::SemanticHash;
use super::graph::{
    CallEdge, ConsumerRef, Edge, EdgeKind, EffectRef, FieldAccess, GraphIndexes, NodeId,
    ObjectAccess, ObjectPathKey, PublisherRef, SymbolGraph, SymbolNode, TransactionRef,
    TransitionKey,
};
use super::symbol::{RequirementFamily, SymbolKey, SymbolVersion};
use super::workspace::{DraftOperation, WorkspaceState};

/// Builds the semantic graph for `workspace`, carrying symbol versions
/// forward from `previous` per §10.1: an unchanged fingerprint retains
/// its version, a changed one increments it, a new symbol starts at 1.
pub fn build(workspace: &WorkspaceState, previous: Option<&SymbolGraph>) -> SymbolGraph {
    let mut builder = Builder {
        workspace,
        nodes: Vec::new(),
        node_ids: FxHashMap::default(),
        edges: Vec::new(),
        indexes: GraphIndexes::default(),
    };

    builder.add_shared_nodes();

    let facts: Vec<(&Id, OperationFacts<'_>)> = workspace
        .operations
        .iter()
        .map(|(id, draft)| (id, collect_operation(id, draft, &mut builder)))
        .collect();

    builder.add_shared_edges();

    for (id, facts) in &facts {
        builder.add_operation_edges(id, &workspace.operations[*id], facts);
    }

    builder.finish(previous)
}

/// Everything one pass over an operation draft yields: the program's
/// derived symbols and the relationships they carry.
struct OperationFacts<'a> {
    transactions: Vec<&'a Transaction>,
    effect_sites: Vec<EffectSiteFacts<'a>>,
    bindings: Vec<BindingFacts<'a>>,

    /// Intent bindings executed by `ExecuteEffectIntent` steps.
    intent_uses: Vec<&'a Id>,
}

struct EffectSiteFacts<'a> {
    effect_id: &'a Id,
    effect: &'a Effect,
}

struct BindingFacts<'a> {
    binding: &'a Id,
    fingerprint: SemanticHash,
    producer: BindingProducer<'a>,
}

/// Where an operation-visible binding comes from, for `ProducesBinding`
/// edge attribution.
enum BindingProducer<'a> {
    /// The synchronous result of a directly executed effect.
    EffectExecution { effect_id: &'a Id },

    /// The synchronous result of executing an established intent; the
    /// producing site is resolved through the intent binding.
    IntentExecution { intent: &'a Id },

    /// An intent artifact established inline by a transaction.
    EstablishedIntent { effect_id: &'a Id },

    /// A typed value a transaction exported into operation control.
    TransactionOutput { transaction: &'a Id },

    /// An intent artifact established by applying a state-machine
    /// transition.
    TransitionIntent { transaction: &'a Id },
}

fn collect_operation<'w>(
    operation: &Id,
    draft: &'w DraftOperation,
    builder: &mut Builder<'w>,
) -> OperationFacts<'w> {
    let mut facts = OperationFacts {
        transactions: Vec::new(),
        effect_sites: Vec::new(),
        bindings: Vec::new(),
        intent_uses: Vec::new(),
    };

    if let Some(program) = &draft.program {
        for (_, step) in program.steps_with_locations() {
            match step {
                OperationStep::Transaction(transaction) => {
                    facts.transactions.push(transaction);

                    for inner in &transaction.steps {
                        match inner {
                            TransactionStep::EstablishEffectIntent(establish) => {
                                facts.effect_sites.push(EffectSiteFacts {
                                    effect_id: &establish.effect_id,
                                    effect: &establish.effect,
                                });

                                facts.bindings.push(BindingFacts {
                                    binding: &establish.bind,
                                    fingerprint: SemanticHash::of(&("effect_intent", establish)),
                                    producer: BindingProducer::EstablishedIntent {
                                        effect_id: &establish.effect_id,
                                    },
                                });
                            }

                            TransactionStep::EstablishTransactionOutput(establish) => {
                                facts.bindings.push(BindingFacts {
                                    binding: &establish.bind,
                                    fingerprint: SemanticHash::of(&(
                                        "transaction_output",
                                        establish,
                                    )),
                                    producer: BindingProducer::TransactionOutput {
                                        transaction: &transaction.id,
                                    },
                                });
                            }

                            TransactionStep::Transition(transition) => {
                                for (side_effect, intent) in &transition.effect_intents {
                                    facts.bindings.push(BindingFacts {
                                        binding: &intent.bind,
                                        fingerprint: SemanticHash::of(&(
                                            "transition_effect_intent",
                                            &transition.machine,
                                            &transition.transition,
                                            side_effect,
                                            intent,
                                        )),
                                        producer: BindingProducer::TransitionIntent {
                                            transaction: &transaction.id,
                                        },
                                    });
                                }
                            }

                            _ => {}
                        }
                    }
                }

                OperationStep::ExecuteEffect(execute) => {
                    facts.effect_sites.push(EffectSiteFacts {
                        effect_id: &execute.effect_id,
                        effect: &execute.effect,
                    });

                    if let Some(bind) = &execute.bind {
                        facts.bindings.push(BindingFacts {
                            binding: bind,
                            fingerprint: SemanticHash::of(&("effect_result", execute)),
                            producer: BindingProducer::EffectExecution {
                                effect_id: &execute.effect_id,
                            },
                        });
                    }
                }

                OperationStep::ExecuteEffectIntent(execute) => {
                    facts.intent_uses.push(&execute.intent);

                    if let Some(bind) = &execute.bind {
                        facts.bindings.push(BindingFacts {
                            binding: bind,
                            fingerprint: SemanticHash::of(&("effect_result_via_intent", execute)),
                            producer: BindingProducer::IntentExecution {
                                intent: &execute.intent,
                            },
                        });
                    }
                }

                _ => {}
            }
        }
    }

    builder.add_operation_nodes(operation, draft, &facts);

    facts
}

struct Builder<'w> {
    workspace: &'w WorkspaceState,
    nodes: Vec<SymbolNode>,
    node_ids: FxHashMap<SymbolKey, NodeId>,
    edges: Vec<(NodeId, EdgeKind, NodeId)>,
    indexes: GraphIndexes,
}

impl<'w> Builder<'w> {
    /// Creates the node for `key` unless one exists. Duplicate IDs in
    /// a draft keep the first declaration's node; the duplication is
    /// the validator's diagnostic, not the graph's.
    fn add_node(&mut self, key: SymbolKey, fingerprint: SemanticHash) -> NodeId {
        if let Some(existing) = self.node_ids.get(&key) {
            return *existing;
        }

        let id = NodeId(self.nodes.len() as u32);
        let kind = key.kind();
        let owner = key.owner();

        self.nodes.push(SymbolNode {
            key: key.clone(),
            version: SymbolVersion::first(),
            fingerprint,
            kind,
            owner,
        });

        self.node_ids.insert(key, id);

        id
    }

    /// Adds an edge when the target symbol exists; a dangling
    /// reference produces none.
    fn link(&mut self, from: NodeId, kind: EdgeKind, to: &SymbolKey) {
        if let Some(to) = self.node_ids.get(to) {
            self.edges.push((from, kind, *to));
        }
    }

    fn add_shared_nodes(&mut self) {
        for (id, service) in &self.workspace.services {
            self.add_node(SymbolKey::Service(id.clone()), SemanticHash::of(service));
        }

        for (id, schema) in &self.workspace.schemas {
            self.add_node(SymbolKey::Schema(id.clone()), SemanticHash::of(schema));
        }

        for (id, data_model) in &self.workspace.data_models {
            self.add_node(
                SymbolKey::DataModel(id.clone()),
                SemanticHash::of(data_model),
            );

            for (object_id, object) in &data_model.objects {
                self.add_node(
                    SymbolKey::DataObject {
                        data_model: id.clone(),
                        object: object_id.clone(),
                    },
                    SemanticHash::of(object),
                );
            }
        }

        for (id, topic) in &self.workspace.topics {
            self.add_node(SymbolKey::Topic(id.clone()), SemanticHash::of(topic));
        }

        for (id, machine) in &self.workspace.state_machines {
            self.add_node(
                SymbolKey::StateMachine(id.clone()),
                SemanticHash::of(machine),
            );

            for (transition_id, transition) in &machine.transitions {
                self.add_node(
                    SymbolKey::Transition {
                        machine: id.clone(),
                        transition: transition_id.clone(),
                    },
                    SemanticHash::of(transition),
                );
            }
        }

        for (id, obligation) in &self.workspace.prompt_obligations {
            self.add_node(
                SymbolKey::PromptObligation(id.clone()),
                SemanticHash::of(obligation),
            );
        }
    }

    fn add_operation_nodes(
        &mut self,
        operation: &Id,
        draft: &DraftOperation,
        facts: &OperationFacts<'_>,
    ) {
        self.add_node(SymbolKey::Operation(operation.clone()), SemanticHash::of(draft));

        self.add_node(
            SymbolKey::OperationInterface(operation.clone()),
            SemanticHash::of(&draft.interface()),
        );

        for (input_id, input) in &draft.inputs {
            self.add_node(
                SymbolKey::Input {
                    operation: operation.clone(),
                    input: input_id.clone(),
                },
                SemanticHash::of(input),
            );
        }

        self.add_node(
            SymbolKey::OperationProgram(operation.clone()),
            SemanticHash::of(&draft.program),
        );

        for transaction in &facts.transactions {
            self.add_node(
                SymbolKey::Transaction {
                    operation: operation.clone(),
                    transaction: transaction.id.clone(),
                },
                SemanticHash::of(transaction),
            );
        }

        for site in &facts.effect_sites {
            self.add_node(
                SymbolKey::EffectSite {
                    operation: operation.clone(),
                    effect: site.effect_id.clone(),
                },
                SemanticHash::of(&(site.effect_id, site.effect)),
            );
        }

        for binding in &facts.bindings {
            self.add_node(
                SymbolKey::Binding {
                    operation: operation.clone(),
                    binding: binding.binding.clone(),
                },
                binding.fingerprint,
            );
        }

        self.add_node(
            SymbolKey::OperationRequirements(operation.clone()),
            SemanticHash::of(&draft.requirements),
        );

        for (family, fingerprint, occurrence, _) in requirement_entries(draft) {
            self.add_node(
                SymbolKey::Requirement {
                    operation: operation.clone(),
                    family,
                    fingerprint,
                    occurrence,
                },
                fingerprint,
            );
        }

        self.add_node(
            SymbolKey::OperationExecution(operation.clone()),
            SemanticHash::of(&draft.execution),
        );
    }

    fn add_shared_edges(&mut self) {
        for (id, schema) in &self.workspace.schemas {
            let from = self.node_ids[&SymbolKey::Schema(id.clone())];

            match schema {
                Schema::Canonical(canonical) => {
                    let mut referenced = Vec::new();

                    for field in canonical.fields.values() {
                        collect_schema_refs(&field.ty, &mut referenced);
                    }

                    for target in referenced {
                        self.link(from, EdgeKind::References, &SymbolKey::Schema(target.clone()));
                    }
                }

                Schema::Fragment(fragment) => {
                    self.link(
                        from,
                        EdgeKind::References,
                        &SymbolKey::Schema(fragment.source.clone()),
                    );
                }
            }
        }

        for (id, data_model) in &self.workspace.data_models {
            let from = self.node_ids[&SymbolKey::DataModel(id.clone())];

            for (object_id, object) in &data_model.objects {
                let object_key = SymbolKey::DataObject {
                    data_model: id.clone(),
                    object: object_id.clone(),
                };

                self.link(from, EdgeKind::Contains, &object_key);

                let object_node = self.node_ids[&object_key];

                self.link(
                    object_node,
                    EdgeKind::References,
                    &SymbolKey::Schema(object.schema.clone()),
                );
            }
        }

        for (id, topic) in &self.workspace.topics {
            let from = self.node_ids[&SymbolKey::Topic(id.clone())];

            for message in &topic.messages {
                self.link(from, EdgeKind::References, &SymbolKey::Schema(message.clone()));
            }
        }

        for (id, machine) in &self.workspace.state_machines {
            let from = self.node_ids[&SymbolKey::StateMachine(id.clone())];

            let StateMachineSubject::Object { object, .. } = &machine.subject;

            for location in self.object_locations(object) {
                self.link(from, EdgeKind::References, &location);
            }

            for (transition_id, transition) in &machine.transitions {
                let transition_key = SymbolKey::Transition {
                    machine: id.clone(),
                    transition: transition_id.clone(),
                };

                self.link(from, EdgeKind::Contains, &transition_key);

                let transition_node = self.node_ids[&transition_key];

                for (effect_id, side_effect) in &transition.side_effects {
                    let site = EffectRef::Transition {
                        machine: id.clone(),
                        transition: transition_id.clone(),
                        effect: effect_id.clone(),
                    };

                    match side_effect {
                        TransitionSideEffect::Publication(publication) => {
                            self.link(
                                transition_node,
                                EdgeKind::PublishesTopic,
                                &SymbolKey::Topic(publication.topic.clone()),
                            );

                            self.link(
                                transition_node,
                                EdgeKind::References,
                                &SymbolKey::Schema(publication.schema.clone()),
                            );

                            if !publication.idempotency_key_propagation.is_empty() {
                                self.link(
                                    transition_node,
                                    EdgeKind::PropagatesIdempotencyKey,
                                    &SymbolKey::Topic(publication.topic.clone()),
                                );
                            }

                            self.indexes
                                .topic_publishers
                                .entry(publication.topic.clone())
                                .or_default()
                                .push(PublisherRef {
                                    site: site.clone(),
                                    schema: publication.schema.clone(),
                                });
                        }

                        TransitionSideEffect::Request(request) => {
                            self.link(
                                transition_node,
                                EdgeKind::CallsOperation,
                                &SymbolKey::Operation(request.target.operation.clone()),
                            );

                            self.link(
                                transition_node,
                                EdgeKind::References,
                                &SymbolKey::Schema(request.schema.clone()),
                            );

                            if !request.idempotency_key_propagation.is_empty() {
                                self.link(
                                    transition_node,
                                    EdgeKind::PropagatesIdempotencyKey,
                                    &SymbolKey::Operation(request.target.operation.clone()),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn add_operation_edges(
        &mut self,
        operation: &Id,
        draft: &DraftOperation,
        facts: &OperationFacts<'_>,
    ) {
        let operation_node = self.node_ids[&SymbolKey::Operation(operation.clone())];
        let interface_node = self.node_ids[&SymbolKey::OperationInterface(operation.clone())];
        let program_node = self.node_ids[&SymbolKey::OperationProgram(operation.clone())];
        let requirements_node =
            self.node_ids[&SymbolKey::OperationRequirements(operation.clone())];

        for part in [
            SymbolKey::OperationInterface(operation.clone()),
            SymbolKey::OperationProgram(operation.clone()),
            SymbolKey::OperationRequirements(operation.clone()),
            SymbolKey::OperationExecution(operation.clone()),
        ] {
            self.link(operation_node, EdgeKind::Contains, &part);
        }

        self.add_interface_edges(operation, draft, operation_node, interface_node);
        self.add_program_edges(operation, facts, program_node);
        self.add_requirement_edges(operation, draft, operation_node, requirements_node);
    }

    fn add_interface_edges(
        &mut self,
        operation: &Id,
        draft: &DraftOperation,
        operation_node: NodeId,
        interface_node: NodeId,
    ) {
        for (input_id, input) in &draft.inputs {
            let input_key = SymbolKey::Input {
                operation: operation.clone(),
                input: input_id.clone(),
            };

            self.link(interface_node, EdgeKind::Contains, &input_key);

            let input_node = self.node_ids[&input_key];

            match input {
                crate::spec::Input::Request(request) => {
                    for schema in [
                        &request.schema,
                        &request.result.ok,
                        &request.result.err.schema,
                    ] {
                        self.link(
                            interface_node,
                            EdgeKind::ContractDependsOn,
                            &SymbolKey::Schema(schema.clone()),
                        );
                    }
                }

                crate::spec::Input::Subscription(subscription) => {
                    let topic_key = SymbolKey::Topic(subscription.topic.clone());

                    self.link(input_node, EdgeKind::ConsumesTopic, &topic_key);
                    self.link(operation_node, EdgeKind::TriggeredBy, &topic_key);

                    self.indexes
                        .topic_consumers
                        .entry(subscription.topic.clone())
                        .or_default()
                        .push(ConsumerRef {
                            operation: operation.clone(),
                            input: input_id.clone(),
                        });

                    let selected: Vec<Id> = match &subscription.messages {
                        MessageSelector::Only(schemas) => schemas.iter().cloned().collect(),
                        MessageSelector::All => self
                            .workspace
                            .topics
                            .get(&subscription.topic)
                            .map(|topic| topic.messages.iter().cloned().collect())
                            .unwrap_or_default(),
                    };

                    for schema in selected {
                        self.link(
                            interface_node,
                            EdgeKind::ContractDependsOn,
                            &SymbolKey::Schema(schema),
                        );
                    }
                }
            }
        }
    }

    fn add_program_edges(
        &mut self,
        operation: &Id,
        facts: &OperationFacts<'_>,
        program_node: NodeId,
    ) {
        for transaction in &facts.transactions {
            let transaction_key = SymbolKey::Transaction {
                operation: operation.clone(),
                transaction: transaction.id.clone(),
            };

            self.link(program_node, EdgeKind::Contains, &transaction_key);

            let transaction_node = self.node_ids[&transaction_key];
            let transaction_ref = TransactionRef {
                operation: operation.clone(),
                transaction: transaction.id.clone(),
            };

            self.add_transaction_edges(transaction, transaction_node, &transaction_ref);
        }

        for site in &facts.effect_sites {
            let site_key = SymbolKey::EffectSite {
                operation: operation.clone(),
                effect: site.effect_id.clone(),
            };

            self.link(program_node, EdgeKind::Contains, &site_key);

            let site_node = self.node_ids[&site_key];
            let site_ref = EffectRef::Operation {
                operation: operation.clone(),
                effect: site.effect_id.clone(),
            };

            match site.effect {
                Effect::Publication(publication) => {
                    let topic_key = SymbolKey::Topic(publication.topic.clone());

                    self.link(site_node, EdgeKind::PublishesTopic, &topic_key);
                    self.link(
                        site_node,
                        EdgeKind::References,
                        &SymbolKey::Schema(publication.schema.clone()),
                    );

                    if !publication.idempotency_key_propagation.is_empty() {
                        self.link(site_node, EdgeKind::PropagatesIdempotencyKey, &topic_key);
                    }

                    self.indexes
                        .topic_publishers
                        .entry(publication.topic.clone())
                        .or_default()
                        .push(PublisherRef {
                            site: site_ref.clone(),
                            schema: publication.schema.clone(),
                        });
                }

                Effect::Request(request) => {
                    let target_key = SymbolKey::Operation(request.target.operation.clone());

                    self.link(site_node, EdgeKind::CallsOperation, &target_key);
                    self.link(
                        site_node,
                        EdgeKind::References,
                        &SymbolKey::Schema(request.schema.clone()),
                    );

                    if !request.idempotency_key_propagation.is_empty() {
                        self.link(site_node, EdgeKind::PropagatesIdempotencyKey, &target_key);
                    }

                    let call = CallEdge {
                        caller: operation.clone(),
                        target: request.target.operation.clone(),
                        target_input: request.target.input.clone(),
                        site: site_ref.clone(),
                    };

                    self.indexes
                        .callers
                        .entry(call.target.clone())
                        .or_default()
                        .push(call.clone());

                    self.indexes
                        .callees
                        .entry(call.caller.clone())
                        .or_default()
                        .push(call);
                }

                Effect::External(external) => {
                    if let Some(result) = &external.result {
                        for schema in [&result.ok, &result.err.schema] {
                            self.link(
                                site_node,
                                EdgeKind::References,
                                &SymbolKey::Schema(schema.clone()),
                            );
                        }
                    }
                }
            }
        }

        for binding in &facts.bindings {
            let binding_key = SymbolKey::Binding {
                operation: operation.clone(),
                binding: binding.binding.clone(),
            };

            self.link(program_node, EdgeKind::Contains, &binding_key);

            let producer_key = match &binding.producer {
                BindingProducer::EffectExecution { effect_id } => Some(SymbolKey::EffectSite {
                    operation: operation.clone(),
                    effect: (*effect_id).clone(),
                }),

                BindingProducer::EstablishedIntent { effect_id, .. } => {
                    Some(SymbolKey::EffectSite {
                        operation: operation.clone(),
                        effect: (*effect_id).clone(),
                    })
                }

                BindingProducer::TransactionOutput { transaction }
                | BindingProducer::TransitionIntent { transaction } => {
                    Some(SymbolKey::Transaction {
                        operation: operation.clone(),
                        transaction: (*transaction).clone(),
                    })
                }

                BindingProducer::IntentExecution { intent } => facts
                    .bindings
                    .iter()
                    .find(|candidate| candidate.binding == *intent)
                    .and_then(|intent_binding| match &intent_binding.producer {
                        BindingProducer::EstablishedIntent { effect_id, .. } => {
                            Some(SymbolKey::EffectSite {
                                operation: operation.clone(),
                                effect: (*effect_id).clone(),
                            })
                        }

                        BindingProducer::TransitionIntent { transaction } => {
                            Some(SymbolKey::Transaction {
                                operation: operation.clone(),
                                transaction: (*transaction).clone(),
                            })
                        }

                        _ => None,
                    }),
            };

            if let Some(producer_key) = producer_key
                && let Some(producer) = self.node_ids.get(&producer_key).copied()
            {
                self.edges.push((
                    producer,
                    EdgeKind::ProducesBinding,
                    self.node_ids[&binding_key],
                ));
            }
        }

        for intent in &facts.intent_uses {
            self.link(
                program_node,
                EdgeKind::UsesBinding,
                &SymbolKey::Binding {
                    operation: operation.clone(),
                    binding: (*intent).clone(),
                },
            );
        }
    }

    fn add_transaction_edges(
        &mut self,
        transaction: &Transaction,
        transaction_node: NodeId,
        transaction_ref: &TransactionRef,
    ) {
        for step in &transaction.steps {
            match step {
                TransactionStep::Read(read) => {
                    self.record_object_access(
                        transaction.data_model.as_ref(),
                        &read.target.object,
                        transaction_node,
                        transaction_ref,
                        EdgeKind::ReadsObject,
                        match &read.fields {
                            crate::spec::FieldSelection::All => FieldAccess::All,
                            crate::spec::FieldSelection::Only(fields) => {
                                FieldAccess::Fields(fields.iter().cloned().collect())
                            }
                        },
                    );
                }

                TransactionStep::Write(write) => {
                    self.record_object_access(
                        transaction.data_model.as_ref(),
                        &write.target.object,
                        transaction_node,
                        transaction_ref,
                        EdgeKind::WritesObject,
                        FieldAccess::Fields(write.fields.iter().cloned().collect()),
                    );
                }

                TransactionStep::Insert(insert) => {
                    self.record_object_access(
                        transaction.data_model.as_ref(),
                        &insert.object,
                        transaction_node,
                        transaction_ref,
                        EdgeKind::WritesObject,
                        FieldAccess::All,
                    );
                }

                TransactionStep::Delete(delete) => {
                    self.record_object_access(
                        transaction.data_model.as_ref(),
                        &delete.target.object,
                        transaction_node,
                        transaction_ref,
                        EdgeKind::WritesObject,
                        FieldAccess::All,
                    );
                }

                // A lock constrains scheduling; it neither observes nor
                // changes object state, so it contributes no access.
                TransactionStep::Lock(_) => {}

                TransactionStep::Transition(transition) => {
                    let machine_key = SymbolKey::StateMachine(transition.machine.clone());
                    let transition_key = SymbolKey::Transition {
                        machine: transition.machine.clone(),
                        transition: transition.transition.clone(),
                    };

                    self.link(transaction_node, EdgeKind::AppliesStateMachine, &machine_key);
                    self.link(transaction_node, EdgeKind::AppliesTransition, &transition_key);

                    self.indexes
                        .transition_users
                        .entry(TransitionKey {
                            machine: transition.machine.clone(),
                            transition: transition.transition.clone(),
                        })
                        .or_default()
                        .push(transaction_ref.clone());

                    // Applying the transition writes the subject's
                    // state field.
                    let state_field = self
                        .workspace
                        .state_machines
                        .get(&transition.machine)
                        .map(|machine| {
                            let StateMachineSubject::Object { state, .. } = &machine.subject;

                            state.clone()
                        });

                    self.record_object_access(
                        transaction.data_model.as_ref(),
                        &transition.subject.object,
                        transaction_node,
                        transaction_ref,
                        EdgeKind::WritesObject,
                        match state_field {
                            Some(field) => FieldAccess::Fields(vec![field]),
                            None => FieldAccess::All,
                        },
                    );
                }

                TransactionStep::EstablishEffectIntent(_)
                | TransactionStep::EstablishTransactionOutput(_) => {}
            }
        }
    }

    fn record_object_access(
        &mut self,
        data_model: Option<&Id>,
        object: &Id,
        transaction_node: NodeId,
        transaction_ref: &TransactionRef,
        kind: EdgeKind,
        access: FieldAccess,
    ) {
        let Some(data_model) = data_model else {
            return;
        };

        let object_key = SymbolKey::DataObject {
            data_model: data_model.clone(),
            object: object.clone(),
        };

        self.link(transaction_node, kind, &object_key);

        let index = match kind {
            EdgeKind::ReadsObject => &mut self.indexes.object_readers,
            EdgeKind::WritesObject => &mut self.indexes.object_writers,
            _ => unreachable!("object accesses are reads or writes"),
        };

        index
            .entry(ObjectPathKey {
                data_model: data_model.clone(),
                object: object.clone(),
            })
            .or_default()
            .push(ObjectAccess {
                transaction: transaction_ref.clone(),
                access,
            });
    }

    fn add_requirement_edges(
        &mut self,
        operation: &Id,
        draft: &DraftOperation,
        operation_node: NodeId,
        requirements_node: NodeId,
    ) {
        for (family, fingerprint, occurrence, roots) in requirement_entries(draft) {
            let requirement_key = SymbolKey::Requirement {
                operation: operation.clone(),
                family,
                fingerprint,
                occurrence,
            };

            self.link(requirements_node, EdgeKind::Contains, &requirement_key);

            let requirement_node = self.node_ids[&requirement_key];

            self.edges
                .push((requirement_node, EdgeKind::RequirementTargets, operation_node));

            for root in roots {
                if let ValueSource::Input(input) = &root.source {
                    self.link(
                        requirement_node,
                        EdgeKind::References,
                        &SymbolKey::Input {
                            operation: operation.clone(),
                            input: input.clone(),
                        },
                    );
                }
            }
        }
    }

    /// Every `(data_model, object)` pair declaring the object id — the
    /// DSL names machine subjects by object id alone.
    fn object_locations(&self, object: &Id) -> Vec<SymbolKey> {
        self.workspace
            .data_models
            .iter()
            .filter(|(_, data_model)| data_model.objects.contains_key(object))
            .map(|(id, _)| SymbolKey::DataObject {
                data_model: id.clone(),
                object: object.clone(),
            })
            .collect()
    }

    fn finish(mut self, previous: Option<&SymbolGraph>) -> SymbolGraph {
        if let Some(previous) = previous {
            for node in &mut self.nodes {
                if let Some(before) = previous.node(&node.key) {
                    node.version = if before.fingerprint == node.fingerprint {
                        before.version
                    } else {
                        before.version.next()
                    };
                }
            }
        }

        let mut outgoing: Vec<SmallVec<[Edge; 4]>> = vec![SmallVec::new(); self.nodes.len()];
        let mut incoming: Vec<SmallVec<[Edge; 4]>> = vec![SmallVec::new(); self.nodes.len()];

        self.edges.sort_unstable_by_key(|(from, kind, to)| (*from, *kind, *to));
        self.edges.dedup();

        for (from, kind, to) in &self.edges {
            outgoing[from.index()].push(Edge {
                kind: *kind,
                to: *to,
            });

            incoming[to.index()].push(Edge {
                kind: *kind,
                to: *from,
            });
        }

        for edges in &mut incoming {
            edges.sort_unstable_by_key(|edge| (edge.kind, edge.to));
        }

        for entries in self.indexes.callers.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        for entries in self.indexes.callees.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        for entries in self.indexes.object_readers.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        for entries in self.indexes.object_writers.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        for entries in self.indexes.topic_publishers.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        for entries in self.indexes.topic_consumers.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        for entries in self.indexes.transition_users.values_mut() {
            entries.sort_unstable();
            entries.dedup();
        }

        SymbolGraph {
            revision: self.workspace.revision,
            nodes: self.nodes,
            node_ids: self.node_ids,
            outgoing,
            incoming,
            indexes: self.indexes,
        }
    }
}

/// Every declared requirement of a draft with its family, content
/// fingerprint, occurrence among identical declarations, and the value
/// roots its key rests on. Occurrences count in declaration order.
fn requirement_entries(
    draft: &DraftOperation,
) -> Vec<(RequirementFamily, SemanticHash, u32, Vec<&ValueRef>)> {
    let mut seen: FxHashMap<(RequirementFamily, SemanticHash), u32> = FxHashMap::default();
    let mut entries: Vec<(RequirementFamily, SemanticHash, u32, Vec<&ValueRef>)> = Vec::new();

    fn push<'a>(
        seen: &mut FxHashMap<(RequirementFamily, SemanticHash), u32>,
        entries: &mut Vec<(RequirementFamily, SemanticHash, u32, Vec<&'a ValueRef>)>,
        family: RequirementFamily,
        fingerprint: SemanticHash,
        roots: Vec<&'a ValueRef>,
    ) {
        let occurrence = seen.entry((family, fingerprint)).or_insert(0);

        entries.push((family, fingerprint, *occurrence, roots));

        *occurrence += 1;
    }

    for requirement in &draft.requirements.serialization {
        push(
            &mut seen,
            &mut entries,
            RequirementFamily::Serialization,
            SemanticHash::of(requirement),
            vec![&requirement.key],
        );
    }

    for requirement in &draft.requirements.ordering {
        push(
            &mut seen,
            &mut entries,
            RequirementFamily::Ordering,
            SemanticHash::of(requirement),
            vec![&requirement.key],
        );
    }

    for requirement in &draft.requirements.idempotency {
        push(
            &mut seen,
            &mut entries,
            RequirementFamily::Idempotency,
            SemanticHash::of(requirement),
            requirement.key.components.iter().collect(),
        );
    }

    for requirement in &draft.requirements.recoverability {
        push(
            &mut seen,
            &mut entries,
            RequirementFamily::Recoverability,
            SemanticHash::of(requirement),
            requirement.key.components.iter().collect(),
        );
    }

    entries
}

fn collect_schema_refs<'a>(ty: &'a TypeRef, out: &mut Vec<&'a Id>) {
    match ty {
        TypeRef::Scalar(_) => {}
        TypeRef::Schema(id) => out.push(id),
        TypeRef::List(inner) => collect_schema_refs(inner, out),
    }
}
