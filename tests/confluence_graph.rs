//! Symbol-graph tests (§99 of the confluence spec): fingerprint and
//! version behavior, derived program symbols, edge derivation, reverse
//! references, and canonical deterministic query results.

use std::collections::BTreeMap;

use conseqa::confluence::{
    DraftOperation, EdgeKind, EffectRef, GraphQuery, OperationInterfaceDraft, ProvenanceRoot,
    QueryRow, RunId, RunMetadata, SymbolGraph, SymbolKey, WorkspaceState, graph_build,
    graph_query,
};
use conseqa::spec::{
    Derivation, Effect, ExecuteEffect, Field, FieldPath, Id, OperationBlock, OperationStep,
    RequestEffect, RequestTarget, RetrySemantics, ScalarType, Schema, TypeRef, ValueRef,
    ValueSource,
};

fn fixture_workspace() -> WorkspaceState {
    let source = std::fs::read_to_string("tests/fixtures/flash_checkout.yaml")
        .expect("fixture exists");

    let model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    WorkspaceState::from_model(&model, RunMetadata::new(RunId("test-run".to_string())))
}

fn id(text: &str) -> Id {
    Id(text.to_string())
}

fn path(text: &str) -> FieldPath {
    FieldPath(text.split('.').map(str::to_string).collect())
}

fn build(workspace: &WorkspaceState, previous: Option<&SymbolGraph>) -> SymbolGraph {
    graph_build::build(workspace, previous)
}

fn run(workspace: &WorkspaceState, graph: &SymbolGraph, query: GraphQuery) -> Vec<QueryRow> {
    graph_query::run(workspace, graph, &query).rows
}

/// A synthetic operation whose program requests
/// `operation.create_order` — the fixture itself has no
/// operation-to-operation request.
fn gateway_operation() -> DraftOperation {
    let mut draft = DraftOperation::planned(OperationInterfaceDraft {
        service: id("service.checkout"),
        description: Some("Calls create_order.".to_string()),
        inputs: BTreeMap::new(),
        invocation_lock: None,
    });

    draft.program = Some(OperationBlock {
        steps: vec![
            OperationStep::ExecuteEffect(ExecuteEffect {
                effect_id: id("effect.gateway.create_order"),
                effect: Effect::Request(RequestEffect {
                    target: RequestTarget {
                        operation: id("operation.create_order"),
                        input: id("input.create_order.request"),
                    },
                    schema: id("schema.CreateOrderRequest"),
                    retry: RetrySemantics::Unspecified,
                    idempotency_key_propagation: Vec::new(),
                }),
                values: Derivation::Unspecified,
                bind: None,
            }),
            OperationStep::Complete,
        ],
    });

    draft.recompute_stage();

    draft
}

#[test]
fn stable_fingerprints_retain_versions() {
    let workspace = fixture_workspace();

    let first = build(&workspace, None);
    let second = build(&workspace, Some(&first));

    assert_eq!(first.nodes.len(), second.nodes.len());

    for node in &second.nodes {
        let before = first.node(&node.key).expect("same symbols");

        assert_eq!(before.fingerprint, node.fingerprint, "{}", node.key);
        assert_eq!(before.version, node.version, "{}", node.key);
    }
}

#[test]
fn changed_content_bumps_only_the_changed_symbol() {
    let workspace = fixture_workspace();
    let first = build(&workspace, None);

    let mut changed = workspace.clone();

    let Some(Schema::Canonical(schema)) = changed.schemas.get_mut(&id("schema.OrderRecord"))
    else {
        panic!("fixture declares schema.OrderRecord as canonical");
    };

    schema.fields.insert(
        "note".to_string(),
        Field {
            ty: TypeRef::Scalar(ScalarType::String),
            optional: true,
        },
    );

    let second = build(&changed, Some(&first));

    let key = SymbolKey::Schema(id("schema.OrderRecord"));
    let before = first.node(&key).unwrap();
    let after = second.node(&key).unwrap();

    assert_ne!(before.fingerprint, after.fingerprint);
    assert_eq!(after.version.0, before.version.0 + 1);

    // An untouched shared symbol keeps its version.
    let untouched = SymbolKey::Service(id("service.checkout"));

    assert_eq!(
        first.node(&untouched).unwrap().version,
        second.node(&untouched).unwrap().version,
    );
}

#[test]
fn operation_sub_symbols_version_separately() {
    let mut workspace = fixture_workspace();
    let first = build(&workspace, None);

    // Clear one operation's program; its interface and requirements
    // must keep their versions while the program (and the operation
    // container) bump.
    let operation = id("operation.cancel_order");

    let draft = workspace.operations.get_mut(&operation).unwrap();

    draft.program = None;

    let second = build(&workspace, Some(&first));

    let program = SymbolKey::OperationProgram(operation.clone());
    let interface = SymbolKey::OperationInterface(operation.clone());
    let requirements = SymbolKey::OperationRequirements(operation.clone());
    let container = SymbolKey::Operation(operation.clone());

    assert_eq!(
        second.node(&program).unwrap().version.0,
        first.node(&program).unwrap().version.0 + 1,
    );

    assert_eq!(
        second.node(&container).unwrap().version.0,
        first.node(&container).unwrap().version.0 + 1,
    );

    assert_eq!(
        first.node(&interface).unwrap().version,
        second.node(&interface).unwrap().version,
    );

    assert_eq!(
        first.node(&requirements).unwrap().version,
        second.node(&requirements).unwrap().version,
    );

    // The program's derived symbols are gone with it.
    assert!(
        second
            .node(&SymbolKey::Transaction {
                operation: operation.clone(),
                transaction: id("tx.cancel_order"),
            })
            .is_none()
    );
}

#[test]
fn removed_symbol_is_absent() {
    let workspace = fixture_workspace();
    let first = build(&workspace, None);

    let mut changed = workspace.clone();

    changed.schemas.remove(&id("schema.OrderPaid"));

    let second = build(&changed, Some(&first));

    let key = SymbolKey::Schema(id("schema.OrderPaid"));

    assert!(first.node(&key).is_some());
    assert!(second.node(&key).is_none());
}

#[test]
fn program_derives_transaction_effect_and_binding_nodes() {
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    let operation = id("operation.create_order");

    for key in [
        SymbolKey::Transaction {
            operation: operation.clone(),
            transaction: id("tx.create_order.new"),
        },
        SymbolKey::EffectSite {
            operation: operation.clone(),
            effect: id("effect.create_order.publish_created"),
        },
        SymbolKey::Binding {
            operation: operation.clone(),
            binding: id("intent.create_order.publish_created"),
        },
        SymbolKey::Binding {
            operation: operation.clone(),
            binding: id("output.create_order"),
        },
        SymbolKey::Binding {
            operation: id("operation.charge_payment"),
            binding: id("result.charge_payment.card"),
        },
    ] {
        assert!(graph.node(&key).is_some(), "missing {key}");
    }
}

#[test]
fn request_effect_creates_call_edge() {
    let mut workspace = fixture_workspace();

    workspace
        .operations
        .insert(id("operation.gateway"), gateway_operation());

    let graph = build(&workspace, None);

    let rows = run(
        &workspace,
        &graph,
        GraphQuery::Callers {
            operation: id("operation.create_order"),
        },
    );

    let [QueryRow::Call(call)] = rows.as_slice() else {
        panic!("expected exactly one caller, got {rows:?}");
    };

    assert_eq!(call.caller, id("operation.gateway"));
    assert_eq!(call.target_input, id("input.create_order.request"));

    let callees = run(
        &workspace,
        &graph,
        GraphQuery::Callees {
            operation: id("operation.gateway"),
        },
    );

    assert_eq!(callees.len(), 1);

    // The graph edge itself: effect site → target operation.
    let site = graph
        .node_id(&SymbolKey::EffectSite {
            operation: id("operation.gateway"),
            effect: id("effect.gateway.create_order"),
        })
        .expect("call site node");

    let target = graph
        .node_id(&SymbolKey::Operation(id("operation.create_order")))
        .unwrap();

    assert!(
        graph
            .outgoing_of(site)
            .iter()
            .any(|edge| edge.kind == EdgeKind::CallsOperation && edge.to == target)
    );
}

#[test]
fn publications_create_topic_edges() {
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    let publishers = run(
        &workspace,
        &graph,
        GraphQuery::Publishers {
            topic: id("topic.order_events"),
        },
    );

    let sites: Vec<&EffectRef> = publishers
        .iter()
        .map(|row| match row {
            QueryRow::Publisher(publisher) => &publisher.site,
            other => panic!("unexpected row {other:?}"),
        })
        .collect();

    assert!(sites.contains(&&EffectRef::Operation {
        operation: id("operation.create_order"),
        effect: id("effect.create_order.publish_created"),
    }));

    // A transition-owned side effect publishes too.
    assert!(sites.contains(&&EffectRef::Transition {
        machine: id("machine.order_lifecycle"),
        transition: id("transition.order.mark_paid"),
        effect: id("effect.order.paid"),
    }));

    let consumers = run(
        &workspace,
        &graph,
        GraphQuery::Consumers {
            topic: id("topic.order_events"),
        },
    );

    assert_eq!(consumers.len(), 3, "{consumers:?}");
}

#[test]
fn reads_and_writes_create_object_access_edges() {
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    let readers = run(
        &workspace,
        &graph,
        GraphQuery::Readers {
            data_model: id("data.inventory"),
            object: id("object.stock"),
            field: None,
        },
    );

    let reading: Vec<&Id> = readers
        .iter()
        .map(|row| match row {
            QueryRow::Access(access) => &access.transaction.transaction,
            other => panic!("unexpected row {other:?}"),
        })
        .collect();

    assert!(reading.contains(&&id("tx.reserve_inventory")));
    assert!(reading.contains(&&id("tx.transfer_stock")));

    // Field-filtered writers: only the transfer writes on_hand.
    let writers = run(
        &workspace,
        &graph,
        GraphQuery::Writers {
            data_model: id("data.inventory"),
            object: id("object.stock"),
            field: Some(path("on_hand")),
        },
    );

    let writing: Vec<&Id> = writers
        .iter()
        .map(|row| match row {
            QueryRow::Access(access) => &access.transaction.transaction,
            other => panic!("unexpected row {other:?}"),
        })
        .collect();

    assert!(writing.contains(&&id("tx.transfer_stock")));
    assert!(!writing.contains(&&id("tx.reserve_inventory")));

    // A transition application writes the machine's state field, and
    // an insert writes every field.
    let status_writers = run(
        &workspace,
        &graph,
        GraphQuery::Writers {
            data_model: id("data.checkout"),
            object: id("object.order"),
            field: Some(path("status")),
        },
    );

    let writing: Vec<&Id> = status_writers
        .iter()
        .map(|row| match row {
            QueryRow::Access(access) => &access.transaction.transaction,
            other => panic!("unexpected row {other:?}"),
        })
        .collect();

    assert!(writing.contains(&&id("tx.cancel_order")));
    assert!(writing.contains(&&id("tx.apply_payment")));
    assert!(writing.contains(&&id("tx.create_order.new")));
}

#[test]
fn transitions_create_state_machine_edges() {
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    let users = run(
        &workspace,
        &graph,
        GraphQuery::TransitionUsers {
            machine: id("machine.order_lifecycle"),
            transition: id("transition.order.mark_paid"),
        },
    );

    let [QueryRow::TransitionUse(user)] = users.as_slice() else {
        panic!("expected exactly one user, got {users:?}");
    };

    assert_eq!(user.operation, id("operation.apply_payment"));
    assert_eq!(user.transaction, id("tx.apply_payment"));

    let transaction = graph
        .node_id(&SymbolKey::Transaction {
            operation: id("operation.apply_payment"),
            transaction: id("tx.apply_payment"),
        })
        .unwrap();

    let machine = graph
        .node_id(&SymbolKey::StateMachine(id("machine.order_lifecycle")))
        .unwrap();

    assert!(
        graph
            .outgoing_of(transaction)
            .iter()
            .any(|edge| edge.kind == EdgeKind::AppliesStateMachine && edge.to == machine)
    );
}

#[test]
fn reverse_references_are_recorded() {
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    let rows = run(
        &workspace,
        &graph,
        GraphQuery::ReferencesTo {
            symbol: SymbolKey::Schema(id("schema.OrderCreated")),
        },
    );

    let froms: Vec<&SymbolKey> = rows
        .iter()
        .map(|row| match row {
            QueryRow::Reference { from, .. } => from,
            other => panic!("unexpected row {other:?}"),
        })
        .collect();

    assert!(froms.contains(&&SymbolKey::Topic(id("topic.order_events"))));

    assert!(froms.contains(&&SymbolKey::EffectSite {
        operation: id("operation.create_order"),
        effect: id("effect.create_order.publish_created"),
    }));

    // The subscriber's interface contract depends on the schema.
    assert!(
        froms.contains(&&SymbolKey::OperationInterface(id(
            "operation.reserve_inventory"
        )))
    );
}

#[test]
fn runtime_declarations_are_tracked_symbols_with_their_references() {
    // L1 is part of the symbol graph, not a blob beside it: each
    // declaration is a versioned node, and each names what it depends
    // on — so a query for what rests on a topic, an input, or a pool
    // finds the runtime facts too.
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    for key in [
        SymbolKey::TopicRuntime(id("topic.order_events")),
        SymbolKey::ExecutionPool(id("pool.order_workers")),
        SymbolKey::Router(id("router.create_order")),
        SymbolKey::StorageLayout(id("layout.order")),
        SymbolKey::SubscriptionRuntime {
            operation: id("operation.reserve_inventory"),
            input: id("input.reserve_inventory.created"),
        },
    ] {
        assert!(graph.node(&key).is_some(), "{key} should be a tracked symbol");
    }

    let referrers = |symbol: SymbolKey| -> Vec<SymbolKey> {
        run(&workspace, &graph, GraphQuery::ReferencesTo { symbol })
            .iter()
            .map(|row| match row {
                QueryRow::Reference { from, .. } => from.clone(),
                other => panic!("unexpected row {other:?}"),
            })
            .collect()
    };

    // The topic's transport ordering rests on the topic.
    assert!(
        referrers(SymbolKey::Topic(id("topic.order_events")))
            .contains(&SymbolKey::TopicRuntime(id("topic.order_events")))
    );

    // A pool is referenced by every boundary assigned to it — the
    // shared execution population, made queryable.
    let pool_referrers = referrers(SymbolKey::ExecutionPool(id("pool.order_workers")));

    assert!(pool_referrers.contains(&SymbolKey::SubscriptionRuntime {
        operation: id("operation.reserve_inventory"),
        input: id("input.reserve_inventory.created"),
    }));

    assert!(pool_referrers.contains(&SymbolKey::SubscriptionRuntime {
        operation: id("operation.apply_payment"),
        input: id("input.apply_payment.captured"),
    }));

    // A router rests on the request boundary it serves.
    assert!(
        referrers(SymbolKey::Input {
            operation: id("operation.create_order"),
            input: id("input.create_order.request"),
        })
        .contains(&SymbolKey::Router(id("router.create_order")))
    );

    // A storage layout rests on the object it partitions.
    assert!(
        referrers(SymbolKey::DataObject {
            data_model: id("data.checkout"),
            object: id("object.order"),
        })
        .contains(&SymbolKey::StorageLayout(id("layout.order")))
    );
}

#[test]
fn query_results_are_canonical_and_deterministic() {
    let workspace = fixture_workspace();

    let query = GraphQuery::Writers {
        data_model: id("data.checkout"),
        object: id("object.order"),
        field: Some(path("status")),
    };

    let first = graph_query::run(&workspace, &build(&workspace, None), &query);
    let second = graph_query::run(&workspace, &build(&workspace, None), &query);

    assert_eq!(first.rows, second.rows);
    assert_eq!(first.fingerprint, second.fingerprint);

    // Adding a writer changes the set's fingerprint — the phantom
    // foundation.
    let mut widened = workspace.clone();

    let mut writer = gateway_operation();

    writer.program = widened
        .operations
        .get(&id("operation.cancel_order"))
        .unwrap()
        .program
        .clone();

    widened
        .operations
        .insert(id("operation.admin_force_state"), writer);

    let third = graph_query::run(&widened, &build(&widened, None), &query);

    assert_ne!(first.fingerprint, third.fingerprint);
}

#[test]
fn provenance_roots_walk_to_the_modeled_world() {
    let workspace = fixture_workspace();
    let graph = build(&workspace, None);

    // output.create_order derives deterministically from the request.
    let rows = run(
        &workspace,
        &graph,
        GraphQuery::ProvenanceRoots {
            operation: id("operation.create_order"),
            binding: id("output.create_order"),
        },
    );

    assert!(rows.iter().any(|row| matches!(
        row,
        QueryRow::Root(ProvenanceRoot::Input { input, path })
            if *input == id("input.create_order.request") && *path == self::path("order_id")
    )));

    // intent.apply_payment.order_paid rests on a transaction read of
    // the order and on the triggering event.
    let rows = run(
        &workspace,
        &graph,
        GraphQuery::ProvenanceRoots {
            operation: id("operation.apply_payment"),
            binding: id("intent.apply_payment.order_paid"),
        },
    );

    assert!(rows.iter().any(|row| matches!(
        row,
        QueryRow::Root(ProvenanceRoot::ObjectRead { transaction, object, .. })
            if *transaction == id("tx.apply_payment") && *object == id("object.order")
    )));

    assert!(rows.iter().any(|row| matches!(
        row,
        QueryRow::Root(ProvenanceRoot::Input { input, .. })
            if *input == id("input.apply_payment.captured")
    )));
}

#[test]
fn value_refs_are_expressible_in_tests() {
    // Exercised so the ValueRef surface stays available to future
    // synthetic-workspace tests.
    let reference = ValueRef {
        source: ValueSource::Input(id("input.create_order.request")),
        path: path("order_id"),
    };

    assert_eq!(reference.path, path("order_id"));
}
