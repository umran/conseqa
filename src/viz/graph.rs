//! Extraction of the system-level view model from a parsed `Model`.
//!
//! The presentation layer renders two kinds of data: the raw model
//! (serialized wholesale for detail panes) and this derived graph,
//! which resolves the DSL's indirections — effect intents, transition
//! ownership, message selectors — into plain vertices and edges so the
//! front end never re-implements those semantics.
//!
//! Vertices: services (boundaries), operations, topics, external
//! systems, and a synthetic client for request inputs nothing in the
//! model invokes. Edges: publications (operation → topic),
//! subscriptions (topic → operation), requests (operation →
//! operation), and external effect executions.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::spec::{
    Effect, Id, Input, MemberConcurrency, MessageSelector, Model, OperationStep, TransactionStep,
    TransitionSideEffect,
};

pub const CLIENT_NODE_ID: &str = "@client";

#[derive(Debug, Clone, Serialize)]
pub struct Graph {
    pub services: Vec<ServiceNode>,
    pub operations: Vec<OperationNode>,
    pub topics: Vec<TopicNode>,
    pub externals: Vec<ExternalNode>,

    /// The declared L1 runtime topology, empty when the model
    /// describes only application semantics.
    pub runtime: RuntimeView,

    /// Present only when at least one request input is externally
    /// invokable.
    pub client: Option<ClientNode>,

    pub edges: Vec<Edge>,

    /// Where each effect id is declared: on an operation or on a
    /// state-machine transition.
    pub effect_owners: BTreeMap<Id, EffectOwner>,

    /// Transaction steps that take each transition, keyed by
    /// `machine/transition`.
    pub transition_refs: BTreeMap<String, Vec<TransitionRef>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceNode {
    pub id: Id,
    pub kind: String,
    pub operations: Vec<Id>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationNode {
    pub id: Id,
    pub service: Id,
    pub description: Option<String>,

    pub inputs: usize,

    /// Steps of the operation program, nested ones included.
    pub steps: usize,

    /// State machines this operation touches, via transition steps or
    /// by executing transition-owned effects.
    pub machines: Vec<Id>,

    pub requirements: RequirementBadges,
}

/// The runtime realization, as flat lists the front end can render
/// beside the application graph.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RuntimeView {
    pub execution_pools: Vec<ExecutionPoolNode>,
    pub routers: Vec<RouterNode>,
    pub storage_layouts: Vec<StorageLayoutNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionPoolNode {
    pub id: Id,
    pub member_concurrency: String,

    /// Operation inputs assigned to this pool, request and
    /// subscription alike — the shared execution population made
    /// visible.
    pub assigned: Vec<BoundaryRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoundaryRef {
    pub operation: Id,
    pub input: Id,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouterNode {
    pub id: Id,
    pub operation: Id,
    pub input: Id,
    pub pool: Id,

    /// The semantic routing key, empty when the router declares no
    /// routing at all.
    pub routing_key: Vec<String>,

    /// `None` when the router declares no routing.
    pub member_assignment: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageLayoutNode {
    pub id: Id,
    pub data_model: Id,
    pub object: Id,
    pub partition_key: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequirementBadges {
    pub serialization: usize,
    pub ordering: usize,
    pub idempotency: usize,
    pub recoverability: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopicNode {
    pub id: Id,

    /// Topic-scoped transport facts. In subscription-scoped mode both
    /// read `none` and each subscribe edge carries its own.
    pub ordering: String,
    pub grouping: String,

    /// Whether the topic declares transport semantics for all its
    /// subscriptions, or leaves each to declare its own.
    pub topic_scoped_transport: bool,

    pub messages: Vec<Id>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalNode {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientNode {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,

    #[serde(flatten)]
    pub detail: EdgeDetail,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeDetail {
    /// A publication effect available to `operation`.
    Publish {
        operation: Id,
        effect: Id,
        schema: Id,

        /// Set when the effect is owned by a state-machine transition
        /// rather than declared on the operation.
        via_transition: Option<TransitionKey>,

        /// Program steps of `operation` that execute the effect, as
        /// step locations. Empty means the capability is declared but
        /// no step of the program uses it.
        executed_at: Vec<String>,

        /// The subset of `executed_at` that launches the effect
        /// asynchronously: initiation without a completion dependency
        /// on the following step.
        async_executed_at: Vec<String>,
    },

    Subscribe {
        operation: Id,
        input: Id,

        /// Concrete message schemas, with `MessageSelector::All`
        /// resolved against the topic.
        schemas: Vec<Id>,

        delivery: String,

        /// The transport facts in force for this subscription,
        /// resolved from whichever scope declares them.
        grouping: String,
        ordering: String,

        /// The dispatch routing key, or `none` when the dispatch
        /// declares no member affinity. Absent entirely when the
        /// subscription has no declared runtime.
        routing: Option<String>,

        /// The execution pool deliveries are assigned to, when a
        /// runtime declares one.
        pool: Option<Id>,

        member_assignment: Option<String>,
    },

    /// A request effect from `operation` to another operation's
    /// request input.
    Request {
        operation: Id,
        effect: Id,
        input: Id,
        schema: Id,
        retry: String,
        via_transition: Option<TransitionKey>,
        executed_at: Vec<String>,

        /// The subset of `executed_at` that launches the effect
        /// asynchronously.
        async_executed_at: Vec<String>,
    },

    /// An external effect execution; the modeled system ends here.
    External {
        operation: Id,
        effect: Id,
        idempotency: String,
        executed_at: Vec<String>,

        /// The subset of `executed_at` that launches the effect
        /// asynchronously.
        async_executed_at: Vec<String>,
    },

    /// A request input no modeled operation invokes.
    Client {
        operation: Id,
        input: Id,
        schema: Id,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct TransitionKey {
    pub machine: Id,
    pub transition: Id,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectOwner {
    Operation { operation: Id },
    Transition { machine: Id, transition: Id },
}

#[derive(Debug, Clone, Serialize)]
pub struct TransitionRef {
    pub operation: Id,
    pub transaction: Id,
    pub step: usize,
}

pub fn extract(model: &Model) -> Graph {
    let effect_owners = collect_effect_owners(model);
    let transition_refs = collect_transition_refs(model);

    // Request inputs targeted by some request effect anywhere in the
    // model; the rest are externally invokable.
    let mut targeted_inputs: BTreeSet<(Id, Id)> = BTreeSet::new();

    for op in model.operations.values() {
        for (_, effect) in op.program.effect_declarations() {
            if let Effect::Request(request) = effect {
                targeted_inputs.insert((
                    request.target.operation.clone(),
                    request.target.input.clone(),
                ));
            }
        }
    }

    for machine in model.state_machines.values() {
        for transition in machine.transitions.values() {
            for effect in transition.side_effects.values() {
                if let TransitionSideEffect::Request(request) = effect {
                    targeted_inputs.insert((
                        request.target.operation.clone(),
                        request.target.input.clone(),
                    ));
                }
            }
        }
    }

    let mut edges = Vec::new();
    let mut external_names: BTreeSet<String> = BTreeSet::new();
    let mut client_used = false;
    let mut edge_seq = 0usize;

    let mut next_edge_id = move || {
        let id = format!("e{edge_seq}");
        edge_seq += 1;
        id
    };

    let mut operations = Vec::new();

    for (op_id, op) in &model.operations {
        let executions = collect_effect_executions(op);
        let mut machines: BTreeSet<Id> = BTreeSet::new();

        // Inputs: subscriptions become topic → operation edges;
        // request inputs nobody targets become client edges.
        for (input_id, input) in &op.inputs {
            match input {
                Input::Subscription(sub) => {
                    let schemas = match &sub.messages {
                        MessageSelector::All => model
                            .topics
                            .get(&sub.topic)
                            .map(|t| t.messages.iter().cloned().collect())
                            .unwrap_or_default(),
                        MessageSelector::Only(schemas) => schemas.iter().cloned().collect(),
                    };

                    edges.push(Edge {
                        id: next_edge_id(),
                        from: sub.topic.to_string(),
                        to: op_id.to_string(),
                        detail: {
                            let runtime = model.subscription_runtime(op_id, input_id);

                            EdgeDetail::Subscribe {
                                operation: op_id.clone(),
                                input: input_id.clone(),
                                schemas,
                                delivery: to_tag(&model.delivery(op_id, input_id)),
                                grouping: grouping_label(
                                    model
                                        .effective_grouping(op_id, input_id, &sub.topic)
                                        .as_ref(),
                                ),
                                ordering: ordering_label(model.effective_ordering(
                                    op_id,
                                    input_id,
                                    &sub.topic,
                                )),
                                routing: runtime.map(|runtime| match &runtime.dispatch.routing {
                                    Some(routing) => to_tag(&routing.key),
                                    None => "none".to_string(),
                                }),
                                pool: runtime.map(|runtime| runtime.dispatch.pool.clone()),
                                member_assignment: runtime.and_then(|runtime| {
                                    runtime
                                        .dispatch
                                        .routing
                                        .as_ref()
                                        .map(|routing| to_tag(&routing.member_assignment))
                                }),
                            }
                        },
                    });
                }

                Input::Request(request) => {
                    let key = (op_id.clone(), input_id.clone());

                    if !targeted_inputs.contains(&key) {
                        client_used = true;

                        edges.push(Edge {
                            id: next_edge_id(),
                            from: CLIENT_NODE_ID.to_string(),
                            to: op_id.to_string(),
                            detail: EdgeDetail::Client {
                                operation: op_id.clone(),
                                input: input_id.clone(),
                                schema: request.schema.clone(),
                            },
                        });
                    }
                }
            }
        }

        // Effects available to the operation: its own inline
        // declarations plus transition-owned effects reachable through
        // its transition applications.
        let mut available: Vec<(Id, ResolvedEffect, Option<TransitionKey>)> = op
            .program
            .effect_declarations()
            .into_iter()
            .map(|(id, effect)| (id.clone(), ResolvedEffect::from(effect), None))
            .collect();

        for (_, transaction) in op.program.transactions() {
            for step in &transaction.steps {
                let TransactionStep::Transition(step) = step else {
                    continue;
                };

                for effect_id in step.effect_intents.keys() {
                    if let Some(effect) =
                        transition_effect(model, &step.machine, &step.transition, effect_id)
                    {
                        machines.insert(step.machine.clone());

                        available.push((
                            effect_id.clone(),
                            effect,
                            Some(TransitionKey {
                                machine: step.machine.clone(),
                                transition: step.transition.clone(),
                            }),
                        ));
                    }
                }
            }
        }

        for (effect_id, effect, via_transition) in available {
            let EffectExecutions {
                all: executed_at,
                asynchronous: async_executed_at,
            } = executions.get(&effect_id).cloned().unwrap_or_default();

            match effect {
                ResolvedEffect::Publication(publication) => {
                    edges.push(Edge {
                        id: next_edge_id(),
                        from: op_id.to_string(),
                        to: publication.topic.to_string(),
                        detail: EdgeDetail::Publish {
                            operation: op_id.clone(),
                            effect: effect_id,
                            schema: publication.schema.clone(),
                            via_transition,
                            executed_at,
                            async_executed_at,
                        },
                    });
                }

                ResolvedEffect::Request(request) => {
                    edges.push(Edge {
                        id: next_edge_id(),
                        from: op_id.to_string(),
                        to: request.target.operation.to_string(),
                        detail: EdgeDetail::Request {
                            operation: op_id.clone(),
                            effect: effect_id,
                            input: request.target.input.clone(),
                            schema: request.schema.clone(),
                            retry: to_tag(&request.retry),
                            via_transition,
                            executed_at,
                            async_executed_at,
                        },
                    });
                }

                ResolvedEffect::External(external) => {
                    let node_id = format!("@external:{}", external.name);
                    external_names.insert(external.name.clone());

                    edges.push(Edge {
                        id: next_edge_id(),
                        from: op_id.to_string(),
                        to: node_id,
                        detail: EdgeDetail::External {
                            operation: op_id.clone(),
                            effect: effect_id,
                            idempotency: idempotency_label(&external.idempotency),
                            executed_at,
                            async_executed_at,
                        },
                    });
                }
            }
        }

        // Machines referenced by transition steps in inline
        // transactions.
        for (_, transaction) in op.program.transactions() {
            for step in &transaction.steps {
                if let TransactionStep::Transition(transition) = step {
                    machines.insert(transition.machine.clone());
                }
            }
        }

        operations.push(OperationNode {
            id: op_id.clone(),
            service: op.service.clone(),
            description: op.description.clone(),
            inputs: op.inputs.len(),
            steps: op.program.steps_with_locations().len(),
            machines: machines.into_iter().collect(),
            requirements: RequirementBadges {
                serialization: op.requirements.serialization.len(),
                ordering: op.requirements.ordering.len(),
                idempotency: op.requirements.idempotency.len(),
                recoverability: op.requirements.recoverability.len(),
            },
        });
    }

    let services = model
        .services
        .iter()
        .map(|(id, service)| ServiceNode {
            id: id.clone(),
            kind: to_tag(&service.kind),
            operations: model
                .operations
                .iter()
                .filter(|(_, op)| &op.service == id)
                .map(|(op_id, _)| op_id.clone())
                .collect(),
        })
        .collect();

    let topics = model
        .topics
        .iter()
        .map(|(id, topic)| TopicNode {
            id: id.clone(),
            ordering: model
                .topic_runtime(id)
                .and_then(|runtime| runtime.ordering)
                .map(ordering_label)
                .unwrap_or_else(|| "none".to_string()),
            grouping: model
                .topic_runtime(id)
                .map(|runtime| grouping_label(runtime.grouping.as_ref()))
                .unwrap_or_else(|| "none".to_string()),
            // Whether these facts govern every subscription of the
            // topic, or each declares its own.
            topic_scoped_transport: model.topic_scoped_transport(id),
            messages: topic.messages.iter().cloned().collect(),
        })
        .collect();

    let externals = external_names
        .into_iter()
        .map(|name| ExternalNode {
            id: format!("@external:{name}"),
            name,
        })
        .collect();

    Graph {
        services,
        operations,
        topics,
        externals,
        runtime: runtime_view(model),
        client: client_used.then(|| ClientNode {
            id: CLIENT_NODE_ID.to_string(),
        }),
        edges,
        effect_owners,
        transition_refs,
    }
}

fn collect_effect_owners(model: &Model) -> BTreeMap<Id, EffectOwner> {
    let mut owners = BTreeMap::new();

    for (op_id, op) in &model.operations {
        for (effect_id, _) in op.program.effect_declarations() {
            owners.insert(
                effect_id.clone(),
                EffectOwner::Operation {
                    operation: op_id.clone(),
                },
            );
        }
    }

    for (machine_id, machine) in &model.state_machines {
        for (transition_id, transition) in &machine.transitions {
            for effect_id in transition.side_effects.keys() {
                owners.insert(
                    effect_id.clone(),
                    EffectOwner::Transition {
                        machine: machine_id.clone(),
                        transition: transition_id.clone(),
                    },
                );
            }
        }
    }

    owners
}

fn collect_transition_refs(model: &Model) -> BTreeMap<String, Vec<TransitionRef>> {
    let mut refs: BTreeMap<String, Vec<TransitionRef>> = BTreeMap::new();

    for (op_id, op) in &model.operations {
        for (_, transaction) in op.program.transactions() {
            for (index, step) in transaction.steps.iter().enumerate() {
                if let TransactionStep::Transition(transition) = step {
                    refs.entry(format!("{}/{}", transition.machine, transition.transition))
                        .or_default()
                        .push(TransitionRef {
                            operation: op_id.clone(),
                            transaction: transaction.id.clone(),
                            step: index,
                        });
                }
            }
        }
    }

    refs
}

/// The execution sites of one effect within one operation program:
/// every step that executes it, and the subset that launches it
/// asynchronously.
#[derive(Debug, Clone, Default)]
struct EffectExecutions {
    all: Vec<String>,
    asynchronous: Vec<String>,
}

/// For one operation: effect id → program steps that execute it —
/// directly, by executing an intent binding that captured it, or by
/// launching either asynchronously.
fn collect_effect_executions(op: &crate::spec::Operation) -> BTreeMap<Id, EffectExecutions> {
    // Intent binding → the effect it captured: an inline establishment
    // site's effect_id, or a transition application's side-effect ID.
    let mut intent_effects: BTreeMap<&Id, &Id> = BTreeMap::new();

    for (_, transaction) in op.program.transactions() {
        for step in &transaction.steps {
            match step {
                TransactionStep::EstablishEffectIntent(establish) => {
                    intent_effects.insert(&establish.bind, &establish.effect_id);
                }

                TransactionStep::Transition(transition) => {
                    for (effect_id, intent) in &transition.effect_intents {
                        intent_effects.insert(&intent.bind, effect_id);
                    }
                }

                _ => {}
            }
        }
    }

    let mut executions: BTreeMap<Id, EffectExecutions> = BTreeMap::new();

    for (location, step) in op.program.steps_with_locations() {
        let (effect_id, asynchronous) = match step {
            OperationStep::ExecuteEffect(step) => (Some(step.effect_id.clone()), false),

            OperationStep::ExecuteEffectAsync(step) => (Some(step.effect_id.clone()), true),

            OperationStep::ExecuteEffectIntent(step) => (
                intent_effects.get(&step.intent).map(|id| (*id).clone()),
                false,
            ),

            OperationStep::ExecuteEffectIntentAsync(step) => (
                intent_effects.get(&step.intent).map(|id| (*id).clone()),
                true,
            ),

            _ => (None, false),
        };

        if let Some(effect_id) = effect_id {
            let entry = executions.entry(effect_id).or_default();

            entry.all.push(location.to_string());

            if asynchronous {
                entry.asynchronous.push(location.to_string());
            }
        }
    }

    executions
}

/// Uniform view over operation-declared and transition-owned effects.
#[derive(Debug, Clone, Copy)]
enum ResolvedEffect<'a> {
    Publication(&'a crate::spec::PublicationEffect),
    Request(&'a crate::spec::RequestEffect),
    External(&'a crate::spec::ExternalEffect),
}

impl<'a> From<&'a Effect> for ResolvedEffect<'a> {
    fn from(effect: &'a Effect) -> Self {
        match effect {
            Effect::Publication(publication) => Self::Publication(publication),
            Effect::Request(request) => Self::Request(request),
            Effect::External(external) => Self::External(external),
        }
    }
}

impl<'a> From<&'a TransitionSideEffect> for ResolvedEffect<'a> {
    fn from(effect: &'a TransitionSideEffect) -> Self {
        match effect {
            TransitionSideEffect::Publication(publication) => Self::Publication(publication),
            TransitionSideEffect::Request(request) => Self::Request(request),
        }
    }
}

fn transition_effect<'a>(
    model: &'a Model,
    machine: &Id,
    transition: &Id,
    effect: &Id,
) -> Option<ResolvedEffect<'a>> {
    model
        .state_machines
        .get(machine)?
        .transitions
        .get(transition)?
        .side_effects
        .get(effect)
        .map(ResolvedEffect::from)
}

fn to_tag<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(tag)) => tag,
        Ok(serde_json::Value::Object(map)) => map
            .get("kind")
            .and_then(|kind| kind.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "unspecified".to_string()),
        _ => "unspecified".to_string(),
    }
}

fn member_concurrency_label(value: MemberConcurrency) -> String {
    match value {
        MemberConcurrency::Unspecified => "unspecified".to_string(),
        MemberConcurrency::Bounded(n) => format!("bounded({n})"),
        MemberConcurrency::Unbounded => "unbounded".to_string(),
    }
}

/// The runtime topology, with each pool carrying the boundaries
/// assigned to it — the fact that two boundaries share an execution
/// population is otherwise only implicit.
fn runtime_view(model: &Model) -> RuntimeView {
    let Some(runtime) = &model.runtime else {
        return RuntimeView::default();
    };

    let mut assignments: BTreeMap<&Id, Vec<BoundaryRef>> = BTreeMap::new();

    for (operation, inputs) in &runtime.subscriptions {
        for (input, subscription) in inputs {
            assignments
                .entry(&subscription.dispatch.pool)
                .or_default()
                .push(BoundaryRef {
                    operation: operation.clone(),
                    input: input.clone(),
                });
        }
    }

    for router in runtime.routers.values() {
        assignments
            .entry(&router.pool)
            .or_default()
            .push(BoundaryRef {
                operation: router.boundary.operation.clone(),
                input: router.boundary.input.clone(),
            });
    }

    RuntimeView {
        execution_pools: runtime
            .execution_pools
            .iter()
            .map(|(id, pool)| ExecutionPoolNode {
                id: id.clone(),
                member_concurrency: member_concurrency_label(pool.member_concurrency),
                assigned: assignments.get(id).cloned().unwrap_or_default(),
            })
            .collect(),

        routers: runtime
            .routers
            .iter()
            .map(|(id, router)| RouterNode {
                id: id.clone(),
                operation: router.boundary.operation.clone(),
                input: router.boundary.input.clone(),
                pool: router.pool.clone(),
                routing_key: router
                    .routing
                    .as_ref()
                    .map(|routing| routing.key.iter().map(ToString::to_string).collect())
                    .unwrap_or_default(),
                member_assignment: router
                    .routing
                    .as_ref()
                    .map(|routing| to_tag(&routing.member_assignment)),
            })
            .collect(),

        storage_layouts: runtime
            .storage_layouts
            .iter()
            .map(|(id, layout)| StorageLayoutNode {
                id: id.clone(),
                data_model: layout.object.data_model.clone(),
                object: layout.object.object.clone(),
                partition_key: layout
                    .partition_key
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
            .collect(),
    }
}

fn idempotency_label(value: &crate::spec::IdempotencyGuarantee) -> String {
    match value {
        crate::spec::IdempotencyGuarantee::Unspecified => "unspecified".to_string(),
        crate::spec::IdempotencyGuarantee::NotDeduplicated => "not_deduplicated".to_string(),
        crate::spec::IdempotencyGuarantee::DeduplicatedBy { .. } => "deduplicated_by".to_string(),
    }
}

fn ordering_label(value: crate::spec::OrderingSemantics) -> String {
    match value {
        crate::spec::OrderingSemantics::None => "none".to_string(),
        crate::spec::OrderingSemantics::Global => "global".to_string(),
        crate::spec::OrderingSemantics::WithinGroup => "within_group".to_string(),
    }
}

fn grouping_label(value: Option<&crate::spec::GroupingKey>) -> String {
    match value {
        Some(_) => "keyed".to_string(),
        None => "none".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flash_checkout() -> Model {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/flash_checkout.yaml");

        crate::parser::yaml::parse(&std::fs::read_to_string(path).expect("fixture readable"))
            .expect("fixture parses")
    }

    fn edges_of_kind<'a>(graph: &'a Graph, kind: &str) -> Vec<&'a Edge> {
        graph
            .edges
            .iter()
            .filter(|e| serde_json::to_value(&e.detail).unwrap()["kind"] == kind)
            .collect()
    }

    #[test]
    fn extracts_expected_vertices() {
        let graph = extract(&flash_checkout());

        assert_eq!(graph.services.len(), 3);
        assert_eq!(graph.operations.len(), 6);
        assert_eq!(graph.topics.len(), 1);
        assert_eq!(graph.externals.len(), 1);
        assert_eq!(graph.externals[0].name, "payment-provider.charge");
        assert!(graph.client.is_some());
    }

    #[test]
    fn extracts_expected_edges() {
        let graph = extract(&flash_checkout());

        // Five operation-declared publications plus one transition-owned
        // publication surfaced through operation.apply_payment's intent.
        assert_eq!(edges_of_kind(&graph, "publish").len(), 6);
        assert_eq!(edges_of_kind(&graph, "subscribe").len(), 3);
        assert_eq!(edges_of_kind(&graph, "external").len(), 1);

        // No modeled operation issues requests, so all three request
        // inputs are client entry points.
        assert_eq!(edges_of_kind(&graph, "request").len(), 0);
        assert_eq!(edges_of_kind(&graph, "client").len(), 3);
    }

    #[test]
    fn transition_owned_publication_is_attributed_to_executor() {
        let graph = extract(&flash_checkout());

        let via_transition: Vec<_> = edges_of_kind(&graph, "publish")
            .into_iter()
            .filter_map(|e| match &e.detail {
                EdgeDetail::Publish {
                    operation,
                    effect,
                    via_transition: Some(key),
                    executed_at,
                    ..
                } => Some((operation, effect, key, executed_at)),
                _ => None,
            })
            .collect();

        assert_eq!(via_transition.len(), 1);
        let (operation, effect, key, executed_at) = &via_transition[0];
        assert_eq!(operation.0, "operation.apply_payment");
        assert_eq!(effect.0, "effect.order.paid");
        assert_eq!(key.machine.0, "machine.order_lifecycle");
        assert_eq!(key.transition.0, "transition.order.mark_paid");
        assert_eq!(**executed_at, vec!["2".to_string()]);
    }

    #[test]
    fn effect_owners_cover_operation_and_transition_effects() {
        let graph = extract(&flash_checkout());

        assert!(matches!(
            graph.effect_owners.get(&Id("effect.order.paid".into())),
            Some(EffectOwner::Transition { .. }),
        ));
        assert!(matches!(
            graph
                .effect_owners
                .get(&Id("effect.charge_payment.card".into())),
            Some(EffectOwner::Operation { .. }),
        ));
    }

    #[test]
    fn transition_refs_locate_transaction_steps() {
        let graph = extract(&flash_checkout());

        let refs = graph
            .transition_refs
            .get("machine.order_lifecycle/transition.order.cancel")
            .expect("cancel transition is referenced");

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].operation.0, "operation.cancel_order");
        assert_eq!(refs[0].transaction.0, "tx.cancel_order");
        assert_eq!(refs[0].step, 0);
    }

    #[test]
    fn subscriptions_resolve_message_selectors() {
        let graph = extract(&flash_checkout());

        for edge in edges_of_kind(&graph, "subscribe") {
            let EdgeDetail::Subscribe { schemas, .. } = &edge.detail else {
                unreachable!();
            };
            assert_eq!(schemas.len(), 1);
        }
    }
}
