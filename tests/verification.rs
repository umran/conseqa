use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use conseqa::{
    analyzer::{
        DiagnosticCode, Severity, VerificationCode, report, validation,
        verification::{
            self, ArtifactReplay, ConsumerCollapse, DecisionGap, DecisionRule, EffectRetrySafety,
            EffectSafety, GoverningKeyDefect, IdempotencyObstacle, IdempotencyProof,
            IdempotencyVerdict, KeyIdentity, PathRef, PayloadIdentityGap, RecoverabilityNote,
            RecoverabilityObstacle, RecoverabilityProof, RecoverabilityVerdict, ReplayGap,
            Resolution, ResultGap, ResultReplayObstacle, ResultReplayProof, ResultReplayVerdict,
            ProofScope, RemedyLayer, RetryDriver, SerializationObstacle, SerializationProof,
            SerializationVerdict, StabilityGap, StabilityRule, StableRoot, TransactionResolution,
            canonical_value_path,
        },
    },
    parser::yaml,
    spec::{
        Arm, AsyncJoin, Branch, CompletionRequirement, Condition, Derivation, Effect,
        ErrorDisposition, ErrorResultType, EstablishTransactionOutput, ExecuteEffect,
        ExecuteEffectAsync, ExternalEffect, FieldPath, Id,
        IdempotencyGuarantee, IdempotencyKey, IdempotencyRequirement, Input, JoinAll,
        MemberAssignment,
        MemberConcurrency, Literal, MatchResult, MessageIdentity, MessageSelector, Model,
        ObjectSelector, OperationBlock, OperationInputRef, OperationStep, Race,
        RecoverabilityRequirement, RequestIdentity, RequestInput, RequestRouting, ResultOutcome,
        ResultReplayRequirement, ResultType, ResultVariant, Return, Router, RuntimeModel, Schema,
        SchemaFragment, SelectorPredicate, SelectorValue, SerializationRequirement,
        OrderingSemantics, SubscriptionInput, SubscriptionRoutingKey, TopicRuntime, Transaction,
        TransactionIsolation, TransactionStep, ValueRef, ValueSource, Write,
    },
};

fn id(value: &str) -> Id {
    Id(value.to_owned())
}

fn path(components: &[&str]) -> FieldPath {
    FieldPath(components.iter().map(|c| (*c).to_owned()).collect())
}

fn input_key(input: &str, components: &[&str]) -> ValueRef {
    ValueRef {
        source: ValueSource::Input(id(input)),
        path: path(components),
    }
}

fn program_mut<'a>(model: &'a mut Model, operation: &str) -> &'a mut OperationBlock {
    &mut model.operations.get_mut(&id(operation)).unwrap().program
}

/// Mutable access to an operation's inline transaction, wherever it
/// sits in the program.
fn transaction_mut<'a>(
    model: &'a mut Model,
    operation: &str,
    transaction: &str,
) -> &'a mut Transaction {
    program_mut(model, operation)
        .transaction_mut(&id(transaction))
        .unwrap_or_else(|| {
            panic!("`{operation}` should declare inline transaction `{transaction}`")
        })
}

fn execute(
    effect_id: &str,
    effect: Effect,
    values: Derivation,
    bind: Option<&str>,
) -> OperationStep {
    OperationStep::ExecuteEffect(ExecuteEffect {
        effect_id: id(effect_id),
        effect,
        values,
        bind: bind.map(id),
    })
}

fn return_ok(request: &str, values: Derivation) -> OperationStep {
    OperationStep::Return(Return {
        request: id(request),
        outcome: ResultOutcome::Ok { values },
    })
}

fn return_err(request: &str, values: Derivation) -> OperationStep {
    OperationStep::Return(Return {
        request: id(request),
        outcome: ResultOutcome::Err { values },
    })
}

fn output_ref(output: &str, components: &[&str]) -> ValueRef {
    ValueRef {
        source: ValueSource::TransactionOutput(id(output)),
        path: path(components),
    }
}

fn block(steps: Vec<OperationStep>) -> OperationBlock {
    OperationBlock { steps }
}

/// A request effect from `caller` into `target`'s request input.
fn request_effect(target: &str, input: &str, schema: &str) -> conseqa::spec::Effect {
    conseqa::spec::Effect::Request(conseqa::spec::RequestEffect {
        target: conseqa::spec::RequestTarget {
            operation: id(target),
            input: id(input),
        },
        schema: id(schema),
        retry: conseqa::spec::RetrySemantics::Unspecified,
        idempotency_key_propagation: vec![],
    })
}

/// Straightens charge_payment: the card is charged without binding its
/// result and the capture is published unconditionally, as before the
/// provider's decline was modeled.
fn linearize_charge_payment(model: &mut Model) {
    let program = program_mut(model, "operation.charge_payment");

    let OperationStep::MatchResult(matched) = program.steps.remove(1) else {
        panic!("expected the card match");
    };

    let OperationStep::ExecuteEffect(card) = &mut program.steps[0] else {
        panic!("expected the card charge");
    };

    card.bind = None;

    program.steps.extend(matched.ok.steps);
}

/// Mutable access to the card charge's inline external contract, which
/// lives at the first step of charge_payment's program.
fn charge_card_mut(model: &mut Model) -> &mut ExternalEffect {
    let OperationStep::ExecuteEffect(step) =
        &mut program_mut(model, "operation.charge_payment").steps[0]
    else {
        panic!("expected the card-charge execute_effect step");
    };

    let Effect::External(card) = &mut step.effect else {
        panic!("card charge should be an external effect");
    };

    card
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load_flash_checkout() -> Model {
    let path = fixture_path("flash_checkout.yaml");

    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture `{}`: {error}", path.display()));

    yaml::parse(&source).expect("flash checkout fixture should parse")
}

fn subscription_mut<'a>(
    model: &'a mut Model,
    operation: &str,
    input: &str,
) -> &'a mut SubscriptionInput {
    let input = model
        .operations
        .get_mut(&id(operation))
        .unwrap()
        .inputs
        .get_mut(&id(input))
        .unwrap();

    match input {
        Input::Subscription(subscription) => subscription,
        Input::Request(_) => panic!("`{input:?}` is not a subscription"),
    }
}

/// The mutable runtime model of a fixture that declares one.
fn runtime_mut(model: &mut Model) -> &mut RuntimeModel {
    model
        .runtime
        .as_mut()
        .expect("the fixture declares a runtime model")
}

/// The grouping key mapping of a topic-scoped grouping declaration.
fn grouping_key_mut<'a>(model: &'a mut Model, topic: &str) -> &'a mut conseqa::spec::GroupingKey {
    topic_runtime_mut(model, topic)
        .grouping
        .as_mut()
        .unwrap_or_else(|| panic!("`{topic}` declares no grouping"))
}

fn topic_runtime_mut<'a>(model: &'a mut Model, topic: &str) -> &'a mut TopicRuntime {
    runtime_mut(model)
        .topics
        .get_mut(&id(topic))
        .unwrap_or_else(|| panic!("`{topic}` declares no runtime"))
}

fn subscription_runtime_mut<'a>(
    model: &'a mut Model,
    operation: &str,
    input: &str,
) -> &'a mut conseqa::spec::SubscriptionRuntime {
    runtime_mut(model)
        .subscriptions
        .get_mut(&id(operation))
        .and_then(|inputs| inputs.get_mut(&id(input)))
        .unwrap_or_else(|| panic!("`{operation}`/`{input}` declares no runtime"))
}

fn pool_mut<'a>(model: &'a mut Model, pool: &str) -> &'a mut conseqa::spec::ExecutionPool {
    runtime_mut(model)
        .execution_pools
        .get_mut(&id(pool))
        .unwrap_or_else(|| panic!("`{pool}` is not declared"))
}

fn bounded(value: u32) -> MemberConcurrency {
    MemberConcurrency::Bounded(NonZeroU32::new(value).expect("non-zero"))
}

/// Declares a router for one request boundary, replacing any existing
/// one.
fn put_router(
    model: &mut Model,
    router: &str,
    operation: &str,
    input: &str,
    pool: &str,
    routing: Option<RequestRouting>,
) {
    let runtime = runtime_mut(model);

    runtime
        .routers
        .retain(|_, existing| existing.boundary.operation != id(operation));

    runtime.routers.insert(
        id(router),
        Router {
            boundary: OperationInputRef {
                operation: id(operation),
                input: id(input),
            },
            pool: id(pool),
            routing,
        },
    );
}

fn consistent_hash(key: &[&[&str]]) -> RequestRouting {
    RequestRouting {
        key: key.iter().map(|component| path(component)).collect(),
        member_assignment: MemberAssignment::ConsistentHash,
    }
}

fn serialization_verdict(
    model: &Model,
    operation: &str,
    requirement: usize,
) -> SerializationVerdict {
    let report = verification::verify(model);

    report
        .serialization
        .iter()
        .find(|check| check.operation == id(operation) && check.requirement == requirement)
        .unwrap_or_else(|| panic!("no serialization check for `{operation}` #{requirement}"))
        .verdict
        .clone()
}

fn obstacles(verdict: &SerializationVerdict) -> &[SerializationObstacle] {
    match verdict {
        SerializationVerdict::Unproven { obstacles } => obstacles,
        SerializationVerdict::Proven { proof, .. } => {
            panic!("expected an unproven verdict, found proof {proof:?}")
        }
    }
}

#[test]
fn flash_checkout_serialization_requirements_are_proven() {
    let model = load_flash_checkout();

    let report = verification::verify(&model);

    // reserve_inventory, charge_payment, and apply_payment each
    // declare one serialization requirement.
    assert_eq!(report.serialization.len(), 3);

    assert!(
        report
            .serialization
            .iter()
            .all(|check| matches!(check.verdict, SerializationVerdict::Proven { .. })),
        "expected every serialization requirement proven:\n{report:#?}"
    );

    // The fixture's honest gaps: reserve_inventory's recoverability,
    // reserve_inventory's idempotency, charge_payment's idempotency
    // (the not_deduplicated card charge), and create_order's
    // idempotency through its cascade into reserve_inventory.
    assert!(!report.all_proven());
    assert_eq!(report.diagnostics().len(), 4);
}

#[test]
fn the_subscription_proof_states_its_facts_and_is_runtime_dependent() {
    let model = load_flash_checkout();

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    let SerializationVerdict::Proven {
        proof:
            SerializationProof::SubscriptionRouted {
                input,
                topic,
                pool,
                message_keys,
                member_assignment,
                ..
            },
        scope,
    } = verdict
    else {
        panic!("expected a subscription-routed proof, found {verdict:?}");
    };

    assert_eq!(input, id("input.reserve_inventory.created"));
    assert_eq!(topic, id("topic.order_events"));
    assert_eq!(pool, id("pool.order_workers"));
    assert_eq!(member_assignment, MemberAssignment::ConsistentHash);

    // The whole argument came from declared runtime topology, so the
    // proof is conditional on that realization.
    assert_eq!(scope, ProofScope::RuntimeDependent);

    // The subscription admits only OrderCreated, and the topic keys it
    // by the same field the requirement uses.
    assert_eq!(message_keys.len(), 1);
    assert_eq!(message_keys[0].schema, id("schema.OrderCreated"));
    assert_eq!(message_keys[0].grouping_key, path(&["order_id"]));
    assert_eq!(message_keys[0].identity, KeyIdentity::SamePath);
}

#[test]
fn a_router_with_a_matching_key_and_a_serial_pool_proves_request_serialization() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    pool_mut(&mut model, "pool.inventory_api").member_concurrency = bounded(1);

    put_router(
        &mut model,
        "router.transfer_stock",
        "operation.transfer_stock",
        "input.transfer_stock.request",
        "pool.inventory_api",
        Some(consistent_hash(&[&["sku"]])),
    );

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    let SerializationVerdict::Proven {
        proof:
            SerializationProof::RequestRouted {
                input,
                router,
                pool,
                routing_key,
                member_assignment,
            },
        scope,
    } = verdict
    else {
        panic!("expected a request-routed proof, found {verdict:?}");
    };

    assert_eq!(input, id("input.transfer_stock.request"));
    assert_eq!(router, id("router.transfer_stock"));
    assert_eq!(pool, id("pool.inventory_api"));
    assert_eq!(member_assignment, MemberAssignment::ConsistentHash);
    assert_eq!(scope, ProofScope::RuntimeDependent);

    assert_eq!(routing_key.len(), 1);
    assert_eq!(routing_key[0].path, path(&["sku"]));
    assert_eq!(routing_key[0].identity, KeyIdentity::SamePath);
}

#[test]
fn a_routing_key_wider_than_the_requirement_key_proves_nothing() {
    // Routing by (sku, source_warehouse_id) partitions same-sku
    // invocations across routing domains, so equal serialization keys
    // need not share an owning member.
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    pool_mut(&mut model, "pool.inventory_api").member_concurrency = bounded(1);

    put_router(
        &mut model,
        "router.transfer_stock",
        "operation.transfer_stock",
        "input.transfer_stock.request",
        "pool.inventory_api",
        Some(consistent_hash(&[&["sku"], &["source_warehouse_id"]])),
    );

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    assert_eq!(
        obstacles(&verdict),
        &[
            SerializationObstacle::RoutingKeyNotEquivalent {
                input: id("input.transfer_stock.request"),
                router: id("router.transfer_stock"),
                component: path(&["source_warehouse_id"]),
            },
            SerializationObstacle::RoutingKeyWiderThanRequirement {
                input: id("input.transfer_stock.request"),
                router: id("router.transfer_stock"),
                key: vec![path(&["sku"]), path(&["source_warehouse_id"])],
            },
        ]
    );
}

#[test]
fn an_empty_routing_key_never_proves_vacuously() {
    // Validation rejects an empty routing key, and verification must
    // stay conservative over the invalid model rather than concluding
    // that every invocation shares a domain because no component
    // separates them.
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    pool_mut(&mut model, "pool.inventory_api").member_concurrency = bounded(1);

    put_router(
        &mut model,
        "router.transfer_stock",
        "operation.transfer_stock",
        "input.transfer_stock.request",
        "pool.inventory_api",
        Some(RequestRouting {
            key: Vec::new(),
            member_assignment: MemberAssignment::ConsistentHash,
        }),
    );

    assert!(!validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    assert!(
        matches!(verdict, SerializationVerdict::Unproven { .. }),
        "{verdict:?}"
    );
}

/// A serialization requirement blocked only by absent topology is
/// routed to the L1 author. This is the common case after the two-layer
/// split: the program is correct and no edit to it can help.
#[test]
fn an_obligation_missing_only_runtime_facts_reports_a_runtime_remedy() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    runtime_mut(&mut model)
        .routers
        .remove(&id("router.transfer_stock"));

    let report = verification::verify(&model);

    let check = report
        .serialization
        .iter()
        .find(|check| {
            check.operation == id("operation.transfer_stock") && check.requirement == 0
        })
        .expect("the added requirement is checked");

    assert_eq!(check.remedy(), Some(RemedyLayer::Runtime));

    let obligations = report::obligations(&model, &report);

    let obligation = obligations
        .obligations
        .iter()
        .find(|obligation| obligation.id == "oblig.operation.transfer_stock.serialization.0")
        .expect("the obligation is reported");

    assert_eq!(obligation.remedy, Some(RemedyLayer::Runtime));
}

/// A key no input carries selects a population no routing declaration
/// can address, so the fix is L0 however the topology is written.
#[test]
fn a_key_not_sourced_from_an_input_reports_an_application_remedy() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .serialization = vec![SerializationRequirement {
        key: ValueRef {
            source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
            path: path(&["order_id"]),
        },
    }];

    let report = verification::verify(&model);

    let check = report
        .serialization
        .iter()
        .find(|check| {
            check.operation == id("operation.apply_payment") && check.requirement == 0
        })
        .expect("the added requirement is checked");

    assert_eq!(
        obstacles(&check.verdict)
            .iter()
            .map(SerializationObstacle::layer)
            .collect::<Vec<_>>(),
        vec![RemedyLayer::Application],
    );

    assert_eq!(check.remedy(), Some(RemedyLayer::Application));
}

/// Obstacles are conjunctive, so one application obstacle keeps the
/// whole obligation on the application side: topology work alone cannot
/// close it.
#[test]
fn a_mixed_obstacle_set_reports_the_application_remedy() {
    assert_eq!(
        RemedyLayer::joined([RemedyLayer::Runtime, RemedyLayer::Runtime]),
        Some(RemedyLayer::Runtime),
    );

    assert_eq!(
        RemedyLayer::joined([RemedyLayer::Runtime, RemedyLayer::Application]),
        Some(RemedyLayer::Application),
    );

    assert_eq!(RemedyLayer::joined([]), None);
}

/// A proven obligation is waiting on nothing.
#[test]
fn a_proven_obligation_reports_no_remedy() {
    let model = load_flash_checkout();
    let report = verification::verify(&model);

    for check in &report.serialization {
        assert!(matches!(check.verdict, SerializationVerdict::Proven { .. }));
        assert_eq!(check.remedy(), None);
    }
}

#[test]
fn a_request_boundary_with_no_router_has_no_runtime_fact_at_all() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    runtime_mut(&mut model)
        .routers
        .remove(&id("router.transfer_stock"));

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::NoRouter {
            input: id("input.transfer_stock.request"),
        }]
    );

    let report = verification::verify(&model);

    // The added serialization gap, plus the fixture's four standing
    // gaps.
    assert_eq!(report.diagnostics().len(), 5);
}

#[test]
fn pool_assignment_without_routing_yields_no_member_affinity() {
    // The router names the pool and nothing else: the analyzer learns
    // the execution population and no relationship between keys and
    // members. There is no `unspecified` routing variant to declare —
    // absence is the whole statement.
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    pool_mut(&mut model, "pool.inventory_api").member_concurrency = bounded(1);

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::RoutingAbsent {
            input: id("input.transfer_stock.request"),
            pool: id("pool.inventory_api"),
        }]
    );
}

#[test]
fn member_concurrency_above_one_defeats_both_routed_proofs() {
    let mut model = load_flash_checkout();

    pool_mut(&mut model, "pool.order_workers").member_concurrency = bounded(2);

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    // The routing leg holds; only the concurrency bound is missing.
    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::MemberConcurrencyNotSerial {
            input: id("input.reserve_inventory.created"),
            pool: id("pool.order_workers"),
            declared: bounded(2),
        }]
    );
}

#[test]
fn an_undeclared_pool_is_an_obstacle_not_a_panic() {
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .dispatch
    .pool = id("pool.missing");

    // The model no longer validates; verification stays total.
    assert!(!validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::PoolUndeclared {
            input: id("input.reserve_inventory.created"),
            pool: id("pool.missing"),
        }]
    );
}

#[test]
fn dispatch_without_routing_defeats_the_subscription_route() {
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .dispatch
    .routing = None;

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::RoutingAbsent {
            input: id("input.reserve_inventory.created"),
            pool: id("pool.order_workers"),
        }]
    );
}

#[test]
fn a_subscription_without_a_runtime_has_no_dispatch_fact() {
    let mut model = load_flash_checkout();

    runtime_mut(&mut model)
        .subscriptions
        .remove(&id("operation.reserve_inventory"));

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::NoSubscriptionRuntime {
            input: id("input.reserve_inventory.created"),
        }]
    );
}

#[test]
fn removing_the_runtime_leaves_l0_valid_and_its_obligations_unproven() {
    // Acceptance: dropping L1 may make requirements unproven, but does
    // not structurally invalidate otherwise valid L0.
    let mut model = load_flash_checkout();

    model.runtime = None;

    assert!(
        validation::validate(&model).is_empty(),
        "an L0-only model stays structurally valid"
    );

    let report = verification::verify(&model);

    assert!(
        report
            .serialization
            .iter()
            .all(|check| matches!(check.verdict, SerializationVerdict::Unproven { .. })),
        "no serialization requirement survives without runtime facts:\n{report:#?}"
    );
}

#[test]
fn a_shared_pool_alone_establishes_no_shared_routing_domain() {
    // reserve_inventory and apply_payment already share
    // pool.order_workers in the fixture. Each proof cites its own
    // routing declaration; neither borrows the other's.
    let model = load_flash_checkout();

    for operation in ["operation.reserve_inventory", "operation.apply_payment"] {
        let verdict = serialization_verdict(&model, operation, 0);

        let SerializationVerdict::Proven {
            proof: SerializationProof::SubscriptionRouted { input, pool, .. },
            ..
        } = &verdict
        else {
            panic!("expected a subscription-routed proof for {operation}, found {verdict:?}");
        };

        assert_eq!(pool, &id("pool.order_workers"));
        assert!(input.0.starts_with("input."));
    }

    // Dropping one boundary's routing leaves the other's proof intact:
    // nothing was inherited through the shared pool.
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .dispatch
    .routing = None;

    assert!(matches!(
        serialization_verdict(&model, "operation.reserve_inventory", 0),
        SerializationVerdict::Proven { .. }
    ));

    assert!(matches!(
        serialization_verdict(&model, "operation.apply_payment", 0),
        SerializationVerdict::Unproven { .. }
    ));
}

#[test]
fn a_storage_partition_key_proves_no_execution_affinity() {
    // A partition key and a routing key may name the same field. They
    // remain different facts, and the analyzer never substitutes one
    // for the other: the fixture partitions object.order by order_id,
    // which establishes nothing about where invocations execute.
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    pool_mut(&mut model, "pool.inventory_api").member_concurrency = bounded(1);

    // A layout keyed exactly like the requirement.
    runtime_mut(&mut model).storage_layouts.insert(
        id("layout.transfer"),
        conseqa::spec::StorageLayout {
            object: conseqa::spec::DataObjectRef {
                data_model: id("data.inventory"),
                object: id("object.stock"),
            },
            partition_key: vec![path(&["sku"])],
        },
    );

    // Two layouts for one object: drop the fixture's own so the model
    // stays valid.
    runtime_mut(&mut model)
        .storage_layouts
        .remove(&id("layout.stock"));

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::RoutingAbsent {
            input: id("input.transfer_stock.request"),
            pool: id("pool.inventory_api"),
        }]
    );
}

#[test]
fn a_proof_states_the_safe_ownership_transfer_it_relies_on() {
    // A member assignment used for a correctness proof carries a
    // normative obligation: ownership of a routing domain transfers
    // safely. The proof says so, because an implementation that
    // rebalances without it is non-conforming.
    let model = load_flash_checkout();

    let verification = verification::verify(&model);
    let report = conseqa::analyzer::report::obligations(&model, &verification);

    let obligation = report
        .obligations
        .iter()
        .find(|obligation| obligation.id.contains("reserve_inventory.serialization"))
        .expect("reserve_inventory declares a serialization obligation");

    assert_eq!(
        obligation.scope,
        Some(conseqa::analyzer::verification::ProofScope::RuntimeDependent)
    );

    assert!(
        obligation.assumptions.iter().any(|assumption| {
            assumption.contains("consistent_hash") && assumption.contains("transfers that ownership")
        }),
        "{:#?}",
        obligation.assumptions
    );

    assert!(
        obligation
            .assumptions
            .iter()
            .any(|assumption| assumption.contains("one simultaneously active invocation")),
        "{:#?}",
        obligation.assumptions
    );
}

#[test]
fn serialization_key_diverging_from_topic_key_is_unproven() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.reserve_inventory.created", &["warehouse_id"]),
        });

    assert!(validation::validate(&model).is_empty());

    // Requirement 0 (order_id) is still proven.
    assert!(matches!(
        serialization_verdict(&model, "operation.reserve_inventory", 0),
        SerializationVerdict::Proven { .. }
    ));

    // Requirement 1 (warehouse_id) does not coincide with the topic
    // key domain.
    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 1);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::KeyIdentityUnestablished {
            input: id("input.reserve_inventory.created"),
            topic: id("topic.order_events"),
            schema: id("schema.OrderCreated"),
            grouping_key: path(&["order_id"]),
        }]
    );
}

#[test]
fn round_robin_assignment_proves_neither_serialization_nor_ordering() {
    // The ownership leg, exercised rather than merely guarded. Every
    // other fact is in place — a keyed grouping matching the
    // requirement key, grouping_key routing, a pool bounded to one
    // invocation per member — and the proof still fails, because
    // rotation puts same-key deliveries on different members.
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .dispatch
    .routing = Some(conseqa::spec::SubscriptionRouting {
        key: SubscriptionRoutingKey::GroupingKey,
        member_assignment: MemberAssignment::RoundRobin,
    });

    // A known-arbitrary assignment is a legitimate declaration, not a
    // malformed one: the model stays valid.
    assert!(
        validation::validate(&model).is_empty(),
        "{:#?}",
        validation::validate(&model)
    );

    let verdict = serialization_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            obstacles(&verdict),
            [SerializationObstacle::MemberAssignmentNotExclusive {
                declared: MemberAssignment::RoundRobin,
                ..
            }]
        ),
        "{verdict:?}"
    );

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    verification::OrderingObstacle::MemberAssignmentNotExclusive {
                        declared: MemberAssignment::RoundRobin,
                        ..
                    }
                ))
        ),
        "{verdict:?}"
    );
}

#[test]
fn round_robin_is_a_stronger_statement_than_declaring_no_routing() {
    // The distinction the refactor spec deferred: unknown member
    // behaviour versus known arbitrary member behaviour. Neither
    // proves, but they are different facts and the obstacles say so.
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .dispatch
    .routing = None;

    let absent = serialization_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            obstacles(&absent),
            [SerializationObstacle::RoutingAbsent { .. }]
        ),
        "{absent:?}"
    );

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .dispatch
    .routing = Some(conseqa::spec::SubscriptionRouting {
        key: SubscriptionRoutingKey::GroupingKey,
        member_assignment: MemberAssignment::RoundRobin,
    });

    let declared = serialization_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            obstacles(&declared),
            [SerializationObstacle::MemberAssignmentNotExclusive { .. }]
        ),
        "{declared:?}"
    );

    assert_ne!(obstacles(&absent), obstacles(&declared));
}

#[test]
fn two_routers_on_one_boundary_prove_nothing() {
    // Regression. `router_for` took the first match, so a boundary
    // routed two ways proved from whichever router sorted first — while
    // the other sent the same invocations to an unbounded pool with no
    // affinity. Verification stays sound over a model validation
    // refuses.
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.transfer_stock.request", &["sku"]),
        });

    pool_mut(&mut model, "pool.inventory_api").member_concurrency = bounded(1);

    put_router(
        &mut model,
        "router.aaa_transfer_stock",
        "operation.transfer_stock",
        "input.transfer_stock.request",
        "pool.inventory_api",
        Some(consistent_hash(&[&["sku"]])),
    );

    runtime_mut(&mut model).routers.insert(
        id("router.zzz_transfer_stock_fallback"),
        conseqa::spec::Router {
            boundary: OperationInputRef {
                operation: id("operation.transfer_stock"),
                input: id("input.transfer_stock.request"),
            },
            pool: id("pool.checkout_api"),
            routing: None,
        },
    );

    assert!(!validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.transfer_stock", 0);

    assert!(
        matches!(
            obstacles(&verdict),
            [SerializationObstacle::AmbiguousRouter { .. }]
        ),
        "{verdict:?}"
    );
}

#[test]
fn declaring_transport_semantics_at_both_scopes_proves_nothing() {
    // Regression. `effective_grouping` reads the winning scope, so a
    // contradicting subscription declaration was dropped and the
    // topic's half proved on its own.
    let mut model = load_flash_checkout();

    let subscription = subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    );

    // The topic groups by order_id; this says event_id, under which
    // same-order_id deliveries land in different groups.
    subscription.grouping = Some(conseqa::spec::GroupingKey {
        mapping: [(id("schema.PaymentCaptured"), vec![path(&["event_id"])])]
            .into_iter()
            .collect(),
    });

    subscription.ordering = Some(OrderingSemantics::Global);

    assert!(!validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            obstacles(&verdict),
            [SerializationObstacle::TransportSemanticsAtBothScopes { .. }]
        ),
        "{verdict:?}"
    );

    // Ordering consumes the same grouping evidence, so it refuses for
    // the same reason rather than reading the topic's half.
    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    verification::OrderingObstacle::TransportSemanticsAtBothScopes { .. }
                ))
        ),
        "{verdict:?}"
    );
}

#[test]
fn an_explicit_ordering_none_claims_its_scope() {
    // Regression. `ordering: none` used to serialize identically to
    // omission, so an explicit negative at the subscription scope read
    // as "declares nothing" and silently inherited the topic's
    // precedence — proving an ordering requirement the author had just
    // declared they had no basis for.
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .ordering = Some(OrderingSemantics::None);

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            conseqa::analyzer::ValidationError::TransportSemanticsAtBothScopes { .. }
        )),
        "an explicit `none` is a declaration and claims the scope: {errors:#?}"
    );
}

#[test]
fn a_transport_without_grouping_is_rejected_before_verification() {
    // `key: grouping_key` names the effective grouping domain, so
    // validation refuses the pair when no such domain exists.
    let mut model = load_flash_checkout();

    topic_runtime_mut(&mut model, "topic.order_events").grouping = None;

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            conseqa::analyzer::ValidationError::RoutingWithoutGrouping { .. }
        )),
        "{errors:#?}"
    );

    // Verification stays total over the invalid model.
    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::NoGroupingDomain {
            input: id("input.reserve_inventory.created"),
            topic: id("topic.order_events"),
        }]
    );
}

#[test]
fn serialization_consumes_grouping_and_never_ordering() {
    // The point of separating the two facts. An unordered transport
    // that still groups by key — an ordinary queue with consistent-hash
    // workers — serializes; the model no longer has to claim an
    // ordering guarantee it does not have in order to say so.
    let mut model = load_flash_checkout();

    topic_runtime_mut(&mut model, "topic.order_events").ordering = Some(OrderingSemantics::None);

    assert!(
        validation::validate(&model).is_empty(),
        "grouping without ordering is a valid declaration"
    );

    assert!(
        matches!(
            serialization_verdict(&model, "operation.reserve_inventory", 0),
            SerializationVerdict::Proven {
                proof: SerializationProof::SubscriptionRouted { .. },
                ..
            }
        ),
        "serialization rests on the grouping alone"
    );

    // Ordering correctly does not follow: nothing orders anything.
    let verdict = ordering_verdict(&model, "operation.reserve_inventory", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if matches!(&obstacles[..], [verification::OrderingObstacle::NoTransportPrecedence { .. }])
        ),
        "{verdict:?}"
    );
}

#[test]
fn a_key_not_sourced_from_an_input_selects_no_population() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .serialization = vec![SerializationRequirement {
        key: ValueRef {
            source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
            path: path(&["order_id"]),
        },
    }];

    assert!(validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.apply_payment", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::KeyNotFromInput {
            source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
        }]
    );
}

#[test]
fn empty_message_selection_is_vacuously_proven_without_any_runtime_fact() {
    let mut model = load_flash_checkout();

    subscription_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .messages = MessageSelector::Only(BTreeSet::new());

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        verdict,
        SerializationVerdict::Proven {
            proof: SerializationProof::NoAdmittedInvocations {
                input: id("input.reserve_inventory.created"),
            },
            scope: ProofScope::L0Only,
        }
    );
}

#[test]
fn fragment_aliasing_establishes_key_identity() {
    let mut model = load_flash_checkout();

    // A fragment view of OrderCreated in which `ref` aliases
    // `order_id` under a different name.
    let mut mapping = BTreeMap::new();

    for field in [
        "order_id",
        "event_id",
        "warehouse_id",
        "sku",
        "quantity",
        "amount",
    ] {
        mapping.insert(field.to_owned(), path(&[field]));
    }

    mapping.insert("ref".to_owned(), path(&["order_id"]));

    model.schemas.insert(
        id("schema.OrderCreatedView"),
        Schema::Fragment(SchemaFragment {
            source: id("schema.OrderCreated"),
            mapping,
        }),
    );

    model
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap()
        .messages
        .insert(id("schema.OrderCreatedView"));

    // The grouping keys the fragment schema by its aliased name.
    grouping_key_mut(&mut model, "topic.order_events")
        .mapping
        .insert(id("schema.OrderCreatedView"), vec![path(&["ref"])]);

    let subscription = subscription_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    );

    subscription.messages = MessageSelector::Only(BTreeSet::from([id("schema.OrderCreatedView")]));

    assert!(
        validation::validate(&model).is_empty(),
        "the fragment mutation should keep the model valid"
    );

    // The requirement key is `order_id`; the topic keys the admitted
    // schema by `ref`. Both expand to OrderCreated.order_id.
    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    let SerializationVerdict::Proven {
        proof: SerializationProof::SubscriptionRouted { message_keys, .. },
        ..
    } = verdict
    else {
        panic!("expected a subscription-routed proof, found {verdict:?}");
    };

    assert_eq!(message_keys.len(), 1);
    assert_eq!(message_keys[0].schema, id("schema.OrderCreatedView"));
    assert_eq!(message_keys[0].grouping_key, path(&["ref"]));
    assert_eq!(
        message_keys[0].identity,
        KeyIdentity::SameCanonicalValue {
            schema: id("schema.OrderCreated"),
            path: path(&["order_id"]),
        }
    );
}

#[test]
fn missing_topic_key_mapping_is_an_obstacle_not_a_panic() {
    let mut model = load_flash_checkout();

    grouping_key_mut(&mut model, "topic.order_events")
        .mapping
        .remove(&id("schema.OrderCreated"));

    // The model no longer validates; verification must stay total and
    // conservative.
    assert!(!validation::validate(&model).is_empty());

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        obstacles(&verdict),
        &[
            SerializationObstacle::GroupingKeyMappingMissing {
                input: id("input.reserve_inventory.created"),
                topic: id("topic.order_events"),
                schema: id("schema.OrderCreated"),
            },
        ]
    );
}

#[test]
fn dangling_key_input_is_unproven_not_a_panic() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap()
        .requirements
        .serialization = vec![SerializationRequirement {
        key: input_key("input.missing", &["order_id"]),
    }];

    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        obstacles(&verdict),
        &[SerializationObstacle::KeyNotFromInput {
            source: ValueSource::Input(id("input.missing")),
        }]
    );
}

#[test]
fn canonical_value_path_expands_fragment_chains() {
    let mut model = load_flash_checkout();

    model.schemas.insert(
        id("schema.ViewA"),
        Schema::Fragment(SchemaFragment {
            source: id("schema.OrderCreated"),
            mapping: BTreeMap::from([("a".to_owned(), path(&["order_id"]))]),
        }),
    );

    model.schemas.insert(
        id("schema.ViewB"),
        Schema::Fragment(SchemaFragment {
            source: id("schema.ViewA"),
            mapping: BTreeMap::from([("b".to_owned(), path(&["a"]))]),
        }),
    );

    let canonical = canonical_value_path(&model, &id("schema.ViewB"), &path(&["b"])).unwrap();

    assert_eq!(canonical.schema, id("schema.OrderCreated"));
    assert_eq!(canonical.path, path(&["order_id"]));

    // Unresolvable paths resolve to nothing rather than panicking.
    assert!(canonical_value_path(&model, &id("schema.ViewB"), &path(&["missing"])).is_none());
    assert!(canonical_value_path(&model, &id("schema.missing"), &path(&["b"])).is_none());
}

#[test]
fn verification_report_round_trips_through_json() {
    let mut model = load_flash_checkout();

    // Make one requirement unproven so both verdict shapes serialize.
    model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap()
        .requirements
        .serialization
        .push(SerializationRequirement {
            key: input_key("input.reserve_inventory.created", &["warehouse_id"]),
        });

    let report = verification::verify(&model);

    let json = serde_json::to_string(&report).expect("report should serialize");

    let restored: verification::VerificationReport =
        serde_json::from_str(&json).expect("report should deserialize");

    assert_eq!(report, restored);
}

fn ikey(input: &str, components: &[&[&str]]) -> IdempotencyKey {
    IdempotencyKey {
        components: components
            .iter()
            .map(|component| input_key(input, component))
            .collect(),
    }
}

fn deterministic(from: Vec<ValueRef>) -> Derivation {
    Derivation::Deterministic { from }
}

fn result_replay_verdict(
    model: &Model,
    operation: &str,
    requirement: usize,
) -> ResultReplayVerdict {
    let report = verification::verify(model);

    report
        .result_replay
        .iter()
        .find(|check| check.operation == id(operation) && check.requirement == requirement)
        .unwrap_or_else(|| panic!("no result-replay check for `{operation}` #{requirement}"))
        .verdict
        .clone()
}

/// The single returning path of a proven result-replay verdict.
fn single_return(verdict: &ResultReplayVerdict) -> &verification::ReturnedResult {
    let ResultReplayVerdict::Proven {
        proof: ResultReplayProof::ClassFixedResult { returns },
        ..
    } = verdict
    else {
        panic!("expected a class-fixed result, found {verdict:?}");
    };

    assert_eq!(
        returns.len(),
        1,
        "expected one returning path:\n{returns:#?}"
    );

    &returns[0]
}

/// The unavailable-artifact gap behind the first unstable root of an
/// unproven result-replay verdict.
fn unavailable_result_root(verdict: &ResultReplayVerdict) -> (&Vec<ReplayGap>, &Vec<ReplayGap>) {
    let ResultReplayVerdict::Unproven { obstacles } = verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    let ResultReplayObstacle::ResultDerivationRootUnstable { roots, .. } = &obstacles[0] else {
        panic!("expected an unstable result root, found {:?}", obstacles[0]);
    };

    let StabilityGap::ArtifactUnavailable {
        recovery,
        reconstruction,
        ..
    } = &roots[0].gap
    else {
        panic!("expected an unavailable artifact, found {:?}", roots[0].gap);
    };

    (recovery, reconstruction)
}

fn create_order_transaction(model: &mut Model) -> &mut Transaction {
    transaction_mut(model, "operation.create_order", "tx.create_order.new")
}

/// Replaces create_order's transaction body with a naturally
/// replayable shape: a key-derived write and an output establishment,
/// with no keyed commit.
fn make_create_order_natural(model: &mut Model) {
    let transaction = create_order_transaction(model);

    transaction.idempotency = IdempotencyGuarantee::Unspecified;

    transaction.steps = vec![
        TransactionStep::Write(Write {
            target: ObjectSelector {
                object: id("object.order"),
                predicate: SelectorPredicate::Eq {
                    field: path(&["order_id"]),
                    value: SelectorValue::Value(input_key(
                        "input.create_order.request",
                        &["order_id"],
                    )),
                },
            },
            fields: BTreeSet::from([path(&["amount"])]),
            values: deterministic(vec![
                input_key("input.create_order.request", &["order_id"]),
                input_key("input.create_order.request", &["amount"]),
            ]),
        }),
        TransactionStep::EstablishTransactionOutput(EstablishTransactionOutput {
            bind: id("output.create_order"),
            schema: id("schema.CreateOrderResponse"),
            values: deterministic(vec![input_key("input.create_order.request", &["order_id"])]),
        }),
    ];

    // The intent is no longer established, so the program must not
    // execute it.
    program_mut(model, "operation.create_order").steps.remove(1);
}

#[test]
fn flash_checkout_result_replay_is_proven_by_recovery() {
    let model = load_flash_checkout();

    let report = verification::verify(&model);

    // Only create_order declares `result: replay_consistent`.
    assert_eq!(report.result_replay.len(), 1);

    let check = &report.result_replay[0];

    assert_eq!(check.operation, id("operation.create_order"));
    assert_eq!(check.requirement, 0);
    assert!(!check.coinductive);

    let returned = single_return(&check.verdict);

    // One path, no decisions, returning ok from the recovered output.
    assert_eq!(returned.path, PathRef::default());
    assert_eq!(returned.variant, ResultVariant::Ok);
    assert!(returned.decisions.is_empty());

    assert_eq!(
        returned.derivation,
        vec![
            StableRoot {
                root: output_ref("output.create_order", &["order_id"]),
                rule: StabilityRule::RecoveredArtifact {
                    transaction: id("tx.create_order.new"),
                },
            },
            StableRoot {
                root: output_ref("output.create_order", &["status"]),
                rule: StabilityRule::RecoveredArtifact {
                    transaction: id("tx.create_order.new"),
                },
            },
        ]
    );

    // Route B rests on the commit key being the governing key itself;
    // the recoverability proof states that fact.
    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order resumable, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].transactions[..],
        [TransactionResolution {
            resolution: Resolution::KeyedCommit { key },
            ..
        }] if key == &vec![StableRoot {
            root: input_key("input.create_order.request", &["idempotency_key"]),
            rule: StabilityRule::KeyComponent,
        }]
    ));
}

#[test]
fn natural_reconstruction_proves_result_replay() {
    let mut model = load_flash_checkout();

    make_create_order_natural(&mut model);

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    let returned = single_return(&verdict);

    // Route A: the output is reconstructed by naturally replaying the
    // transaction, every root of its derivation covered by the request
    // identity pinned by the governing key.
    assert!(
        returned.derivation.iter().all(|root| root.rule
            == StabilityRule::ReconstructedArtifact {
                transaction: id("tx.create_order.new"),
            }),
        "{:#?}",
        returned.derivation
    );

    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order resumable, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].artifacts[..],
        [verification::ArtifactAvailability {
            replay: ArtifactReplay::Reconstructed { derivation, .. },
            ..
        }] if derivation == &vec![StableRoot {
            root: input_key("input.create_order.request", &["order_id"]),
            rule: StabilityRule::IdentifiedPayload,
        }]
    ));
}

#[test]
fn poison_retry_defeats_natural_reconstruction() {
    let mut model = load_flash_checkout();

    make_create_order_natural(&mut model);

    // Without the boundary identity, same-key attempts may present
    // different payloads.
    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Unspecified;

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    let (recovery, reconstruction) = unavailable_result_root(&verdict);

    assert_eq!(recovery, &vec![ReplayGap::NoKeyedCommit]);

    assert!(
        reconstruction.contains(&ReplayGap::MutationDerivationRootUnstable {
            root: input_key("input.create_order.request", &["amount"]),
            gap: StabilityGap::UnidentifiedPayloadField {
                input: id("input.create_order.request"),
                identity: PayloadIdentityGap::NotDeclared,
            },
        }),
        "expected the poison-retry gap on `amount`:\n{reconstruction:#?}"
    );
}

#[test]
fn unstable_commit_key_defeats_recovery() {
    let mut model = load_flash_checkout();

    create_order_transaction(&mut model).idempotency = IdempotencyGuarantee::DeduplicatedBy {
        key: ikey("input.create_order.request", &[&["amount"]]),
    };

    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Unspecified;

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    let (recovery, reconstruction) = unavailable_result_root(&verdict);

    // The keyed commit exists, but its key is an unidentified non-key
    // field, so attempts may address different commits.
    assert_eq!(
        recovery,
        &vec![ReplayGap::CommitKeyRootUnstable {
            root: input_key("input.create_order.request", &["amount"]),
            gap: StabilityGap::UnidentifiedPayloadField {
                input: id("input.create_order.request"),
                identity: PayloadIdentityGap::NotDeclared,
            },
        }]
    );

    assert!(reconstruction.contains(&ReplayGap::ContainsInsert));
}

#[test]
fn identified_payload_stabilizes_commit_key() {
    let mut model = load_flash_checkout();

    // Same non-key commit key, but the declared request identity is
    // pinned by the governing key, so `amount` is class-fixed.
    create_order_transaction(&mut model).idempotency = IdempotencyGuarantee::DeduplicatedBy {
        key: ikey("input.create_order.request", &[&["amount"]]),
    };

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    assert!(matches!(
        single_return(&verdict).derivation[0].rule,
        StabilityRule::RecoveredArtifact { .. }
    ));

    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order resumable, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].transactions[..],
        [TransactionResolution {
            resolution: Resolution::KeyedCommit { key },
            ..
        }] if key == &vec![StableRoot {
            root: input_key("input.create_order.request", &["amount"]),
            rule: StabilityRule::IdentifiedPayload,
        }]
    ));
}

#[test]
fn chained_artifact_recovery_is_class_fixed() {
    let mut model = load_flash_checkout();

    // A second keyed inline transaction whose commit key is a field of
    // the first transaction's recovered result.
    let operation = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap();

    operation.program.steps.insert(
        1,
        OperationStep::Transaction(Transaction {
            id: id("tx.create_order.receipt"),
            data_model: None,
            isolation: TransactionIsolation::Unspecified,
            idempotency: IdempotencyGuarantee::DeduplicatedBy {
                key: IdempotencyKey {
                    components: vec![output_ref("output.create_order", &["order_id"])],
                },
            },
            steps: vec![TransactionStep::EstablishTransactionOutput(
                EstablishTransactionOutput {
                    bind: id("output.create_order.receipt"),
                    schema: id("schema.CreateOrderResponse"),
                    values: deterministic(vec![input_key(
                        "input.create_order.request",
                        &["order_id"],
                    )]),
                },
            )],
        }),
    );

    operation.program.steps[3] = return_ok(
        "input.create_order.request",
        deterministic(vec![output_ref(
            "output.create_order.receipt",
            &["order_id"],
        )]),
    );

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    assert_eq!(
        single_return(&verdict).derivation,
        vec![StableRoot {
            root: output_ref("output.create_order.receipt", &["order_id"]),
            rule: StabilityRule::RecoveredArtifact {
                transaction: id("tx.create_order.receipt"),
            },
        }]
    );

    // The second commit key is stable because the first artifact is
    // recovered: stability chains through the artifact context.
    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order resumable, found {verdict:?}");
    };

    let TransactionResolution {
        resolution: Resolution::KeyedCommit { key },
        ..
    } = &paths[0].transactions[1]
    else {
        panic!("expected the receipt commit resolved by key");
    };

    assert_eq!(key.len(), 1);

    assert_eq!(
        key[0].rule,
        StabilityRule::RecoveredArtifact {
            transaction: id("tx.create_order.new"),
        }
    );
}

#[test]
fn subscription_key_without_a_return_is_vacuous() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap()
        .requirements
        .idempotency[0]
        .result = ResultReplayRequirement::ReplayConsistent;

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        verdict,
        ResultReplayVerdict::Proven {
            proof: ResultReplayProof::NoReturnedResult {
                input: id("input.reserve_inventory.created"),
            },
            scope: ProofScope::L0Only,
        }
    );
}

#[test]
fn governing_key_mixing_sources_is_inadmissible() {
    let mut model = load_flash_checkout();

    let requirement = &mut model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .requirements
        .idempotency[0];

    requirement.key.components.push(ValueRef {
        source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
        path: path(&["order_id"]),
    });

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    assert_eq!(
        verdict,
        ResultReplayVerdict::Unproven {
            obstacles: vec![ResultReplayObstacle::GoverningKeyInadmissible {
                defect: GoverningKeyDefect::ComponentNotFromInput {
                    source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
                },
            }],
        }
    );
}

#[test]
fn read_dependent_result_is_not_reconstructible() {
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.transfer_stock", "tx.transfer_stock")
        .steps
        .push(TransactionStep::EstablishTransactionOutput(
            EstablishTransactionOutput {
                bind: id("output.transfer_stock"),
                schema: id("schema.TransferStockResponse"),
                values: deterministic(vec![ValueRef {
                    source: ValueSource::TransactionRead(id("read.transfer_stock.source_stock")),
                    path: path(&["on_hand"]),
                }]),
            },
        ));

    let operation = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    operation.program.steps[1] = return_ok(
        "input.transfer_stock.request",
        deterministic(vec![output_ref("output.transfer_stock", &["accepted"])]),
    );

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            result: ResultReplayRequirement::ReplayConsistent,
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let (recovery, reconstruction) = unavailable_result_root(&verdict);

    assert_eq!(recovery, &vec![ReplayGap::NoKeyedCommit]);

    // The artifact derives from a transaction read, which is never
    // replay-stable.
    assert!(
        reconstruction.contains(&ReplayGap::ArtifactDerivationRootUnstable {
            root: ValueRef {
                source: ValueSource::TransactionRead(id("read.transfer_stock.source_stock")),
                path: path(&["on_hand"]),
            },
            gap: StabilityGap::TransactionReadRoot {
                read: id("read.transfer_stock.source_stock"),
            },
        }),
        "expected the transaction-read gap:\n{reconstruction:#?}"
    );

    // The destination write declares no provenance at all.
    assert!(reconstruction.contains(&ReplayGap::MutationDerivationUnspecified));
}

#[test]
fn output_never_established_is_conservatively_unproven() {
    let mut model = load_flash_checkout();

    create_order_transaction(&mut model)
        .steps
        .retain(|step| !matches!(step, TransactionStep::EstablishTransactionOutput(_)));

    // Validation rejects the shape; verification stays total and finds
    // the output missing from the context at the terminal.
    assert!(!validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    let ResultReplayVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert!(matches!(
        &obstacles[..],
        [ResultReplayObstacle::ResultDerivationRootUnstable { roots, .. }]
            if roots.iter().all(|root| matches!(
                &root.gap,
                StabilityGap::ArtifactNotInContext { artifact }
                    if artifact == &id("output.create_order")
            ))
    ));
}

/// Wraps create_order's whole program in a branch on the request's
/// `amount`: the original steps become the `then` arm, and the
/// `otherwise` arm returns directly from the governing key — inline
/// declarations are single-producer, so the arms cannot run the same
/// steps.
fn branch_create_order_on_amount(model: &mut Model, condition: Condition) {
    let program = program_mut(model, "operation.create_order");

    let steps = std::mem::take(&mut program.steps);

    program.steps = vec![OperationStep::Branch(Branch {
        condition,
        then: block(steps),
        otherwise: Some(block(vec![return_ok(
            "input.create_order.request",
            deterministic(vec![input_key(
                "input.create_order.request",
                &["idempotency_key"],
            )]),
        )])),
    })];
}

fn amount_is_large() -> Condition {
    Condition::Eq {
        value: input_key("input.create_order.request", &["amount"]),
        equals: SelectorValue::Literal(Literal::Int(1000)),
    }
}

#[test]
fn a_branch_over_stable_roots_replays() {
    let mut model = load_flash_checkout();

    branch_create_order_on_amount(&mut model, amount_is_large());

    assert!(validation::validate(&model).is_empty());

    // `amount` is covered by the request identity pinned by the key, so
    // every attempt in a class takes the same arm: each path returns a
    // class-fixed result, and no cross-path argument is needed.
    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    let ResultReplayVerdict::Proven {
        proof: ResultReplayProof::ClassFixedResult { returns },
        ..
    } = &verdict
    else {
        panic!("expected a class-fixed result, found {verdict:?}");
    };

    assert_eq!(returns.len(), 2);

    for (returned, arm) in returns.iter().zip([Arm::Then, Arm::Otherwise]) {
        assert!(matches!(
            &returned.path.decisions[..],
            [verification::DecisionTaken::Branch { arm: taken, .. }] if *taken == arm
        ));

        assert!(matches!(
            &returned.decisions[..],
            [verification::DecisionReplay {
                rule: DecisionRule::StableCondition { roots },
                ..
            }] if roots == &vec![StableRoot {
                root: input_key("input.create_order.request", &["amount"]),
                rule: StabilityRule::IdentifiedPayload,
            }]
        ));
    }
}

#[test]
fn a_branch_over_unstable_roots_defeats_result_replay() {
    let mut model = load_flash_checkout();

    branch_create_order_on_amount(&mut model, amount_is_large());

    // Without the boundary identity, same-key attempts may present
    // different amounts and take different arms.
    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Unspecified;

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    let ResultReplayVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    // The same decision reached by both paths is one obstacle.
    assert!(
        matches!(
            &obstacles[..],
            [ResultReplayObstacle::PathDecisionUnstable {
                gap: DecisionGap::ConditionRootsUnstable { roots },
                ..
            }] if roots.len() == 1
                && roots[0].root == input_key("input.create_order.request", &["amount"])
                && matches!(roots[0].gap, StabilityGap::UnidentifiedPayloadField { .. })
        ),
        "{obstacles:#?}"
    );
}

#[test]
fn an_unspecified_condition_never_replays() {
    let mut model = load_flash_checkout();

    branch_create_order_on_amount(&mut model, Condition::Unspecified);

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.create_order", 0);

    assert!(
        matches!(
            &verdict,
            ResultReplayVerdict::Unproven { obstacles }
                if matches!(
                    &obstacles[..],
                    [ResultReplayObstacle::PathDecisionUnstable {
                        gap: DecisionGap::ConditionUnspecified,
                        ..
                    }]
                )
        ),
        "{verdict:?}"
    );
}

/// Makes transfer_stock forward a request into create_order and return
/// its result: ok from the created order, err from the rejection.
fn forward_transfer_to_create_order(model: &mut Model) {
    let operation = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    operation.program.steps = vec![
        execute(
            "effect.transfer_stock.forward",
            request_effect(
                "operation.create_order",
                "input.create_order.request",
                "schema.CreateOrderRequest",
            ),
            deterministic(vec![input_key("input.transfer_stock.request", &["sku"])]),
            Some("result.transfer_stock.forward"),
        ),
        OperationStep::MatchResult(MatchResult {
            result: id("result.transfer_stock.forward"),
            ok: block(vec![return_ok(
                "input.transfer_stock.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultOk(id("result.transfer_stock.forward")),
                    path: path(&["order_id"]),
                }]),
            )]),
            err: block(vec![return_err(
                "input.transfer_stock.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultErr(id("result.transfer_stock.forward")),
                    path: path(&["reason"]),
                }]),
            )]),
        }),
    ];

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            result: ResultReplayRequirement::ReplayConsistent,
        });
}

#[test]
fn a_match_on_a_consistent_request_result_replays() {
    let mut model = load_flash_checkout();

    forward_transfer_to_create_order(&mut model);

    assert!(validation::validate(&model).is_empty());

    // create_order proves its result replay-consistent for payload-equal
    // requests, so the class-fixed request observes one result: the
    // match replays, and each arm returns a payload derived from it.
    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let ResultReplayVerdict::Proven {
        proof: ResultReplayProof::ClassFixedResult { returns },
        ..
    } = &verdict
    else {
        panic!("expected a class-fixed result, found {verdict:?}");
    };

    assert_eq!(returns.len(), 2);
    assert_eq!(returns[0].variant, ResultVariant::Ok);
    assert_eq!(returns[1].variant, ResultVariant::Err);

    for returned in returns {
        assert!(matches!(
            &returned.decisions[..],
            [verification::DecisionReplay {
                rule: DecisionRule::StableResult {
                    rule: verification::ResultStabilityRule::ReplayConsistentTarget {
                        operation, input, requirement: 0, ..
                    },
                    ..
                },
                ..
            }] if operation == &id("operation.create_order")
                && input == &id("input.create_order.request")
        ));

        assert!(matches!(
            &returned.derivation[..],
            [StableRoot {
                rule: StabilityRule::ReplayConsistentResult { result, effect },
                ..
            }] if result == &id("result.transfer_stock.forward")
                && effect == &id("effect.transfer_stock.forward")
        ));
    }

    // Without the target's declaration, nothing says the request
    // observes one result.
    model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .requirements
        .idempotency[0]
        .result = ResultReplayRequirement::Unspecified;

    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let ResultReplayVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    // The decision and both returned payloads rest on the same result.
    assert!(
        obstacles.iter().any(|obstacle| matches!(
            obstacle,
            ResultReplayObstacle::PathDecisionUnstable {
                gap: DecisionGap::ResultUnstable {
                    gap: ResultGap::TargetResultNotDeclared { operation, .. },
                    ..
                },
                ..
            } if operation == &id("operation.create_order")
        )),
        "{obstacles:#?}"
    );

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        ResultReplayObstacle::ResultDerivationRootUnstable { .. }
    )));
}

#[test]
fn a_request_result_is_only_as_stable_as_its_request() {
    let mut model = load_flash_checkout();

    forward_transfer_to_create_order(&mut model);

    // The forwarded request now depends on an unidentified field of the
    // transfer request, so the target may be asked different questions.
    let OperationStep::ExecuteEffect(forward) =
        &mut program_mut(&mut model, "operation.transfer_stock").steps[0]
    else {
        panic!("expected the forwarded request");
    };

    forward.values = deterministic(vec![input_key(
        "input.transfer_stock.request",
        &["quantity"],
    )]);

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    assert!(
        matches!(
            &verdict,
            ResultReplayVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    ResultReplayObstacle::PathDecisionUnstable {
                        gap: DecisionGap::ResultUnstable {
                            gap: ResultGap::InstanceNotClassFixed {
                                gap: verification::InstanceGap::RootsUnstable { .. },
                            },
                            ..
                        },
                        ..
                    }
                ))
        ),
        "{verdict:?}"
    );
}

#[test]
fn cyclic_result_dependencies_prove_coinductively() {
    let mut model = load_flash_checkout();

    // transfer_stock and cancel_order each return the other's result.
    let transfer = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    transfer.program.steps = vec![
        execute(
            "effect.transfer_stock.cancel",
            request_effect(
                "operation.cancel_order",
                "input.cancel_order.request",
                "schema.CancelOrderRequest",
            ),
            deterministic(vec![input_key("input.transfer_stock.request", &["sku"])]),
            Some("result.transfer_stock.cancel"),
        ),
        OperationStep::MatchResult(MatchResult {
            result: id("result.transfer_stock.cancel"),
            ok: block(vec![return_ok(
                "input.transfer_stock.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultOk(id("result.transfer_stock.cancel")),
                    path: path(&["order_id"]),
                }]),
            )]),
            err: block(vec![return_err(
                "input.transfer_stock.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultErr(id("result.transfer_stock.cancel")),
                    path: path(&["reason"]),
                }]),
            )]),
        }),
    ];

    transfer
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            result: ResultReplayRequirement::ReplayConsistent,
        });

    let cancel = model
        .operations
        .get_mut(&id("operation.cancel_order"))
        .unwrap();

    cancel.program.steps = vec![
        execute(
            "effect.cancel_order.transfer",
            request_effect(
                "operation.transfer_stock",
                "input.transfer_stock.request",
                "schema.TransferStockRequest",
            ),
            deterministic(vec![input_key("input.cancel_order.request", &["order_id"])]),
            Some("result.cancel_order.transfer"),
        ),
        OperationStep::MatchResult(MatchResult {
            result: id("result.cancel_order.transfer"),
            ok: block(vec![return_ok(
                "input.cancel_order.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultOk(id("result.cancel_order.transfer")),
                    path: path(&["accepted"]),
                }]),
            )]),
            err: block(vec![return_err(
                "input.cancel_order.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultErr(id("result.cancel_order.transfer")),
                    path: path(&["reason"]),
                }]),
            )]),
        }),
    ];

    cancel
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.cancel_order.request", &[&["order_id"]]),
            result: ResultReplayRequirement::ReplayConsistent,
        });

    assert!(validation::validate(&model).is_empty());

    let report = verification::verify(&model);

    for operation in ["operation.transfer_stock", "operation.cancel_order"] {
        let check = report
            .result_replay
            .iter()
            .find(|check| check.operation == id(operation))
            .unwrap();

        assert!(
            matches!(check.verdict, ResultReplayVerdict::Proven { .. }),
            "expected `{operation}` proven, found {:?}",
            check.verdict
        );

        assert!(
            check.coinductive,
            "`{operation}` should be marked coinductive"
        );
    }

    // A member failing locally fails the cycle with it.
    let OperationStep::ExecuteEffect(forward) =
        &mut program_mut(&mut model, "operation.cancel_order").steps[0]
    else {
        panic!("expected the forwarded request");
    };

    forward.values = Derivation::Unspecified;

    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    assert!(
        matches!(
            &verdict,
            ResultReplayVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    ResultReplayObstacle::PathDecisionUnstable {
                        gap: DecisionGap::ResultUnstable {
                            gap: ResultGap::TargetResultUnproven { .. },
                            ..
                        },
                        ..
                    }
                ))
        ),
        "{verdict:?}"
    );
}

fn recoverability_verdict(
    model: &Model,
    operation: &str,
    requirement: usize,
) -> RecoverabilityVerdict {
    let report = verification::verify(model);

    report
        .recoverability
        .iter()
        .find(|check| check.operation == id(operation) && check.requirement == requirement)
        .unwrap_or_else(|| panic!("no recoverability check for `{operation}` #{requirement}"))
        .verdict
        .clone()
}

#[test]
fn flash_checkout_recoverability_verdicts() {
    let model = load_flash_checkout();

    let report = verification::verify(&model);

    // create_order (resumable), reserve_inventory (guaranteed), and
    // apply_payment (guaranteed).
    assert_eq!(report.recoverability.len(), 3);

    // create_order resumes through its keyed commit; the intent and
    // output are recovered, and the return derives from the output.
    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order resumable, found {verdict:?}");
    };

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].path, PathRef::default());

    assert!(matches!(
        &paths[0].transactions[..],
        [TransactionResolution {
            resolution: Resolution::KeyedCommit { .. },
            ..
        }]
    ));

    // The consumed intent and the returned output, both recovered.
    assert_eq!(paths[0].artifacts.len(), 2);

    assert!(
        paths[0]
            .artifacts
            .iter()
            .all(|artifact| matches!(artifact.replay, ArtifactReplay::Recovered { .. }))
    );

    // apply_payment is guaranteed: keyed commit, recovered transition
    // intent, and at-least-once delivery as the driver.
    let verdict = recoverability_verdict(&model, "operation.apply_payment", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Guaranteed { driver, .. },
        ..
    } = &verdict
    else {
        panic!("expected apply_payment guaranteed, found {verdict:?}");
    };

    assert_eq!(
        driver,
        &RetryDriver::AtLeastOnceDelivery {
            input: id("input.apply_payment.captured"),
            topic: id("topic.order_events"),
        }
    );

    // reserve_inventory has the driver but no continuation: the
    // committed reservation resolves by neither route, and the
    // publication intent is replay-available by neither route.
    let verdict = recoverability_verdict(&model, "operation.reserve_inventory", 0);

    let RecoverabilityVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected reserve_inventory unproven, found {verdict:?}");
    };

    assert_eq!(obstacles.len(), 2);

    let RecoverabilityObstacle::TransactionNotResolvable {
        transaction,
        recovery,
        reconstruction,
        ..
    } = &obstacles[0]
    else {
        panic!(
            "expected an unresolvable transaction, found {:?}",
            obstacles[0]
        );
    };

    assert_eq!(transaction, &id("tx.reserve_inventory"));
    assert_eq!(recovery, &vec![ReplayGap::NoKeyedCommit]);

    assert!(
        reconstruction.iter().any(|gap| matches!(
            gap,
            ReplayGap::MutationDerivationRootUnstable {
                gap: StabilityGap::TransactionReadRoot { .. },
                ..
            }
        )),
        "expected the read-dependent write gap:\n{reconstruction:#?}"
    );

    assert!(matches!(
        &obstacles[1],
        RecoverabilityObstacle::ArtifactNotReplayAvailable { artifact, .. }
            if artifact == &id("intent.reserve_inventory.publish_reserved")
    ));
}

#[test]
fn final_transaction_before_completion_needs_no_replay_route() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    // The transaction is unreplayable in every way, but nothing
    // follows it once the program completes instead of returning.
    operation.program.steps[1] = OperationStep::Complete;

    operation
        .requirements
        .recoverability
        .push(RecoverabilityRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            completion: CompletionRequirement::Resumable,
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.transfer_stock", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected resumable, found {verdict:?}");
    };

    assert_eq!(
        paths[0].transactions,
        vec![TransactionResolution {
            transaction: id("tx.transfer_stock"),
            resolution: Resolution::TerminalStep,
        }]
    );
}

#[test]
fn a_return_after_the_final_transaction_requires_resolution() {
    let mut model = load_flash_checkout();

    // Same program, but the result is returned after the transaction,
    // so a failing prefix exists after its commit.
    model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap()
        .requirements
        .recoverability
        .push(RecoverabilityRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            completion: CompletionRequirement::Resumable,
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.transfer_stock", 0);

    let RecoverabilityVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert_eq!(obstacles.len(), 1);

    assert!(matches!(
        &obstacles[0],
        RecoverabilityObstacle::TransactionNotResolvable { transaction, .. }
            if transaction == &id("tx.transfer_stock")
    ));
}

#[test]
fn guaranteed_completion_needs_a_modeled_driver() {
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .delivery = conseqa::spec::DeliverySemantics::AtMostOnce;

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.apply_payment", 0);

    assert_eq!(
        verdict,
        RecoverabilityVerdict::Unproven {
            obstacles: vec![RecoverabilityObstacle::NoModeledRetryDriver {
                input: id("input.apply_payment.captured"),
                delivery: Some(conseqa::spec::DeliverySemantics::AtMostOnce),
            }],
        }
    );
}

#[test]
fn inbound_repeatable_request_supplies_the_driver() {
    let mut model = load_flash_checkout();

    // Guaranteed completion on a request-driven operation: no driver
    // until a modeled caller declares a repeatable request effect.
    model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .requirements
        .recoverability[0]
        .completion = CompletionRequirement::Guaranteed;

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    assert_eq!(
        verdict,
        RecoverabilityVerdict::Unproven {
            obstacles: vec![RecoverabilityObstacle::NoModeledRetryDriver {
                input: id("input.create_order.request"),
                delivery: None,
            }],
        }
    );

    // A modeled caller declares the repeatable request at an inline
    // execution site.
    program_mut(&mut model, "operation.transfer_stock")
        .steps
        .insert(
            1,
            execute(
                "effect.transfer_stock.create_order",
                conseqa::spec::Effect::Request(conseqa::spec::RequestEffect {
                    target: conseqa::spec::RequestTarget {
                        operation: id("operation.create_order"),
                        input: id("input.create_order.request"),
                    },
                    schema: id("schema.CreateOrderRequest"),
                    retry: conseqa::spec::RetrySemantics::MayRepeat,
                    idempotency_key_propagation: vec![],
                }),
                Derivation::Unspecified,
                None,
            ),
        );

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Guaranteed { driver, .. },
        ..
    } = &verdict
    else {
        panic!("expected guaranteed, found {verdict:?}");
    };

    assert_eq!(
        driver,
        &RetryDriver::InboundRepeatableRequest {
            operation: id("operation.transfer_stock"),
            effect: id("effect.transfer_stock.create_order"),
        }
    );
}

#[test]
fn an_unestablished_intent_is_conservatively_unproven() {
    let mut model = load_flash_checkout();

    // The intent executes ahead of the transaction that establishes
    // it: its producer exists in the program, but no earlier step of
    // the path establishes the binding.
    program_mut(&mut model, "operation.create_order")
        .steps
        .swap(0, 1);

    // Validation rejects an intent executed where no path has
    // established it; verification stays total on the shape.
    assert!(!validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert_eq!(
        obstacles,
        &vec![RecoverabilityObstacle::ArtifactNotEstablished {
            path: PathRef::default(),
            artifact: id("intent.create_order.publish_created"),
            consumer: id("intent.create_order.publish_created"),
        }]
    );
}

#[test]
fn unavailable_consumed_artifacts_block_resumption() {
    let mut model = load_flash_checkout();

    // Natural-shape transaction with no keyed commit and no declared
    // request identity: the result is neither recoverable nor
    // reconstructible, and the transaction itself cannot resolve.
    make_create_order_natural(&mut model);

    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Unspecified;

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        RecoverabilityObstacle::TransactionNotResolvable { transaction, .. }
            if transaction == &id("tx.create_order.new")
    )));

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        RecoverabilityObstacle::ArtifactNotReplayAvailable { artifact, .. }
            if artifact == &id("output.create_order")
    )));
}

#[test]
fn a_diverging_decision_does_not_block_progress() {
    let mut model = load_flash_checkout();

    branch_create_order_on_amount(&mut model, amount_is_large());

    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Unspecified;

    assert!(validation::validate(&model).is_empty());

    // A retry may take the other arm, but whichever path it follows
    // resumes: progress holds per path, and the difference in work is
    // idempotency's concern.
    let verdict = recoverability_verdict(&model, "operation.create_order", 0);

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Resumable { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order resumable, found {verdict:?}");
    };

    assert_eq!(paths.len(), 2);

    assert!(matches!(
        result_replay_verdict(&model, "operation.create_order", 0),
        ResultReplayVerdict::Unproven { .. }
    ));
}

#[test]
fn an_unterminated_path_cannot_reach_a_terminal() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    operation.program.steps.pop();

    operation
        .requirements
        .recoverability
        .push(RecoverabilityRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            completion: CompletionRequirement::Resumable,
        });

    // Validation rejects the fall-through; verification records it.
    assert!(!validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.transfer_stock", 0);

    assert!(
        matches!(
            &verdict,
            RecoverabilityVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    RecoverabilityObstacle::PathNotTerminated { .. }
                ))
        ),
        "{verdict:?}"
    );
}

#[test]
fn empty_subscription_population_is_vacuously_recoverable() {
    let mut model = load_flash_checkout();

    subscription_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .messages = MessageSelector::Only(BTreeSet::new());

    let verdict = recoverability_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        verdict,
        RecoverabilityVerdict::Proven {
            proof: RecoverabilityProof::NoAdmittedInvocations {
                input: id("input.reserve_inventory.created"),
            },
            scope: ProofScope::L0Only,
        }
    );
}

#[test]
fn recoverability_key_mixing_sources_is_inadmissible() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .recoverability[0]
        .key
        .components
        .push(ValueRef {
            source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
            path: path(&["order_id"]),
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = recoverability_verdict(&model, "operation.apply_payment", 0);

    assert_eq!(
        verdict,
        RecoverabilityVerdict::Unproven {
            obstacles: vec![RecoverabilityObstacle::GoverningKeyInadmissible {
                defect: GoverningKeyDefect::ComponentNotFromInput {
                    source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
                },
            }],
        }
    );
}

fn order_events_message_identity(model: &mut Model) -> &mut BTreeMap<Id, Vec<FieldPath>> {
    let topic = model.topics.get_mut(&id("topic.order_events")).unwrap();

    let MessageIdentity::Keyed(identity) = &mut topic.message_identity else {
        panic!("order_events should declare a keyed message identity");
    };

    &mut identity.mapping
}

fn idempotency_verdict(model: &Model, operation: &str, requirement: usize) -> IdempotencyVerdict {
    let report = verification::verify(model);

    report
        .idempotency
        .iter()
        .find(|check| check.operation == id(operation) && check.requirement == requirement)
        .unwrap_or_else(|| panic!("no idempotency check for `{operation}` #{requirement}"))
        .verdict
        .clone()
}

#[test]
fn flash_checkout_idempotency_verdicts() {
    let model = load_flash_checkout();

    let report = verification::verify(&model);

    assert_eq!(report.idempotency.len(), 4);

    // create_order: keyed commit plus a recovered publication intent
    // whose schema the topic identifies — but its cascade does not
    // collapse: reserve_inventory consumes OrderCreated, and its own
    // requirement is unproven, so a retried create_order is not
    // established to avoid duplicate work downstream.
    let verdict = idempotency_verdict(&model, "operation.create_order", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected create_order unproven, found {verdict:?}");
    };

    assert!(
        matches!(
            &obstacles[..],
            [IdempotencyObstacle::PublicationConsumerRequirementUnproven {
                schema,
                operation,
                input,
                ..
            }] if schema == &id("schema.OrderCreated")
                && operation == &id("operation.reserve_inventory")
                && input == &id("input.reserve_inventory.created")
        ),
        "expected only the cascade obstacle for create_order:\n{obstacles:#?}"
    );

    // apply_payment: the transition transaction and its implicitly
    // established intent, both through the keyed commit.
    assert!(matches!(
        idempotency_verdict(&model, "operation.apply_payment", 0),
        IdempotencyVerdict::Proven { .. }
    ));

    // charge_payment: the capture publication is safe, but the card
    // charge is explicitly not deduplicated — the model admits charging
    // the card twice — so no same-key terminal result is fixed either:
    // the match on the provider's result is not established to replay,
    // and the decline publication, which reads that result, is not
    // class-fixed. Each fact is reported once, however many paths run
    // through its step.
    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected charge_payment unproven, found {verdict:?}");
    };

    assert!(
        matches!(
            &obstacles[..],
            [
                IdempotencyObstacle::ExternalEffectNotDeduplicated { effect, .. },
                IdempotencyObstacle::PathDecisionUnstable {
                    decision: verification::DecisionTaken::Match { result, arm: ResultVariant::Ok, .. },
                    gap: DecisionGap::ResultUnstable {
                        gap: ResultGap::ExternalNotDeduplicated,
                        ..
                    },
                    ..
                },
                IdempotencyObstacle::EffectInstanceRootUnstable { effect: failed, roots, .. },
            ] if effect == &id("effect.charge_payment.card")
                && result == &id("result.charge_payment.card")
                && failed == &id("effect.charge_payment.publish_failed")
                && matches!(roots[0].gap, StabilityGap::ResultUnstable { .. })
        ),
        "{obstacles:#?}"
    );

    // reserve_inventory: the reservation transaction is retry-unsafe
    // and the intent it establishes is unavailable.
    let verdict = idempotency_verdict(&model, "operation.reserve_inventory", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected reserve_inventory unproven, found {verdict:?}");
    };

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        IdempotencyObstacle::TransactionNotRetrySafe { transaction, .. }
            if transaction == &id("tx.reserve_inventory")
    )));

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        IdempotencyObstacle::IntentNotReplayAvailable { intent, .. }
            if intent == &id("intent.reserve_inventory.publish_reserved")
    )));

    // Its cascade reaches charge_payment, whose card charge is not
    // deduplicated.
    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        IdempotencyObstacle::PublicationConsumerRequirementUnproven { operation, .. }
            if operation == &id("operation.charge_payment")
    )));
}

#[test]
fn declared_external_deduplication_completes_the_charge_proof() {
    let mut model = load_flash_checkout();

    // Declare the payment provider's own idempotency: the card charge
    // deduplicates by the propagated event id.
    let card = charge_card_mut(&mut model);

    card.idempotency = IdempotencyGuarantee::DeduplicatedBy {
        key: ikey("input.charge_payment.reserved", &[&["event_id"]]),
    };

    assert!(validation::validate(&model).is_empty());

    // The boundary now collapses duplicate charges and fixes its
    // terminal result — but the decline's disposition is unspecified,
    // so nothing says an observed error terminally resolved the
    // charge: the err arm of the match, and the decline publication
    // reading that error, remain the obstacles. The ok arm replays.
    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    assert!(
        matches!(
            &verdict,
            IdempotencyVerdict::Unproven { obstacles }
                if matches!(
                    &obstacles[..],
                    [
                        IdempotencyObstacle::PathDecisionUnstable {
                            decision: verification::DecisionTaken::Match {
                                arm: ResultVariant::Err,
                                ..
                            },
                            gap: DecisionGap::ResultUnstable {
                                gap: ResultGap::ExternalErrorDispositionUnspecified,
                                ..
                            },
                            ..
                        },
                        IdempotencyObstacle::EffectInstanceRootUnstable { .. },
                    ]
                )
        ),
        "{verdict:?}"
    );

    // Without the branch, the declared deduplication completes the
    // proof.
    linearize_charge_payment(&mut model);

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected charge_payment proven, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].effects[0],
        EffectRetrySafety {
            safety: EffectSafety::ExternallyDeduplicated { key },
            ..
        } if matches!(key[0].rule, StabilityRule::KeyComponent)
    ));
}

#[test]
fn unstable_external_deduplication_key_is_an_obstacle() {
    let mut model = load_flash_checkout();

    let card = charge_card_mut(&mut model);

    card.idempotency = IdempotencyGuarantee::DeduplicatedBy {
        key: IdempotencyKey {
            components: vec![ValueRef {
                source: ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
                path: path(&["order_id"]),
            }],
        },
    };

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected charge_payment unproven, found {verdict:?}");
    };

    assert!(matches!(
        &obstacles[0],
        IdempotencyObstacle::ExternalDeduplicationKeyUnstable { roots, .. }
            if matches!(roots[0].gap, StabilityGap::MutableSubjectState { .. })
    ));
}

#[test]
fn a_terminal_error_disposition_completes_the_branching_charge_proof() {
    let mut model = load_flash_checkout();

    let card = charge_card_mut(&mut model);

    card.idempotency = IdempotencyGuarantee::DeduplicatedBy {
        key: ikey("input.charge_payment.reserved", &[&["event_id"]]),
    };

    card.result.as_mut().unwrap().err.disposition = ErrorDisposition::Terminal;

    assert!(validation::validate(&model).is_empty());

    // The provider deduplicates by a class-fixed key, which fixes each
    // charge's terminal result, and a decline terminally resolves it:
    // both arms of the match replay, the decline publication's payload
    // is class-fixed, and the whole branching program proves — no
    // linearization needed.
    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected charge_payment proven, found {verdict:?}");
    };

    assert_eq!(paths.len(), 2);

    for (path, variant) in paths.iter().zip([ResultVariant::Ok, ResultVariant::Err]) {
        assert!(
            matches!(
                &path.decisions[..],
                [verification::DecisionReplay {
                    decision: verification::DecisionTaken::Match { arm, .. },
                    rule: DecisionRule::StableResult {
                        rule: verification::ResultStabilityRule::ExternalTerminalResult {
                            variant: cited,
                            key,
                        },
                        ..
                    },
                }] if *arm == variant
                    && *cited == variant
                    && matches!(key[0].rule, StabilityRule::KeyComponent)
            ),
            "{path:#?}"
        );
    }
}

/// The external substrate of the terminal-result replay tests:
/// `transfer_stock` charges an external provider that deduplicates by
/// the governing `sku` key, matches the result, and returns each
/// terminal variant's payload from the corresponding bound result.
fn charge_transfer_externally(model: &mut Model, disposition: ErrorDisposition) {
    let operation = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    operation.program.steps = vec![
        execute(
            "effect.transfer_stock.charge",
            conseqa::spec::Effect::External(ExternalEffect {
                name: "payments.charge".into(),
                idempotency: IdempotencyGuarantee::DeduplicatedBy {
                    key: ikey("input.transfer_stock.request", &[&["sku"]]),
                },
                result: Some(ResultType {
                    ok: id("schema.ChargeAccepted"),
                    err: ErrorResultType {
                        schema: id("schema.ChargeDeclined"),
                        disposition,
                    },
                }),
            }),
            deterministic(vec![input_key("input.transfer_stock.request", &["sku"])]),
            Some("result.transfer_stock.charge"),
        ),
        OperationStep::MatchResult(MatchResult {
            result: id("result.transfer_stock.charge"),
            ok: block(vec![return_ok(
                "input.transfer_stock.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultOk(id("result.transfer_stock.charge")),
                    path: path(&["authorization_id"]),
                }]),
            )]),
            err: block(vec![return_err(
                "input.transfer_stock.request",
                deterministic(vec![ValueRef {
                    source: ValueSource::EffectResultErr(id("result.transfer_stock.charge")),
                    path: path(&["reason"]),
                }]),
            )]),
        }),
    ];

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            result: ResultReplayRequirement::ReplayConsistent,
        });
}

#[test]
fn an_idempotent_external_terminal_result_proves_result_replay() {
    let mut model = load_flash_checkout();

    charge_transfer_externally(&mut model, ErrorDisposition::Terminal);

    assert!(validation::validate(&model).is_empty());

    // The external key is class-fixed, so same-class attempts address
    // one logical charge whose terminal result the strengthened
    // `deduplicated_by` fixes; `Ok` is terminal by definition and the
    // decline is declared terminal, so both returning paths replay
    // their decision and derive their payload from a stable terminal
    // result. The external boundary no longer destroys request-result
    // replayability.
    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let ResultReplayVerdict::Proven {
        proof: ResultReplayProof::ClassFixedResult { returns },
        ..
    } = &verdict
    else {
        panic!("expected a class-fixed result, found {verdict:?}");
    };

    assert_eq!(returns.len(), 2);

    for (returned, variant) in returns.iter().zip([ResultVariant::Ok, ResultVariant::Err]) {
        assert_eq!(returned.variant, variant);

        assert!(matches!(
            &returned.decisions[..],
            [verification::DecisionReplay {
                rule: DecisionRule::StableResult {
                    rule: verification::ResultStabilityRule::ExternalTerminalResult {
                        variant: cited,
                        ..
                    },
                    ..
                },
                ..
            }] if *cited == variant
        ));

        assert!(matches!(
            &returned.derivation[..],
            [StableRoot {
                rule: StabilityRule::DeduplicatedExternalResult { result, effect, variant: cited },
                ..
            }] if result == &id("result.transfer_stock.charge")
                && effect == &id("effect.transfer_stock.charge")
                && *cited == variant
        ));
    }
}

#[test]
fn a_retryable_external_error_is_not_terminal_result_evidence() {
    let mut model = load_flash_checkout();

    charge_transfer_externally(&mut model, ErrorDisposition::Retryable);

    assert!(validation::validate(&model).is_empty());

    // A retryable decline conclusively ends one attempt without
    // terminally resolving the charge, so external idempotency does
    // not fix it: the err arm is not established to replay, and the
    // err payload is not a stable root. The ok path contributes no
    // obstacle — its terminal is fixed.
    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let ResultReplayVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert!(
        matches!(
            &obstacles[..],
            [
                ResultReplayObstacle::PathDecisionUnstable {
                    decision: verification::DecisionTaken::Match {
                        arm: ResultVariant::Err,
                        ..
                    },
                    gap: DecisionGap::ResultUnstable {
                        gap: ResultGap::ExternalErrorRetryable,
                        ..
                    },
                    ..
                },
                ResultReplayObstacle::ResultDerivationRootUnstable { roots, .. },
            ] if matches!(
                &roots[0].gap,
                StabilityGap::ResultUnstable { gap, .. }
                    if matches!(**gap, ResultGap::ExternalErrorRetryable)
            )
        ),
        "{obstacles:#?}"
    );
}

#[test]
fn an_unspecified_error_disposition_leaves_the_error_unknown() {
    let mut model = load_flash_checkout();

    charge_transfer_externally(&mut model, ErrorDisposition::Unspecified);

    assert!(validation::validate(&model).is_empty());

    // `unspecified` is epistemic: no usable fact says whether the
    // observed error terminally resolved the charge, so nothing is
    // promoted — and nothing is condemned.
    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let ResultReplayVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert!(
        obstacles.iter().any(|obstacle| matches!(
            obstacle,
            ResultReplayObstacle::PathDecisionUnstable {
                gap: DecisionGap::ResultUnstable {
                    gap: ResultGap::ExternalErrorDispositionUnspecified,
                    ..
                },
                ..
            }
        )),
        "{obstacles:#?}"
    );
}

#[test]
fn an_undeduplicated_external_result_gains_no_stability() {
    let mut model = load_flash_checkout();

    charge_transfer_externally(&mut model, ErrorDisposition::Terminal);

    let OperationStep::ExecuteEffect(step) =
        &mut program_mut(&mut model, "operation.transfer_stock").steps[0]
    else {
        panic!("expected the charge execute_effect step");
    };

    let Effect::External(charge) = &mut step.effect else {
        panic!("the charge should be an external effect");
    };

    charge.idempotency = IdempotencyGuarantee::Unspecified;

    assert!(validation::validate(&model).is_empty());

    // Without `deduplicated_by`, nothing identifies same-key
    // executions as one logical interaction, so no terminal result is
    // fixed — a declared terminal disposition alone proves nothing,
    // and even the ok arm does not replay.
    let verdict = result_replay_verdict(&model, "operation.transfer_stock", 0);

    let ResultReplayVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:?}");
    };

    assert!(
        obstacles.iter().any(|obstacle| matches!(
            obstacle,
            ResultReplayObstacle::PathDecisionUnstable {
                decision: verification::DecisionTaken::Match {
                    arm: ResultVariant::Ok,
                    ..
                },
                gap: DecisionGap::ResultUnstable {
                    gap: ResultGap::ExternalDeduplicationUnknown,
                    ..
                },
                ..
            }
        )),
        "{obstacles:#?}"
    );
}

#[test]
fn unidentified_publication_defeats_the_duplicate_discharge() {
    let mut model = load_flash_checkout();

    // The topic no longer identifies PaymentCaptured messages; the
    // partial mapping remains valid, but the capture publication loses
    // its same-logical-message argument.
    order_events_message_identity(&mut model).remove(&id("schema.PaymentCaptured"));

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected charge_payment unproven, found {verdict:?}");
    };

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        IdempotencyObstacle::PublicationNotIdentified { schema, .. }
            if schema == &id("schema.PaymentCaptured")
    )));
}

#[test]
fn single_delivery_discharges_idempotency_vacuously() {
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .delivery = conseqa::spec::DeliverySemantics::AtMostOnce;

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.reserve_inventory", 0);

    assert_eq!(
        verdict,
        IdempotencyVerdict::Proven {
            proof: IdempotencyProof::SingleDelivery {
                input: id("input.reserve_inventory.created"),
                topic: id("topic.order_events"),
            },
            // `at_most_once` is a delivery fact, and delivery is L1.
            scope: ProofScope::RuntimeDependent,
        }
    );
}

#[test]
fn request_discharge_needs_a_proven_target_through_the_fixpoint() {
    let mut model = load_flash_checkout();

    // create_order's own requirement proves only when its cascade
    // collapses: deliver OrderCreated to reserve_inventory at most
    // once, so the one logical message reaches it once.
    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .delivery = conseqa::spec::DeliverySemantics::AtMostOnce;

    // transfer_stock forwards a class-fixed request into create_order,
    // whose own idempotency requirement is proven; the fixpoint's
    // rounds discharge the cascade and then the request leg.
    let operation = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    operation.program.steps = vec![
        execute(
            "effect.transfer_stock.forward",
            conseqa::spec::Effect::Request(conseqa::spec::RequestEffect {
                target: conseqa::spec::RequestTarget {
                    operation: id("operation.create_order"),
                    input: id("input.create_order.request"),
                },
                schema: id("schema.CreateOrderRequest"),
                retry: conseqa::spec::RetrySemantics::MayRepeat,
                idempotency_key_propagation: vec![],
            }),
            deterministic(vec![input_key("input.transfer_stock.request", &["sku"])]),
            None,
        ),
        return_ok("input.transfer_stock.request", Derivation::Unspecified),
    ];

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            result: ResultReplayRequirement::Unspecified,
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.transfer_stock", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected transfer_stock proven, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].effects[..],
        [EffectRetrySafety {
            safety: EffectSafety::DeduplicatedByTarget { operation, input, .. },
            ..
        }] if operation == &id("operation.create_order")
            && input == &id("input.create_order.request")
    ));

    // Without the target's requirement, nothing collapses duplicate
    // invocations.
    model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .requirements
        .idempotency
        .clear();

    let verdict = idempotency_verdict(&model, "operation.transfer_stock", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected transfer_stock unproven, found {verdict:?}");
    };

    assert!(matches!(
        &obstacles[..],
        [IdempotencyObstacle::RequestTargetHasNoKeyedRequirement { operation, .. }]
            if operation == &id("operation.create_order")
    ));
}

#[test]
fn cyclic_request_dependencies_prove_coinductively() {
    let mut model = load_flash_checkout();

    // transfer_stock and cancel_order request each other; each passes
    // its local checks under the mutual assumption, so the greatest
    // fixpoint proves the cycle and marks both proofs coinductive.
    let transfer = model
        .operations
        .get_mut(&id("operation.transfer_stock"))
        .unwrap();

    transfer.program.steps = vec![
        execute(
            "effect.transfer_stock.cancel",
            conseqa::spec::Effect::Request(conseqa::spec::RequestEffect {
                target: conseqa::spec::RequestTarget {
                    operation: id("operation.cancel_order"),
                    input: id("input.cancel_order.request"),
                },
                schema: id("schema.CancelOrderRequest"),
                retry: conseqa::spec::RetrySemantics::Unspecified,
                idempotency_key_propagation: vec![],
            }),
            deterministic(vec![input_key("input.transfer_stock.request", &["sku"])]),
            None,
        ),
        return_ok("input.transfer_stock.request", Derivation::Unspecified),
    ];

    transfer
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.transfer_stock.request", &[&["sku"]]),
            result: ResultReplayRequirement::Unspecified,
        });

    let cancel = model
        .operations
        .get_mut(&id("operation.cancel_order"))
        .unwrap();

    cancel.program.steps = vec![
        execute(
            "effect.cancel_order.transfer",
            conseqa::spec::Effect::Request(conseqa::spec::RequestEffect {
                target: conseqa::spec::RequestTarget {
                    operation: id("operation.transfer_stock"),
                    input: id("input.transfer_stock.request"),
                },
                schema: id("schema.TransferStockRequest"),
                retry: conseqa::spec::RetrySemantics::Unspecified,
                idempotency_key_propagation: vec![],
            }),
            deterministic(vec![input_key("input.cancel_order.request", &["order_id"])]),
            None,
        ),
        return_ok("input.cancel_order.request", Derivation::Unspecified),
    ];

    cancel
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.cancel_order.request", &[&["order_id"]]),
            result: ResultReplayRequirement::Unspecified,
        });

    assert!(validation::validate(&model).is_empty());

    let report = verification::verify(&model);

    for operation in ["operation.transfer_stock", "operation.cancel_order"] {
        let check = report
            .idempotency
            .iter()
            .find(|check| check.operation == id(operation))
            .unwrap();

        assert!(
            matches!(check.verdict, IdempotencyVerdict::Proven { .. }),
            "expected `{operation}` proven, found {:?}",
            check.verdict
        );

        assert!(
            check.coinductive,
            "`{operation}` should be marked coinductive"
        );
    }

    // A member failing locally fails the cycle with it: with an
    // unspecified instance, cancel_order's request leg is unsafe, and
    // transfer_stock's target is no longer proven.
    let OperationStep::ExecuteEffect(forward) =
        &mut program_mut(&mut model, "operation.cancel_order").steps[0]
    else {
        panic!("expected the forwarded request");
    };

    forward.values = Derivation::Unspecified;

    let verdict = idempotency_verdict(&model, "operation.transfer_stock", 0);

    assert!(
        matches!(
            &verdict,
            IdempotencyVerdict::Unproven { obstacles }
                if matches!(&obstacles[..], [IdempotencyObstacle::RequestTargetRequirementUnproven { .. }])
        ),
        "expected transfer_stock unproven through its failed partner, found {verdict:?}"
    );
}

#[test]
fn publication_cascade_needs_collapsing_consumers_through_the_fixpoint() {
    let mut model = load_flash_checkout();

    // With the card charge deduplicated, charge_payment's remaining
    // leg is its cascade: PaymentCaptured reaches apply_payment, whose
    // own requirement the fixpoint proves in its first round, so the
    // second round discharges the publication.
    let card = charge_card_mut(&mut model);

    card.idempotency = IdempotencyGuarantee::DeduplicatedBy {
        key: ikey("input.charge_payment.reserved", &[&["event_id"]]),
    };

    linearize_charge_payment(&mut model);

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected charge_payment proven, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].effects[..],
        [
            EffectRetrySafety {
                safety: EffectSafety::ExternallyDeduplicated { .. },
                ..
            },
            EffectRetrySafety {
                safety: EffectSafety::SameLogicalMessage {
                    schema,
                    consumers,
                    ..
                },
                ..
            },
        ] if schema == &id("schema.PaymentCaptured")
            && consumers
                == &vec![ConsumerCollapse::ProvenRequirement {
                    operation: id("operation.apply_payment"),
                    input: id("input.apply_payment.captured"),
                }]
    ));

    // Without the consumer's requirement, nothing collapses the
    // duplicate deliveries a duplicate publication causes there.
    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .idempotency
        .clear();

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected charge_payment unproven, found {verdict:?}");
    };

    assert!(
        matches!(
            &obstacles[..],
            [IdempotencyObstacle::PublicationConsumerNotKeyed {
                schema,
                operation,
                input,
                ..
            }] if schema == &id("schema.PaymentCaptured")
                && operation == &id("operation.apply_payment")
                && input == &id("input.apply_payment.captured")
        ),
        "expected the unkeyed-consumer obstacle:\n{obstacles:#?}"
    );
}

#[test]
fn at_most_once_consumers_collapse_duplicates_by_delivery() {
    let mut model = load_flash_checkout();

    // An at-most-once consumer never sees a second delivery of the one
    // logical message, so it needs no requirement of its own.
    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .delivery = conseqa::spec::DeliverySemantics::AtMostOnce;

    model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap()
        .requirements
        .idempotency
        .clear();

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.create_order", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected create_order proven, found {verdict:?}");
    };

    assert!(matches!(
        &paths[0].effects[..],
        [EffectRetrySafety {
            safety: EffectSafety::SameLogicalMessage { consumers, .. },
            ..
        }] if consumers
            == &vec![ConsumerCollapse::SingleDelivery {
                operation: id("operation.reserve_inventory"),
                input: id("input.reserve_inventory.created"),
            }]
    ));
}

#[test]
fn cyclic_publication_dependencies_prove_coinductively() {
    let mut model = load_flash_checkout();

    // apply_payment also consumes the OrderPaid it publishes, so its
    // cascade collapses only if its own requirement is proven. Its
    // local checks pass under that assumption, so the greatest
    // fixpoint proves it and marks the proof coinductive.
    subscription_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .messages = MessageSelector::Only(
        [id("schema.PaymentCaptured"), id("schema.OrderPaid")]
            .into_iter()
            .collect(),
    );

    assert!(validation::validate(&model).is_empty());

    let check = verification::verify(&model)
        .idempotency
        .into_iter()
        .find(|check| check.operation == id("operation.apply_payment"))
        .unwrap();

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &check.verdict
    else {
        panic!("expected apply_payment proven, found {:?}", check.verdict);
    };

    assert!(
        check.coinductive,
        "the self-consuming proof should be marked coinductive"
    );

    assert!(
        paths[0].effects.iter().any(|effect| matches!(
            &effect.safety,
            EffectSafety::SameLogicalMessage { consumers, .. }
                if consumers.iter().any(|consumer| matches!(
                    consumer,
                    verification::ConsumerCollapse::ProvenRequirement { operation, .. }
                        if operation == &id("operation.apply_payment")
                ))
        )),
        "the proof should cite apply_payment as its own collapsing consumer:\n{paths:#?}"
    );
}

#[test]
fn guaranteed_completion_without_keyed_idempotency_is_noted() {
    let mut model = load_flash_checkout();

    let apply_payment = |model: &Model| {
        verification::verify(model)
            .recoverability
            .into_iter()
            .find(|check| check.operation == id("operation.apply_payment"))
            .expect("apply_payment declares recoverability")
    };

    // apply_payment guarantees completion through at-least-once
    // redelivery and declares those retries safe with an idempotency
    // requirement keyed from the same input: nothing to note.
    let check = apply_payment(&model);

    assert!(matches!(
        check.verdict,
        RecoverabilityVerdict::Proven {
            proof: RecoverabilityProof::Guaranteed { .. },
            ..
        }
    ));

    assert!(check.notes.is_empty());

    // Without that requirement the proof stands — recoverability is
    // progress only — but the expected retries have undeclared safety.
    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .idempotency
        .clear();

    assert!(validation::validate(&model).is_empty());

    let check = apply_payment(&model);

    assert!(matches!(
        check.verdict,
        RecoverabilityVerdict::Proven { .. }
    ));

    assert_eq!(
        check.notes,
        vec![RecoverabilityNote::RetrySafetyUndeclared {
            input: id("input.apply_payment.captured"),
        }]
    );

    let diagnostics = verification::verify(&model).diagnostics();

    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.code
                    == DiagnosticCode::Verification(
                        VerificationCode::RecoverabilityRetrySafetyUndeclared,
                    )
                && diagnostic.subject == Some(id("operation.apply_payment"))
        }),
        "expected the retry-safety warning:\n{diagnostics:#?}"
    );
}

#[test]
fn no_admitted_path_is_vacuously_idempotent() {
    let mut model = load_flash_checkout();

    // A second request input whose result the program never returns:
    // no path is admitted for it, so its attempts do no modeled work.
    let operation = model
        .operations
        .get_mut(&id("operation.cancel_order"))
        .unwrap();

    operation.inputs.insert(
        id("input.cancel_order.admin"),
        Input::Request(RequestInput {
            schema: id("schema.CancelOrderRequest"),
            identity: RequestIdentity::Unspecified,
            result: ResultType {
                ok: id("schema.CancelOrderResponse"),
                err: ErrorResultType {
                    schema: id("schema.RequestRejected"),
                    disposition: ErrorDisposition::Unspecified,
                },
            },
        }),
    );

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.cancel_order.admin", &[&["order_id"]]),
            result: ResultReplayRequirement::Unspecified,
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.cancel_order", 0);

    assert_eq!(
        verdict,
        IdempotencyVerdict::Proven {
            proof: IdempotencyProof::NoAdmittedPaths {
                input: id("input.cancel_order.admin"),
            },
            scope: ProofScope::L0Only,
        }
    );

    // Recoverability treats the same shape as an obstacle: progress is
    // impossible where safety is trivial.
    model
        .operations
        .get_mut(&id("operation.cancel_order"))
        .unwrap()
        .requirements
        .recoverability
        .push(RecoverabilityRequirement {
            key: ikey("input.cancel_order.admin", &[&["order_id"]]),
            completion: CompletionRequirement::Resumable,
        });

    assert_eq!(
        recoverability_verdict(&model, "operation.cancel_order", 0),
        RecoverabilityVerdict::Unproven {
            obstacles: vec![RecoverabilityObstacle::NoAdmittedPath {
                input: id("input.cancel_order.admin"),
            }],
        }
    );
}

#[test]
fn an_unstable_branch_decision_defeats_idempotency() {
    let mut model = load_flash_checkout();

    // apply_payment branches on the captured amount, working in one
    // arm and completing directly in the other. The topic identity
    // pins `amount`, so the decision replays and the proof records it.
    let program = program_mut(&mut model, "operation.apply_payment");

    let steps = std::mem::take(&mut program.steps);

    program.steps = vec![OperationStep::Branch(Branch {
        condition: Condition::Eq {
            value: input_key("input.apply_payment.captured", &["amount"]),
            equals: SelectorValue::Literal(Literal::Int(0)),
        },
        then: block(steps),
        otherwise: Some(block(vec![OperationStep::Complete])),
    })];

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.apply_payment", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected apply_payment proven, found {verdict:?}");
    };

    assert_eq!(paths.len(), 2);

    assert!(matches!(
        &paths[0].decisions[..],
        [verification::DecisionReplay {
            rule: DecisionRule::StableCondition { roots },
            ..
        }] if matches!(roots[0].rule, StabilityRule::IdentifiedPayload)
    ));

    // Without the identity, same-key deliveries may carry different
    // amounts and a retry may take the other arm.
    order_events_message_identity(&mut model).remove(&id("schema.PaymentCaptured"));

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            IdempotencyVerdict::Unproven { obstacles }
                if matches!(
                    &obstacles[..],
                    [IdempotencyObstacle::PathDecisionUnstable {
                        gap: DecisionGap::ConditionRootsUnstable { .. },
                        ..
                    }]
                )
        ),
        "{verdict:?}"
    );
}

#[test]
fn a_match_on_a_consistent_request_result_is_idempotent() {
    let mut model = load_flash_checkout();

    // create_order's own requirement proves only when its cascade
    // collapses.
    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .delivery = conseqa::spec::DeliverySemantics::AtMostOnce;

    forward_transfer_to_create_order(&mut model);

    assert!(validation::validate(&model).is_empty());

    // The request is collapsed by create_order's proven requirement,
    // and the match on its result replays because create_order proves
    // its result replay-consistent.
    let verdict = idempotency_verdict(&model, "operation.transfer_stock", 0);

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected transfer_stock proven, found {verdict:?}");
    };

    assert_eq!(paths.len(), 2);

    for path in paths {
        assert!(matches!(
            &path.effects[..],
            [EffectRetrySafety {
                safety: EffectSafety::DeduplicatedByTarget { .. },
                ..
            }]
        ));

        assert!(matches!(
            &path.decisions[..],
            [verification::DecisionReplay {
                rule: DecisionRule::StableResult { .. },
                ..
            }]
        ));
    }
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

fn ordering_verdict(
    model: &Model,
    operation: &str,
    requirement: usize,
) -> verification::OrderingVerdict {
    verification::verify(model)
        .ordering
        .into_iter()
        .find(|check| check.operation == id(operation) && check.requirement == requirement)
        .unwrap_or_else(|| panic!("no ordering check for `{operation}` #{requirement}"))
        .verdict
}

#[test]
fn flash_checkout_ordering_verdicts() {
    let model = load_flash_checkout();

    let report = verification::verify(&model);

    assert_eq!(report.ordering.len(), 3);

    // apply_payment: the keyed topic runtime is the precedence,
    // topic_key routing onto a serial member preserves it through
    // order-preserving redelivery, and its proven idempotency
    // requirement answers for duplicates.
    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    let verification::OrderingVerdict::Proven {
        proof:
            verification::OrderingProof::RoutedOrder {
                input,
                topic,
                pool,
                precedence,
                routing_key,
                member_assignment,
                duplicates,
                ..
            },
        scope,
    } = &verdict
    else {
        panic!("expected apply_payment proven, found {verdict:?}");
    };

    assert_eq!(input, &id("input.apply_payment.captured"));
    assert_eq!(topic, &id("topic.order_events"));
    assert_eq!(pool, &id("pool.order_workers"));
    assert_eq!(*routing_key, SubscriptionRoutingKey::GroupingKey);
    assert_eq!(*member_assignment, MemberAssignment::ConsistentHash);

    // Precedence, routing, and member concurrency are all L1 facts.
    assert_eq!(*scope, ProofScope::RuntimeDependent);

    assert_eq!(*precedence, verification::PrecedenceSource::WithinGroup);

    assert_eq!(
        *duplicates,
        verification::DuplicateHandling::OrderPreservingRedelivery {
            idempotency: Some(verification::DuplicateCoverage {
                requirement: 0,
                proven: true,
            }),
        }
    );

    // reserve_inventory and charge_payment prove by the same facts; the
    // proofs record that the requirements answering for duplicates are
    // unproven — the gap is reported under idempotency, not here.
    for operation in ["operation.reserve_inventory", "operation.charge_payment"] {
        let verdict = ordering_verdict(&model, operation, 0);

        assert!(
            matches!(
                &verdict,
                verification::OrderingVerdict::Proven {
                    proof: verification::OrderingProof::RoutedOrder {
                        duplicates: verification::DuplicateHandling::OrderPreservingRedelivery {
                            idempotency: Some(verification::DuplicateCoverage {
                                requirement: 0,
                                proven: false,
                            }),
                        },
                        ..
                    },
                    ..
                }
            ),
            "expected `{operation}` proven with unproven duplicate coverage, found {verdict:?}"
        );
    }
}

#[test]
fn at_most_once_delivery_records_single_delivery() {
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.reserve_inventory",
        "input.reserve_inventory.created",
    )
    .delivery = conseqa::spec::DeliverySemantics::AtMostOnce;

    let verdict = ordering_verdict(&model, "operation.reserve_inventory", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Proven {
                proof: verification::OrderingProof::RoutedOrder {
                    duplicates: verification::DuplicateHandling::SingleDelivery,
                    ..
                },
                ..
            }
        ),
        "expected reserve_inventory proven by single delivery, found {verdict:?}"
    );
}

#[test]
fn request_routing_does_not_invent_request_ordering() {
    // A router keyed exactly like the requirement, on a serial pool,
    // establishes serialization — and still no ordering, because
    // arrival order of unmodeled callers is not a logical precedence.
    let mut model = load_flash_checkout();

    let requirements = &mut model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .requirements;

    requirements.serialization.push(SerializationRequirement {
        key: input_key("input.create_order.request", &["order_id"]),
    });

    requirements
        .ordering
        .push(conseqa::spec::OrderingRequirement {
            key: input_key("input.create_order.request", &["order_id"]),
        });

    pool_mut(&mut model, "pool.checkout_api").member_concurrency = bounded(1);

    assert!(validation::validate(&model).is_empty());

    assert!(
        matches!(
            serialization_verdict(&model, "operation.create_order", 0),
            SerializationVerdict::Proven {
                proof: SerializationProof::RequestRouted { .. },
                ..
            }
        ),
        "the same facts do prove serialization"
    );

    let verdict = ordering_verdict(&model, "operation.create_order", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if matches!(&obstacles[..], [verification::OrderingObstacle::RequestInputHasNoPrecedenceSource { .. }])
        ),
        "{verdict:?}"
    );
}

#[test]
fn ordering_key_not_carrying_the_topic_key_inherits_no_precedence() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .ordering[0]
        .key = input_key("input.apply_payment.captured", &["event_id"]);

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if matches!(
                    &obstacles[..],
                    [verification::OrderingObstacle::KeyIdentityUnestablished { schema, .. }]
                        if schema == &id("schema.PaymentCaptured")
                )
        ),
        "{verdict:?}"
    );
}

#[test]
fn member_concurrency_above_one_admits_overtaking() {
    let mut model = load_flash_checkout();

    pool_mut(&mut model, "pool.order_workers").member_concurrency =
        MemberConcurrency::Unbounded;

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if matches!(&obstacles[..], [verification::OrderingObstacle::MemberConcurrencyNotSerial { .. }])
        ),
        "{verdict:?}"
    );
}

#[test]
fn dispatch_without_routing_preserves_no_order() {
    let mut model = load_flash_checkout();

    subscription_runtime_mut(
        &mut model,
        "operation.apply_payment",
        "input.apply_payment.captured",
    )
    .dispatch
    .routing = None;

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if matches!(&obstacles[..], [verification::OrderingObstacle::RoutingAbsent { .. }])
        ),
        "{verdict:?}"
    );
}

#[test]
fn a_global_transport_orders_any_key_the_grouping_keeps_together() {
    // Global precedence plus a grouping that matches the requirement
    // key composes soundly: the transport orders everything, and the
    // grouping keeps same-key deliveries on one member so the order
    // survives into execution. Separating the two facts is what made
    // `global` usable — it no longer has to carry a key of its own.
    let mut model = load_flash_checkout();

    topic_runtime_mut(&mut model, "topic.order_events").ordering = Some(OrderingSemantics::Global);

    assert!(validation::validate(&model).is_empty());

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    let verification::OrderingVerdict::Proven {
        proof: verification::OrderingProof::RoutedOrder { precedence, .. },
        ..
    } = &verdict
    else {
        panic!("expected a proven ordering verdict, found {verdict:?}");
    };

    assert_eq!(*precedence, verification::PrecedenceSource::Global);
}

#[test]
fn a_global_transport_still_needs_the_grouping_to_reach_execution() {
    // The regression the pairing rule guarded: a precedence over one
    // domain composed with routing over another proves nothing. Now
    // that grouping is a separate fact, the guard is structural — the
    // ordering proof consumes the same grouping identity serialization
    // does, whichever precedence supplied the order.
    let mut model = load_flash_checkout();

    topic_runtime_mut(&mut model, "topic.order_events").ordering = Some(OrderingSemantics::Global);

    model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap()
        .requirements
        .ordering = vec![conseqa::spec::OrderingRequirement {
        key: input_key("input.reserve_inventory.created", &["quantity"]),
    }];

    assert!(validation::validate(&model).is_empty());

    let verdict = ordering_verdict(&model, "operation.reserve_inventory", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if matches!(
                    &obstacles[..],
                    [verification::OrderingObstacle::KeyIdentityUnestablished { .. }]
                )
        ),
        "a global order does not reach a key the grouping does not keep together: {verdict:?}"
    );
}

#[test]
fn a_transport_with_no_precedence_provides_none() {
    let mut model = load_flash_checkout();

    topic_runtime_mut(&mut model, "topic.order_events").ordering = Some(OrderingSemantics::None);

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    verification::OrderingObstacle::NoTransportPrecedence { .. }
                ))
        ),
        "{verdict:?}"
    );
}

#[test]
fn a_topic_with_no_declared_runtime_provides_no_precedence() {
    // Absence of any transport declaration leaves both facts absent,
    // and the topic in neither scope.
    let mut model = load_flash_checkout();

    runtime_mut(&mut model)
        .topics
        .remove(&id("topic.order_events"));

    let verdict = ordering_verdict(&model, "operation.apply_payment", 0);

    assert!(
        matches!(
            &verdict,
            verification::OrderingVerdict::Unproven { obstacles }
                if obstacles.iter().any(|obstacle| matches!(
                    obstacle,
                    verification::OrderingObstacle::NoTransportPrecedence { .. }
                ))
        ),
        "{verdict:?}"
    );
}

#[test]
fn subscription_scoped_transport_semantics_prove_the_same_way() {
    // The second declaration mode: the topic declares nothing, and each
    // subscription states its own grouping and ordering. Two
    // subscribers of one topic may group differently — the case a
    // single topic-wide domain could not express.
    let mut model = load_flash_checkout();

    let topic = topic_runtime_mut(&mut model, "topic.order_events");

    // Clearing the scope means declaring nothing — not declaring an
    // explicit `none`, which would itself claim the topic scope.
    topic.grouping = None;
    topic.ordering = None;

    let by_order_id = conseqa::spec::GroupingKey {
        mapping: [
            "schema.InventoryReserved",
            "schema.OrderCancelled",
            "schema.OrderCreated",
            "schema.OrderPaid",
            "schema.PaymentCaptured",
            "schema.PaymentFailed",
        ]
        .into_iter()
        .map(|schema| (id(schema), vec![path(&["order_id"])]))
        .collect(),
    };

    for (operation, input) in [
        ("operation.reserve_inventory", "input.reserve_inventory.created"),
        ("operation.charge_payment", "input.charge_payment.reserved"),
        ("operation.apply_payment", "input.apply_payment.captured"),
    ] {
        let subscription = subscription_runtime_mut(&mut model, operation, input);

        subscription.grouping = Some(by_order_id.clone());
        subscription.ordering = Some(OrderingSemantics::WithinGroup);
    }

    assert!(
        validation::validate(&model).is_empty(),
        "{:#?}",
        validation::validate(&model)
    );

    // Every serialization and ordering obligation still proves, now
    // citing the subscription scope rather than the topic.
    let verdict = serialization_verdict(&model, "operation.reserve_inventory", 0);

    let SerializationVerdict::Proven {
        proof: SerializationProof::SubscriptionRouted { grouping_scope, .. },
        ..
    } = &verdict
    else {
        panic!("expected a subscription-routed proof, found {verdict:?}");
    };

    assert_eq!(
        *grouping_scope,
        verification::GroupingScope::Subscription {
            operation: id("operation.reserve_inventory"),
            input: id("input.reserve_inventory.created"),
        }
    );

    assert!(matches!(
        ordering_verdict(&model, "operation.apply_payment", 0),
        verification::OrderingVerdict::Proven { .. }
    ));
}

// ---------------------------------------------------------------------------
// Model notes and lineage
// ---------------------------------------------------------------------------

#[test]
fn duplicate_delivery_without_a_keyed_requirement_is_noted() {
    let mut model = load_flash_checkout();

    assert!(verification::verify(&model).notes.is_empty());

    model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .requirements
        .idempotency
        .clear();

    let report = verification::verify(&model);

    assert!(
        matches!(
            &report.notes[..],
            [verification::ModelNote::DuplicateDeliveryUnchecked { operation, input, .. }]
                if operation == &id("operation.apply_payment")
                    && input == &id("input.apply_payment.captured")
        ),
        "{:#?}",
        report.notes
    );

    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.severity == conseqa::analyzer::Severity::Warning
            && matches!(
                diagnostic.code,
                conseqa::analyzer::DiagnosticCode::Verification(
                    conseqa::analyzer::VerificationCode::DuplicateDeliveryUnchecked
                )
            )
    }));
}

#[test]
fn consumer_checks_record_producer_lineage() {
    let mut model = load_flash_checkout();

    let lineage = |model: &Model| {
        verification::verify(model)
            .idempotency
            .into_iter()
            .find(|check| check.operation == id("operation.apply_payment"))
            .unwrap()
            .lineage
    };

    // charge_payment propagates its governing key onto PaymentCaptured's
    // identity, so apply_payment's population rests on a carried key.
    let facts = lineage(&model);

    assert!(
        matches!(
            &facts[..],
            [verification::IdentityLineage {
                schema,
                producer: verification::ProducerRef::Operation { operation, effect },
                fact: verification::LineageFact::Propagated { requirement: Some(0), .. },
                ..
            }] if schema == &id("schema.PaymentCaptured")
                && operation == &id("operation.charge_payment")
                && effect == &id("effect.charge_payment.publish_captured")
        ),
        "{facts:#?}"
    );

    // Without the declaration the identity rests on the topic alone.
    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    let OperationStep::ExecuteEffect(captured) = &mut matched.ok.steps[0] else {
        panic!("expected the capture publication");
    };

    let Effect::Publication(publication) = &mut captured.effect else {
        panic!("publish_captured should be a publication");
    };

    publication.idempotency_key_propagation.clear();

    let facts = lineage(&model);

    assert!(
        matches!(
            &facts[..],
            [verification::IdentityLineage {
                fact: verification::LineageFact::Undeclared,
                ..
            }]
        ),
        "{facts:#?}"
    );
}

// ---------------------------------------------------------------------------
// Transition transactions without keyed deduplication
// ---------------------------------------------------------------------------

#[test]
fn a_transition_without_keyed_recovery_is_unknown_not_invalid() {
    let mut model = load_flash_checkout();

    // The transaction is structurally valid with the explicit negative
    // guarantee; what it loses is every replay route, so the
    // obligations over it settle unproven with the missing facts
    // recorded — never as a validation error.
    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment").idempotency =
        IdempotencyGuarantee::NotDeduplicated;

    assert!(validation::validate(&model).is_empty());

    let verdict = idempotency_verdict(&model, "operation.apply_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected apply_payment unproven, found {verdict:?}");
    };

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        IdempotencyObstacle::TransactionNotRetrySafe {
            transaction,
            recovery,
            reconstruction,
            ..
        } if transaction == &id("tx.apply_payment")
            && recovery == &vec![ReplayGap::NoKeyedCommit]
            && reconstruction.contains(&ReplayGap::ContainsTransition)
    )));

    // The transition-established intent is replay-available by neither
    // route, so its execution is not class-fixed either.
    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        IdempotencyObstacle::IntentNotReplayAvailable { intent, .. }
            if intent == &id("intent.apply_payment.order_paid")
    )));

    let verdict = recoverability_verdict(&model, "operation.apply_payment", 0);

    let RecoverabilityVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected apply_payment recoverability unproven, found {verdict:?}");
    };

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        RecoverabilityObstacle::TransactionNotResolvable { transaction, .. }
            if transaction == &id("tx.apply_payment")
    )));

    assert!(obstacles.iter().any(|obstacle| matches!(
        obstacle,
        RecoverabilityObstacle::ArtifactNotReplayAvailable { artifact, .. }
            if artifact == &id("intent.apply_payment.order_paid")
    )));
}

#[test]
fn a_transition_established_output_without_recovery_defeats_result_replay() {
    let mut model = load_flash_checkout();

    // A transition transaction with no keyed commit exports an output
    // the terminal returns: structurally valid, and the result-replay
    // obligation over it is unknown because the output is
    // replay-available by neither route.
    let transaction = transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order");

    transaction.idempotency = IdempotencyGuarantee::Unspecified;

    transaction
        .steps
        .push(TransactionStep::EstablishTransactionOutput(
            EstablishTransactionOutput {
                bind: id("output.cancel_order"),
                schema: id("schema.CancelOrderResponse"),
                values: deterministic(vec![input_key("input.cancel_order.request", &["order_id"])]),
            },
        ));

    let operation = model
        .operations
        .get_mut(&id("operation.cancel_order"))
        .unwrap();

    operation.program.steps[2] = return_ok(
        "input.cancel_order.request",
        deterministic(vec![output_ref("output.cancel_order", &["order_id"])]),
    );

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: ikey("input.cancel_order.request", &[&["order_id"]]),
            result: ResultReplayRequirement::ReplayConsistent,
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = result_replay_verdict(&model, "operation.cancel_order", 0);

    let (recovery, reconstruction) = unavailable_result_root(&verdict);

    assert_eq!(recovery, &vec![ReplayGap::NoKeyedCommit]);
    assert!(reconstruction.contains(&ReplayGap::ContainsTransition));
}

// Asynchronous effect execution: launches are ordinary effect attempts,
// join_all surfaces launch-time result judgments, race is conservative.

/// Restructures charge_payment's opening card charge into an async
/// launch joined back under the original result binding: the
/// asynchronous form of the same program.
fn asyncify_charge_payment(model: &mut Model) {
    let program = program_mut(model, "operation.charge_payment");

    let OperationStep::ExecuteEffect(card) = program.steps.remove(0) else {
        panic!("expected the card-charge execute_effect step");
    };

    program.steps.insert(
        0,
        OperationStep::ExecuteEffectAsync(ExecuteEffectAsync {
            handle: id("async.charge_payment.card"),
            effect_id: card.effect_id,
            effect: card.effect,
            values: card.values,
        }),
    );

    program.steps.insert(
        1,
        OperationStep::JoinAll(JoinAll {
            handles: vec![AsyncJoin {
                handle: id("async.charge_payment.card"),
                bind: card.bind,
            }],
        }),
    );
}

/// Rebuilds charge_payment's opening into a hedged pair of async card
/// charges — both deduplicated by the delivery's event id, declines
/// terminal, so each candidate's result is individually replay-stable
/// — synchronized by the step the caller chooses at position 2.
fn hedge_charge_payment(model: &mut Model, synchronize: OperationStep) {
    {
        let card = charge_card_mut(model);

        card.idempotency = IdempotencyGuarantee::DeduplicatedBy {
            key: IdempotencyKey {
                components: vec![input_key("input.charge_payment.reserved", &["event_id"])],
            },
        };

        card.result
            .as_mut()
            .expect("card charge declares a result")
            .err
            .disposition = ErrorDisposition::Terminal;
    }

    let program = program_mut(model, "operation.charge_payment");

    let OperationStep::ExecuteEffect(card) = program.steps.remove(0) else {
        panic!("expected the card-charge execute_effect step");
    };

    program.steps.insert(
        0,
        OperationStep::ExecuteEffectAsync(ExecuteEffectAsync {
            handle: id("async.charge_payment.primary"),
            effect_id: card.effect_id,
            effect: card.effect.clone(),
            values: card.values.clone(),
        }),
    );

    program.steps.insert(
        1,
        OperationStep::ExecuteEffectAsync(ExecuteEffectAsync {
            handle: id("async.charge_payment.hedge"),
            effect_id: id("effect.charge_payment.card_hedge"),
            effect: card.effect,
            values: card.values,
        }),
    );

    program.steps.insert(2, synchronize);
}

#[test]
fn async_launch_and_join_judge_like_the_synchronous_execution() {
    // Not waiting removes nothing: the launch is the same effect
    // attempt, and the joined binding carries the judgment the effect
    // would have had synchronously — so the async restructure raises
    // exactly the synchronous program's obstacles.
    let mut model = load_flash_checkout();

    asyncify_charge_payment(&mut model);

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "{errors:#?}");

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected charge_payment unproven, found {verdict:?}");
    };

    assert!(
        matches!(
            &obstacles[..],
            [
                IdempotencyObstacle::ExternalEffectNotDeduplicated { effect, .. },
                IdempotencyObstacle::PathDecisionUnstable {
                    decision: verification::DecisionTaken::Match { result, arm: ResultVariant::Ok, .. },
                    gap: DecisionGap::ResultUnstable {
                        gap: ResultGap::ExternalNotDeduplicated,
                        ..
                    },
                    ..
                },
                IdempotencyObstacle::EffectInstanceRootUnstable { effect: failed, roots, .. },
            ] if effect == &id("effect.charge_payment.card")
                && result == &id("result.charge_payment.card")
                && failed == &id("effect.charge_payment.publish_failed")
                && matches!(roots[0].gap, StabilityGap::ResultUnstable { .. })
        ),
        "{obstacles:#?}"
    );
}

#[test]
fn joining_hedged_deduplicated_charges_proves_idempotency() {
    // Both candidates address one logical external interaction whose
    // terminal result is fixed, and the join surfaces the primary's
    // launch-time judgment unchanged: the requirement proves.
    let mut model = load_flash_checkout();

    hedge_charge_payment(
        &mut model,
        OperationStep::JoinAll(JoinAll {
            handles: vec![
                AsyncJoin {
                    handle: id("async.charge_payment.primary"),
                    bind: Some(id("result.charge_payment.card")),
                },
                AsyncJoin {
                    handle: id("async.charge_payment.hedge"),
                    bind: None,
                },
            ],
        }),
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "{errors:#?}");

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    assert!(
        matches!(verdict, IdempotencyVerdict::Proven { .. }),
        "expected charge_payment proven, found {verdict:?}"
    );
}

#[test]
fn a_race_bound_result_is_conservatively_replay_unstable() {
    // The same launches, raced instead of joined: even though each
    // candidate's result is individually replay-stable, which one
    // completes first is scheduling nondeterminism, so the bound
    // result — and everything resting on it — is not established to
    // replay.
    let mut model = load_flash_checkout();

    hedge_charge_payment(
        &mut model,
        OperationStep::Race(Race {
            handles: vec![
                id("async.charge_payment.primary"),
                id("async.charge_payment.hedge"),
            ],
            bind: Some(id("result.charge_payment.card")),
        }),
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "{errors:#?}");

    let verdict = idempotency_verdict(&model, "operation.charge_payment", 0);

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected charge_payment unproven, found {verdict:?}");
    };

    assert!(
        matches!(
            &obstacles[..],
            [
                IdempotencyObstacle::PathDecisionUnstable {
                    decision: verification::DecisionTaken::Match { result, .. },
                    gap: DecisionGap::ResultUnstable {
                        gap: ResultGap::RaceWinnerNondeterministic { candidates },
                        ..
                    },
                    ..
                },
                IdempotencyObstacle::EffectInstanceRootUnstable { effect: failed, roots, .. },
            ] if result == &id("result.charge_payment.card")
                && candidates
                    == &vec![
                        id("effect.charge_payment.card"),
                        id("effect.charge_payment.card_hedge"),
                    ]
                && failed == &id("effect.charge_payment.publish_failed")
                && matches!(roots[0].gap, StabilityGap::ResultUnstable { .. })
        ),
        "{obstacles:#?}"
    );
}

#[test]
fn fire_and_forget_launches_stay_in_the_effect_cascade() {
    // Launching the capture publication without ever joining it leaves
    // the operation's judgment exactly where the synchronous execution
    // left it: the launch is in the blast radius, and the terminal
    // neither joins nor absolves it.
    let mut sync_model = load_flash_checkout();

    linearize_charge_payment(&mut sync_model);

    let mut async_model = load_flash_checkout();

    linearize_charge_payment(&mut async_model);

    {
        let program = program_mut(&mut async_model, "operation.charge_payment");

        let OperationStep::ExecuteEffect(publish) = program.steps.remove(1) else {
            panic!("expected the capture publication");
        };

        program.steps.insert(
            1,
            OperationStep::ExecuteEffectAsync(ExecuteEffectAsync {
                handle: id("async.charge_payment.captured"),
                effect_id: publish.effect_id,
                effect: publish.effect,
                values: publish.values,
            }),
        );
    }

    let errors = validation::validate(&async_model);

    assert!(errors.is_empty(), "{errors:#?}");

    assert_eq!(
        idempotency_verdict(&sync_model, "operation.charge_payment", 0),
        idempotency_verdict(&async_model, "operation.charge_payment", 0),
    );
}
