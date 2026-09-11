use std::{
    fs,
    path::{Path, PathBuf},
};

use conseqa::{
    parser::yaml,
    spec::{
        CompletionRequirement, Condition, DeliverySemantics, Derivation, Effect, ErrorDisposition,
        ErrorResultType, ExternalIdempotency, ExternalIdentity, ExternalResultReplay, Field,
        FieldPath, Id, IdempotencyGuarantee, Input, Literal,
        MemberAssignment, MemberConcurrency, MessageIdentity, Model, OperationStep,
        RequestIdentity, ResultOutcome, ResultVariant, ScalarType, Schema, SchemaCompleteness,
        SelectorValue, ServiceKind, SubscriptionRoutingKey, OrderingSemantics, Transaction,
        TransactionStep, TransitionSideEffect, TypeRef, ValueSource,
    },
};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    let path = fixture_path(name);

    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture `{}`: {error}", path.display()))
}

/// The inline transaction of an operation's program, by its stable ID.
fn transaction<'a>(model: &'a Model, operation: &str, transaction: &str) -> &'a Transaction {
    model
        .operations
        .get(&Id(operation.to_owned()))
        .unwrap_or_else(|| panic!("`{operation}` should exist"))
        .program
        .transaction(&Id(transaction.to_owned()))
        .unwrap_or_else(|| {
            panic!("`{operation}` should declare inline transaction `{transaction}`")
        })
}

/// An operation-owned inline effect declaration, by its inline id.
fn effect<'a>(model: &'a Model, operation: &str, effect: &str) -> &'a Effect {
    let wanted = Id(effect.to_owned());

    model
        .operations
        .get(&Id(operation.to_owned()))
        .unwrap_or_else(|| panic!("`{operation}` should exist"))
        .program
        .effect_declarations()
        .into_iter()
        .find_map(|(id, declared)| (id == &wanted).then_some(declared))
        .unwrap_or_else(|| panic!("`{operation}` should declare inline effect `{effect}`"))
}

#[test]
fn parses_minimal_model() {
    let source = read_fixture("minimal.yaml");

    let model = yaml::parse(&source).expect("minimal fixture should parse");

    assert_eq!(model.revision.0, 1);

    assert_eq!(model.services.len(), 1);
    assert_eq!(model.schemas.len(), 1);
    assert_eq!(model.topics.len(), 1);

    assert!(model.data_models.is_empty());
    assert!(model.state_machines.is_empty());
    assert!(model.operations.is_empty());

    let checkout = model
        .services
        .get(&Id("checkout".into()))
        .expect("checkout service should exist");

    assert_eq!(checkout.kind, ServiceKind::Backend);

    let order_created = model
        .schemas
        .get(&Id("OrderCreated".into()))
        .expect("OrderCreated schema should exist");

    let Schema::Canonical(order_created) = order_created else {
        panic!("OrderCreated should be a canonical schema");
    };

    assert_eq!(order_created.completeness, SchemaCompleteness::Complete);

    assert!(order_created.fields.contains_key("order_id"));
    assert!(order_created.fields.contains_key("quantity"));

    let order_events = model
        .topics
        .get(&Id("order_events".into()))
        .expect("order_events topic should exist");

    assert!(order_events.messages.contains(&Id("OrderCreated".into())));

    // Transport facts are L1: the logical channel says what it carries,
    // the runtime says how it groups and orders.
    let runtime = model
        .topic_runtime(&Id("order_events".into()))
        .expect("the fixture declares a topic runtime");

    assert_eq!(runtime.ordering, None);
    assert!(runtime.grouping.is_none());
}

#[test]
fn parses_keyed_topic_model() {
    let source = read_fixture("keyed_topic.yaml");

    let model = yaml::parse(&source).expect("keyed topic fixture should parse");

    assert_eq!(model.revision.0, 2);

    assert_eq!(model.services.len(), 2);
    assert_eq!(model.schemas.len(), 2);
    assert_eq!(model.topics.len(), 1);

    let checkout = model
        .services
        .get(&Id("checkout".into()))
        .expect("checkout service should exist");

    assert_eq!(checkout.kind, ServiceKind::Backend);

    let payments = model
        .services
        .get(&Id("payments".into()))
        .expect("payments service should exist");

    assert_eq!(payments.kind, ServiceKind::Worker);

    let order_event = model
        .schemas
        .get(&Id("OrderEvent".into()))
        .expect("OrderEvent schema should exist");

    let Schema::Canonical(order_event) = order_event else {
        panic!("OrderEvent should be canonical");
    };

    assert_eq!(order_event.completeness, SchemaCompleteness::Complete);

    assert!(order_event.fields.contains_key("order_id"));
    assert!(order_event.fields.contains_key("event_id"));
    assert!(order_event.fields.contains_key("event_type"));

    let order_identity = model
        .schemas
        .get(&Id("OrderIdentity".into()))
        .expect("OrderIdentity schema should exist");

    let Schema::Fragment(order_identity) = order_identity else {
        panic!("OrderIdentity should be a fragment");
    };

    assert_eq!(order_identity.source, Id("OrderEvent".into()));

    let order_id_mapping = order_identity
        .mapping
        .get("order_id")
        .expect("fragment should map order_id");

    assert_eq!(order_id_mapping.0, vec!["order_id".to_string()]);

    let topic = model
        .topics
        .get(&Id("order_events".into()))
        .expect("order_events topic should exist");

    assert_eq!(
        topic.messages,
        [Id("OrderEvent".into())].into_iter().collect()
    );

    let runtime = model
        .topic_runtime(&Id("order_events".into()))
        .expect("the fixture declares a topic runtime");

    assert_eq!(runtime.ordering, Some(OrderingSemantics::WithinGroup));

    let key = runtime
        .grouping
        .as_ref()
        .expect("order_events should declare a keyed grouping");

    let order_event_key = key
        .mapping
        .get(&Id("OrderEvent".into()))
        .expect("OrderEvent should define its grouping key");

    assert_eq!(order_event_key, &vec![FieldPath(vec!["order_id".to_string()])]);

    // The grouping key and the message identity are separate
    // declarations: order_id groups events for an order, event_id
    // identifies one logical message.
    let MessageIdentity::Keyed(identity) = &topic.message_identity else {
        panic!("order_events should declare a keyed message identity");
    };

    let order_event_identity = identity
        .mapping
        .get(&Id("OrderEvent".into()))
        .expect("OrderEvent should define its message identity");

    assert_eq!(order_event_identity.len(), 1);
    assert_eq!(order_event_identity[0].0, vec!["event_id".to_string()]);
}

#[test]
fn flash_checkout_parses_stimulus_identities() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let input = model
        .operations
        .get(&Id("operation.create_order".into()))
        .expect("create_order should exist")
        .inputs
        .get(&Id("input.create_order.request".into()))
        .expect("create_order request should exist");

    let Input::Request(request) = input else {
        panic!("create_order input should be a request");
    };

    let RequestIdentity::Keyed(identity) = &request.identity else {
        panic!("create_order request should declare a keyed identity");
    };

    assert_eq!(identity.fields.len(), 1);
    assert_eq!(identity.fields[0].0, vec!["idempotency_key".to_string()]);

    let topic = model
        .topics
        .get(&Id("topic.order_events".into()))
        .expect("order_events topic should exist");

    let MessageIdentity::Keyed(identity) = &topic.message_identity else {
        panic!("order_events should declare a keyed message identity");
    };

    // Every carried schema is identified by its event_id.
    assert_eq!(identity.mapping.len(), 6);

    for tuple in identity.mapping.values() {
        assert_eq!(tuple.len(), 1);
        assert_eq!(tuple[0].0, vec!["event_id".to_string()]);
    }
}

#[test]
fn serializes_and_reparses_minimal_model() {
    let source = read_fixture("minimal.yaml");

    let original = yaml::parse(&source).expect("minimal fixture should parse");

    let serialized = yaml::serialize(&original).expect("model should serialize");

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(original, reparsed);
}

#[test]
fn serializes_and_reparses_keyed_topic_model() {
    let source = read_fixture("keyed_topic.yaml");

    let original = yaml::parse(&source).expect("keyed topic fixture should parse");

    let serialized = yaml::serialize(&original).expect("model should serialize");

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(original, reparsed);
}

#[test]
fn serialization_is_canonical() {
    let source = read_fixture("keyed_topic.yaml");

    let model = yaml::parse(&source).expect("fixture should parse");

    let first = yaml::serialize(&model).expect("model should serialize");

    let reparsed = yaml::parse(&first).expect("serialized model should parse");

    let second = yaml::serialize(&reparsed).expect("reparsed model should serialize");

    assert_eq!(
        first, second,
        "serializing a canonical model twice should produce identical YAML"
    );
}

#[test]
fn rejects_invalid_service_kind() {
    let source = read_fixture("invalid_service_kind.yaml");

    let error = yaml::parse(&source).expect_err("invalid service kind should fail to deserialize");

    let message = error.to_string();

    assert!(
        message.contains("definitely_not_a_service"),
        "error should mention the invalid value, got: {message}"
    );
}

#[test]
fn parses_flash_checkout_model() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    assert_eq!(model.revision.0, 1);

    assert_eq!(model.services.len(), 3);
    assert_eq!(model.schemas.len(), 17);
    assert_eq!(model.data_models.len(), 2);
    assert_eq!(model.topics.len(), 1);
    assert_eq!(model.state_machines.len(), 1);
    assert_eq!(model.operations.len(), 6);

    assert!(
        model
            .operations
            .contains_key(&Id("operation.create_order".into()))
    );

    assert!(
        model
            .operations
            .contains_key(&Id("operation.reserve_inventory".into()))
    );

    assert!(
        model
            .operations
            .contains_key(&Id("operation.charge_payment".into()))
    );

    assert!(
        model
            .operations
            .contains_key(&Id("operation.cancel_order".into()))
    );

    assert!(
        model
            .operations
            .contains_key(&Id("operation.apply_payment".into()))
    );

    assert!(
        model
            .operations
            .contains_key(&Id("operation.transfer_stock".into()))
    );
}

#[test]
fn flash_checkout_parses_nested_semantics() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    // The subscription input is purely logical: a topic and a message
    // selector, nothing about delivery or dispatch.
    let reserve_inventory = model
        .operations
        .get(&Id("operation.reserve_inventory".into()))
        .expect("reserve_inventory should exist");

    let input = reserve_inventory
        .inputs
        .get(&Id("input.reserve_inventory.created".into()))
        .expect("reserve_inventory subscription should exist");

    let Input::Subscription(subscription) = input else {
        panic!("reserve_inventory input should be a subscription");
    };

    assert_eq!(subscription.topic, Id("topic.order_events".into()));

    // Delivery and dispatch parse into the runtime model, addressed by
    // the (operation, input) pair the L0 boundary already establishes.
    let runtime = model
        .subscription_runtime(
            &Id("operation.reserve_inventory".into()),
            &Id("input.reserve_inventory.created".into()),
        )
        .expect("reserve_inventory should declare a subscription runtime");

    // The topic declares transport semantics, so the subscription
    // declares none of its own: the two scopes are exclusive.
    assert!(runtime.grouping.is_none());
    assert!(runtime.ordering.is_none());

    assert_eq!(runtime.delivery, DeliverySemantics::AtLeastOnce);
    assert_eq!(runtime.dispatch.pool, Id("pool.order_workers".into()));

    let routing = runtime
        .dispatch
        .routing
        .as_ref()
        .expect("the dispatch should declare routing");

    assert_eq!(routing.key, SubscriptionRoutingKey::GroupingKey);
    assert_eq!(routing.member_assignment, MemberAssignment::ConsistentHash);

    let pool = model
        .execution_pool(&Id("pool.order_workers".into()))
        .expect("the target pool should be declared");

    let MemberConcurrency::Bounded(concurrency) = pool.member_concurrency else {
        panic!("pool member concurrency should be bounded");
    };

    assert_eq!(concurrency.get(), 1);

    // Inline effect + the external boundary's three declarations.
    let Effect::External(card) = effect(
        &model,
        "operation.charge_payment",
        "effect.charge_payment.card",
    ) else {
        panic!("card charge should be an external effect");
    };

    assert_eq!(card.identity, ExternalIdentity::Unspecified);
    assert_eq!(card.idempotency, ExternalIdempotency::Distinguishable);
    assert_eq!(card.result_replay, ExternalResultReplay::Unspecified);

    // TransactionStep + SelectorPredicate + SelectorValue +
    // FieldSelection + LockOrder, inside an inline transaction.
    let transfer = transaction(&model, "operation.transfer_stock", "tx.transfer_stock");

    assert_eq!(transfer.id, Id("tx.transfer_stock".into()));

    assert_eq!(transfer.steps.len(), 5);

    assert!(matches!(&transfer.steps[0], TransactionStep::Lock(_)));

    assert!(matches!(&transfer.steps[1], TransactionStep::Lock(_)));

    assert!(matches!(&transfer.steps[2], TransactionStep::Read(_)));

    assert!(matches!(&transfer.steps[3], TransactionStep::Write(_)));

    assert!(matches!(&transfer.steps[4], TransactionStep::Write(_)));
}

#[test]
fn flash_checkout_round_trips_through_yaml() {
    let source = read_fixture("flash_checkout.yaml");

    let original = yaml::parse(&source).expect("flash checkout fixture should parse");

    let serialized = yaml::serialize(&original).expect("flash checkout model should serialize");

    let reparsed = yaml::parse(&serialized).expect("serialized flash checkout model should parse");

    assert_eq!(original, reparsed);
}

#[test]
fn flash_checkout_serialization_is_canonical() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let first = yaml::serialize(&model).expect("flash checkout model should serialize");

    let reparsed = yaml::parse(&first).expect("canonical YAML should parse");

    let second = yaml::serialize(&reparsed).expect("reparsed model should serialize");

    assert_eq!(first, second);
}

#[test]
fn flash_checkout_parses_keyed_transaction_idempotency() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let created = transaction(&model, "operation.create_order", "tx.create_order.new");

    let IdempotencyGuarantee::DeduplicatedBy { key } = &created.idempotency else {
        panic!("create_order transaction should declare keyed commit deduplication");
    };

    assert_eq!(key.components.len(), 1);

    assert_eq!(
        key.components[0].source,
        ValueSource::Input(Id("input.create_order.request".into()))
    );

    assert_eq!(
        key.components[0].path.0,
        vec!["idempotency_key".to_string()]
    );

    // The output binder declares the artifact's binding, schema, and
    // derivation at its production site; durable identity comes from
    // the committing transaction.
    let TransactionStep::EstablishTransactionOutput(establish) = &created.steps[2] else {
        panic!("third step should establish the output");
    };

    assert_eq!(establish.bind, Id("output.create_order".into()));
    assert_eq!(establish.schema, Id("schema.CreateOrderResponse".into()));
}

#[test]
fn flash_checkout_parses_transaction_read_provenance() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let reserve = transaction(
        &model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    );

    let TransactionStep::Read(read) = &reserve.steps[0] else {
        panic!("first step should be a read");
    };

    assert_eq!(read.bind, Id("read.reserve_inventory.stock".into()));

    let TransactionStep::Write(write) = &reserve.steps[1] else {
        panic!("second step should be a write");
    };

    let Derivation::Deterministic { from } = &write.values else {
        panic!("write should declare deterministic value provenance");
    };

    assert_eq!(
        from[0].source,
        ValueSource::TransactionRead(Id("read.reserve_inventory.stock".into()))
    );

    // Provenance is declared even where V1 will not use it to prove
    // natural replayability.
    assert_eq!(from[0].path.0, vec!["reserved".to_string()]);
}

#[test]
fn flash_checkout_parses_transition_side_effect_intent() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let transition = model
        .state_machines
        .get(&Id("machine.order_lifecycle".into()))
        .expect("order lifecycle should exist")
        .transitions
        .get(&Id("transition.order.mark_paid".into()))
        .expect("mark_paid transition should exist");

    let effect = transition
        .side_effects
        .get(&Id("effect.order.paid".into()))
        .expect("mark_paid should declare a side effect");

    assert!(matches!(effect, TransitionSideEffect::Publication(_)));

    // The application site binds the implicitly established intent so
    // a program step can execute it.
    let apply = transaction(&model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::Transition(applied) = &apply.steps[1] else {
        panic!("second step should be the mark_paid transition");
    };

    let intent = applied
        .effect_intents
        .get(&Id("effect.order.paid".into()))
        .expect("the transition application should bind the side effect's intent");

    assert_eq!(intent.bind, Id("intent.apply_payment.order_paid".into()));

    // The transaction establishes no intent explicitly.
    assert!(
        apply
            .steps
            .iter()
            .all(|step| !matches!(step, TransactionStep::EstablishEffectIntent(_)))
    );
}

#[test]
fn flash_checkout_parses_unspecified_derivation() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let transfer = transaction(&model, "operation.transfer_stock", "tx.transfer_stock");

    assert_eq!(transfer.idempotency, IdempotencyGuarantee::Unspecified);

    let TransactionStep::Write(write) = &transfer.steps[4] else {
        panic!("fifth step should be the destination write");
    };

    assert_eq!(write.values, Derivation::Unspecified);
}

#[test]
fn flash_checkout_parses_recoverability_requirements() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    // Request-driven: no retry driver is modeled, so only resumability
    // is required.
    let create_order = model
        .operations
        .get(&Id("operation.create_order".into()))
        .expect("create_order should exist");

    let requirement = &create_order.requirements.recoverability[0];

    assert_eq!(requirement.completion, CompletionRequirement::Resumable);

    assert_eq!(
        requirement.key.components[0].source,
        ValueSource::Input(Id("input.create_order.request".into()))
    );

    // Subscription-driven with at-least-once delivery: completion is
    // required outright.
    let apply_payment = model
        .operations
        .get(&Id("operation.apply_payment".into()))
        .expect("apply_payment should exist");

    let requirement = &apply_payment.requirements.recoverability[0];

    assert_eq!(requirement.completion, CompletionRequirement::Guaranteed);

    assert_eq!(
        requirement.key.components[0].path.0,
        vec!["event_id".to_string()]
    );

    // Recoverability is independent of idempotency: both are declared
    // here, keyed by the same logical invocation identity.
    assert_eq!(apply_payment.requirements.idempotency.len(), 1);

    assert_eq!(
        apply_payment.requirements.idempotency[0].key,
        requirement.key
    );

    // An operation may require neither.
    let transfer_stock = model
        .operations
        .get(&Id("operation.transfer_stock".into()))
        .expect("transfer_stock should exist");

    assert!(transfer_stock.requirements.recoverability.is_empty());
}

#[test]
fn flash_checkout_parses_execute_effect_values_and_result_bindings() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let program = &model
        .operations
        .get(&Id("operation.charge_payment".into()))
        .expect("charge_payment should exist")
        .program;

    // Unknown provenance is declared explicitly, never omitted; the
    // card charge declares its contract inline and binds the
    // provider's result.
    let OperationStep::ExecuteEffect(card) = &program.steps[0] else {
        panic!("first step should execute the card charge");
    };

    assert_eq!(card.effect_id, Id("effect.charge_payment.card".into()));
    assert!(matches!(card.effect, Effect::External(_)));
    assert_eq!(card.values, Derivation::Unspecified);
    assert_eq!(card.bind, Some(Id("result.charge_payment.card".into())));

    let OperationStep::MatchResult(matched) = &program.steps[1] else {
        panic!("second step should match the card result");
    };

    assert_eq!(matched.result, Id("result.charge_payment.card".into()));

    let OperationStep::ExecuteEffect(captured) = &matched.ok.steps[0] else {
        panic!("the ok arm should publish the capture");
    };

    assert_eq!(
        captured.effect_id,
        Id("effect.charge_payment.publish_captured".into())
    );

    assert!(matches!(captured.effect, Effect::Publication(_)));

    assert_eq!(captured.bind, None);

    let Derivation::Deterministic { from } = &captured.values else {
        panic!("publication values should declare deterministic provenance");
    };

    assert_eq!(from.len(), 3);

    assert_eq!(
        from[0].source,
        ValueSource::Input(Id("input.charge_payment.reserved".into()))
    );

    assert_eq!(from[0].path.0, vec!["event_id".to_string()]);

    // The err arm reads the provider's err payload.
    let OperationStep::ExecuteEffect(failed) = &matched.err.steps[0] else {
        panic!("the err arm should publish the failure");
    };

    let Derivation::Deterministic { from } = &failed.values else {
        panic!("failure values should declare deterministic provenance");
    };

    assert_eq!(
        from[2].source,
        ValueSource::EffectResultErr(Id("result.charge_payment.card".into()))
    );

    assert_eq!(from[2].path.0, vec!["reason".to_string()]);

    assert!(matches!(matched.ok.steps[1], OperationStep::Complete));
    assert!(matches!(matched.err.steps[1], OperationStep::Complete));
}

#[test]
fn flash_checkout_parses_request_results_and_return_terminals() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let create_order = model
        .operations
        .get(&Id("operation.create_order".into()))
        .expect("create_order should exist");

    let Some(Input::Request(request)) = create_order
        .inputs
        .get(&Id("input.create_order.request".into()))
    else {
        panic!("create_order input should be a request");
    };

    assert_eq!(request.result.ok, Id("schema.CreateOrderResponse".into()));
    assert_eq!(
        request.result.err.schema,
        Id("schema.RequestRejected".into())
    );

    // The bare-schema shorthand declares nothing about disposition.
    assert_eq!(
        request.result.err.disposition,
        ErrorDisposition::Unspecified
    );
    assert_eq!(
        request.result.schema(ResultVariant::Err),
        &Id("schema.RequestRejected".into())
    );

    let OperationStep::Return(returned) = &create_order.program.steps[2] else {
        panic!("the program should end by returning the request's result");
    };

    assert_eq!(returned.request, Id("input.create_order.request".into()));
    assert_eq!(returned.outcome.variant(), ResultVariant::Ok);

    let ResultOutcome::Ok { values } = &returned.outcome else {
        panic!("create_order returns ok");
    };

    let Derivation::Deterministic { from } = values else {
        panic!("the returned payload declares provenance");
    };

    assert_eq!(
        from[0].source,
        ValueSource::TransactionOutput(Id("output.create_order".into()))
    );

    // The output is established by the transaction; the return only
    // reads it.
    let created = transaction(&model, "operation.create_order", "tx.create_order.new");

    assert!(matches!(
        &created.steps[2],
        TransactionStep::EstablishTransactionOutput(establish)
            if establish.bind == Id("output.create_order".into())
    ));
}

#[test]
fn external_effects_declare_their_result_contract() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let Effect::External(card) = effect(
        &model,
        "operation.charge_payment",
        "effect.charge_payment.card",
    ) else {
        panic!("card charge should be an external effect");
    };

    let result = card.result.as_ref().expect("the provider returns a result");

    assert_eq!(result.ok, Id("schema.ChargeAccepted".into()));
    assert_eq!(result.err.schema, Id("schema.ChargeDeclined".into()));
    assert_eq!(result.err.disposition, ErrorDisposition::Unspecified);

    // A boundary modeling no synchronous result says so.
    let source = read_fixture("video_streaming.yaml");

    let model = yaml::parse(&source).expect("video streaming fixture should parse");

    let Effect::External(push) = effect(
        &model,
        "operation.notify_published",
        "effect.notify_published.push",
    ) else {
        panic!("push should be an external effect");
    };

    assert_eq!(push.result, None);

    // A declared disposition parses as part of the contract.
    let Effect::External(engine) = effect(
        &model,
        "operation.transcode_video",
        "effect.transcode_video.engine",
    ) else {
        panic!("the engine should be an external effect");
    };

    let result = engine.result.as_ref().expect("the engine returns a result");

    assert_eq!(result.err.schema, Id("schema.RenderFailed".into()));
    assert_eq!(result.err.disposition, ErrorDisposition::Terminal);
}

fn parse_error_contract(declaration: &str) -> ErrorResultType {
    serde_yaml::from_str(declaration)
        .unwrap_or_else(|error| panic!("`{declaration}` should parse: {error}"))
}

fn error_contract_error(declaration: &str) -> String {
    serde_yaml::from_str::<ErrorResultType>(declaration)
        .expect_err(&format!("`{declaration}` should not parse"))
        .to_string()
}

#[test]
fn an_error_contract_declares_schema_and_disposition() {
    // Every disposition parses in the canonical map form.
    for (text, disposition) in [
        ("unspecified", ErrorDisposition::Unspecified),
        ("terminal", ErrorDisposition::Terminal),
        ("retryable", ErrorDisposition::Retryable),
    ] {
        let contract = parse_error_contract(&format!(
            "schema: schema.ProviderError\ndisposition: {text}"
        ));

        assert_eq!(contract.schema, Id("schema.ProviderError".into()));
        assert_eq!(contract.disposition, disposition);
    }

    // The bare-schema shorthand declares nothing: `unspecified` is
    // epistemic, and no shorthand may silently strengthen it.
    assert_eq!(
        parse_error_contract("schema.ProviderError"),
        ErrorResultType {
            schema: Id("schema.ProviderError".into()),
            disposition: ErrorDisposition::Unspecified,
        }
    );

    // So does omitting the disposition in the map form.
    assert_eq!(
        parse_error_contract("schema: schema.ProviderError").disposition,
        ErrorDisposition::Unspecified
    );

    // A disposition outside the declared three is rejected.
    let message = error_contract_error(
        "schema: schema.ProviderError\ndisposition: definitely_not_a_disposition",
    );

    assert!(
        message.contains("definitely_not_a_disposition"),
        "error should mention the invalid value, got: {message}"
    );

    // The map form requires the error schema.
    error_contract_error("disposition: terminal");
}

#[test]
fn error_dispositions_serialize_into_the_canonical_form() {
    let source = read_fixture("video_streaming.yaml");

    let model = yaml::parse(&source).expect("video streaming fixture should parse");

    let serialized = yaml::serialize(&model).expect("model should serialize");

    // Canonical serialization always emits the disposition — the
    // declared terminal one, and `unspecified` for every contract the
    // shorthand left undeclared.
    assert!(
        serialized.contains("disposition: terminal"),
        "serialized model should carry the engine's terminal disposition"
    );

    assert!(
        serialized.contains("disposition: unspecified"),
        "serialized model should make undeclared dispositions explicit"
    );

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(model, reparsed);
}

#[test]
fn a_result_binding_may_be_omitted() {
    let step: OperationStep = serde_yaml::from_str(
        "kind: execute_effect
effect_id: effect.x
effect:
  kind: publication
  topic: topic.x
  schema: schema.X
  idempotency_key_propagation: []
values:
  kind: unspecified",
    )
    .expect("a step without a binding should parse");

    let OperationStep::ExecuteEffect(step) = step else {
        panic!("expected an execute_effect step");
    };

    assert_eq!(step.bind, None);

    let step: OperationStep = serde_yaml::from_str(
        "kind: execute_effect_intent
intent: intent.x",
    )
    .expect("an intent execution without a binding should parse");

    assert!(matches!(
        step,
        OperationStep::ExecuteEffectIntent(step) if step.bind.is_none()
    ));
}

#[test]
fn an_inline_transaction_step_carries_its_whole_declaration() {
    let step: OperationStep = serde_yaml::from_str(
        "kind: transaction
id: tx.x
data_model: null
isolation: unspecified
idempotency:
  kind: unspecified
steps: []",
    )
    .expect("an inline transaction should parse");

    let OperationStep::Transaction(transaction) = step else {
        panic!("expected an inline transaction step");
    };

    assert_eq!(transaction.id, Id("tx.x".into()));
    assert_eq!(transaction.data_model, None);
    assert!(transaction.steps.is_empty());

    // The old reference form is gone: a step that names a transaction
    // without declaring it does not parse.
    serde_yaml::from_str::<OperationStep>(
        "kind: transaction
transaction: tx.x",
    )
    .expect_err("a transaction reference should be rejected");
}

/// A minimal operation body, with `extra` spliced in at operation
/// level.
fn operation_source(extra: &str) -> String {
    let mut source = String::from(
        "dsl: 1
revision: 1
services:
  service.a:
    kind: backend
schemas: {}
data_models: {}
topics: {}
state_machines: {}
operations:
  operation.a:
    service: service.a
    description: null
    inputs: {}
",
    );

    for line in extra.lines() {
        source.push_str("    ");
        source.push_str(line);
        source.push('\n');
    }

    source.push_str(
        "    program:
      steps:
      - kind: complete
    requirements:
      serialization: []
      ordering: []
      idempotency: []
      recoverability: []
",
    );

    source
}

#[test]
fn the_removed_operation_registries_are_rejected() {
    // An operation without any of the four registries parses.
    yaml::parse(&operation_source("")).expect("a registry-free operation should parse");

    // Each retired registry field is rejected rather than ignored.
    for retired in [
        "effects: {}",
        "effect_intents: {}",
        "transaction_outputs: {}",
        "transactions: {}",
    ] {
        let error = yaml::parse(&operation_source(retired))
            .expect_err(&format!("`{retired}` should be rejected"));

        let field = retired.split(':').next().unwrap();

        assert!(
            error.to_string().contains(field),
            "error should name the retired field `{field}`, got: {error}"
        );
    }
}

#[test]
fn a_branch_condition_accepts_the_selector_value_surface() {
    let condition: Condition = serde_yaml::from_str(
        "kind: eq
value:
  source: input:input.checkout
  path: region
equals: CA",
    )
    .expect("a literal comparison should parse");

    let Condition::Eq { value, equals } = &condition else {
        panic!("expected an equality");
    };

    assert_eq!(
        value.source,
        ValueSource::Input(Id("input.checkout".into()))
    );
    assert_eq!(
        equals,
        &SelectorValue::Literal(Literal::String("CA".into()))
    );
    assert!(condition.is_deterministic());
    assert_eq!(condition.roots().len(), 1);

    let condition: Condition = serde_yaml::from_str(
        "kind: not
condition:
  kind: and
  conditions:
    - kind: eq
      value:
        source: input:input.checkout
        path: region
      equals:
        source: transaction_output:output.routing
        path: region
    - kind: unspecified",
    )
    .expect("a nested condition should parse");

    // Both sides of a reference comparison are roots; `unspecified`
    // anywhere makes the whole decision non-deterministic.
    assert_eq!(condition.roots().len(), 2);
    assert!(!condition.is_deterministic());
}

#[test]
fn flash_checkout_parses_transition_effect_intents() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let apply = transaction(&model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::Transition(transition) = &apply.steps[1] else {
        panic!("second step should be the mark_paid transition");
    };

    assert_eq!(transition.effect_intents.len(), 1);

    let intent = transition
        .effect_intents
        .get(&Id("effect.order.paid".into()))
        .expect("the mark_paid side effect should have an intent binding");

    // Each side effect receives exactly one bind and one derivation.
    assert_eq!(intent.bind, Id("intent.apply_payment.order_paid".into()));

    let Derivation::Deterministic { from } = &intent.values else {
        panic!("transition intent values should declare deterministic provenance");
    };

    // The derivation is evaluated in the transaction context, so it may
    // reference the preceding read.
    assert_eq!(
        from[0].source,
        ValueSource::TransactionRead(Id("read.apply_payment.order".into()))
    );

    assert_eq!(from[0].path.0, vec!["order_id".to_string()]);

    // A transition without side effects declares an explicit empty map.
    let cancel = transaction(&model, "operation.cancel_order", "tx.cancel_order");

    let TransactionStep::Transition(transition) = &cancel.steps[0] else {
        panic!("first step should be the cancel transition");
    };

    assert!(transition.effect_intents.is_empty());
}

/// A one-schema model whose sole schema declares `fields`, so field
/// surface syntax can be exercised without a fixture.
fn field_source(fields: &str) -> String {
    let mut source = String::from(
        "dsl: 1
revision: 1
services: {}
schemas:
  Subject:
    kind: canonical
    completeness: complete
    fields:
",
    );

    for line in fields.lines() {
        source.push_str("      ");
        source.push_str(line);
        source.push('\n');
    }

    source.push_str(
        "data_models: {}
topics: {}
state_machines: {}
operations: {}
",
    );

    source
}

fn parse_field(declaration: &str) -> Field {
    let source = field_source(declaration);

    let model = yaml::parse(&source)
        .unwrap_or_else(|error| panic!("`{declaration}` should parse: {error}"));

    let Some(Schema::Canonical(subject)) = model.schemas.get(&Id("Subject".into())) else {
        panic!("Subject should be a canonical schema");
    };

    subject
        .fields
        .values()
        .next()
        .cloned()
        .expect("the schema should declare a field")
}

fn field_error(declaration: &str) -> String {
    let source = field_source(declaration);

    yaml::parse(&source)
        .expect_err(&format!("`{declaration}` should not parse"))
        .to_string()
}

#[test]
fn a_shorthand_field_means_what_the_canonical_form_means() {
    let shorthand = parse_field("order_id: uuid");

    let canonical = parse_field(
        "order_id:
  ty:
    kind: scalar
    value: uuid
  optional: false",
    );

    assert_eq!(shorthand, canonical);

    assert_eq!(shorthand.ty, TypeRef::Scalar(ScalarType::Uuid));
    assert!(!shorthand.optional);
}

#[test]
fn a_trailing_question_mark_marks_a_field_optional() {
    let field = parse_field("note: string?");

    assert_eq!(field.ty, TypeRef::Scalar(ScalarType::String));
    assert!(field.optional);
}

#[test]
fn a_shorthand_name_that_is_not_a_scalar_is_a_schema_reference() {
    let field = parse_field("customer: schema.Customer");

    assert_eq!(field.ty, TypeRef::Schema(Id("schema.Customer".into())));
    assert!(!field.optional);
}

#[test]
fn a_schema_named_for_a_scalar_stays_reachable_through_the_canonical_form() {
    // `uuid` reads as the scalar in shorthand, so the canonical form
    // is the escape hatch rather than a special case in the grammar.
    let field = parse_field(
        "subject:
  ty:
    kind: schema
    value: uuid
  optional: false",
    );

    assert_eq!(field.ty, TypeRef::Schema(Id("uuid".into())));
}

#[test]
fn a_one_element_sequence_is_a_list_type() {
    let field = parse_field("tags: [string]");

    assert_eq!(
        field.ty,
        TypeRef::List(Box::new(TypeRef::Scalar(ScalarType::String)))
    );

    assert!(!field.optional);

    // Nesting works because the element is itself a type.
    let field = parse_field("rows: [[string]]");

    assert_eq!(
        field.ty,
        TypeRef::List(Box::new(TypeRef::List(Box::new(TypeRef::Scalar(
            ScalarType::String
        )))))
    );

    // An optional list needs the whole declaration quoted, because the
    // marker belongs to the field rather than to the element type.
    let field = parse_field("tags: \"[string]?\"");

    assert_eq!(
        field.ty,
        TypeRef::List(Box::new(TypeRef::Scalar(ScalarType::String)))
    );

    assert!(field.optional);
}

#[test]
fn shorthand_types_are_accepted_inside_the_canonical_form() {
    let field = parse_field(
        "note:
  ty: string
  optional: true",
    );

    assert_eq!(field.ty, TypeRef::Scalar(ScalarType::String));
    assert!(field.optional);

    let field = parse_field(
        "tags:
  ty: [schema.Tag]
  optional: true",
    );

    assert_eq!(
        field.ty,
        TypeRef::List(Box::new(TypeRef::Schema(Id("schema.Tag".into()))))
    );

    assert!(field.optional);
}

#[test]
fn a_type_may_not_carry_the_optional_marker() {
    // Optionality is a claim about the field, not about the type, so
    // the marker is rejected wherever a type alone is expected.
    let message = field_error(
        "note:
  ty: string?
  optional: true",
    );

    assert!(
        message.contains("`?` marks a field optional"),
        "error should explain where the marker belongs, got: {message}"
    );

    let message = field_error("tags: [string?]");

    assert!(
        message.contains("`?` marks a field optional"),
        "error should reject an optional element type, got: {message}"
    );
}

#[test]
fn a_list_shorthand_holds_exactly_one_element_type() {
    let message = field_error("tags: []");

    assert!(
        message.contains("exactly one element type"),
        "error should reject an empty list shorthand, got: {message}"
    );

    let message = field_error("tags: [string, int]");

    assert!(
        message.contains("exactly one element type"),
        "error should reject a multi-element list shorthand, got: {message}"
    );
}

#[test]
fn an_unterminated_list_shorthand_is_rejected() {
    let message = field_error("tags: \"[string\"");

    assert!(
        message.contains("unterminated"),
        "error should name the unterminated bracket, got: {message}"
    );
}

#[test]
fn a_canonical_schema_may_omit_its_description() {
    let source = field_source("order_id: uuid");

    let model = yaml::parse(&source).expect("a schema without a description should parse");

    let Some(Schema::Canonical(subject)) = model.schemas.get(&Id("Subject".into())) else {
        panic!("Subject should be a canonical schema");
    };

    assert_eq!(subject.description, None);
}

#[test]
fn shorthand_fields_serialize_into_the_canonical_form() {
    let source = field_source("note: string?");

    let model = yaml::parse(&source).expect("shorthand model should parse");

    let serialized = yaml::serialize(&model).expect("model should serialize");

    // Serialization is the wire format tooling reads, so it stays
    // explicit even when the source was written in shorthand.
    assert!(
        serialized.contains("kind: scalar"),
        "serialized model should carry the tagged type, got:\n{serialized}"
    );

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(model, reparsed);
}

fn parse_path(declaration: &str) -> FieldPath {
    serde_yaml::from_str(declaration)
        .unwrap_or_else(|error| panic!("`{declaration}` should parse: {error}"))
}

fn path_error(declaration: &str) -> String {
    serde_yaml::from_str::<FieldPath>(declaration)
        .expect_err(&format!("`{declaration}` should not parse"))
        .to_string()
}

fn parse_source(declaration: &str) -> ValueSource {
    serde_yaml::from_str(declaration)
        .unwrap_or_else(|error| panic!("`{declaration}` should parse: {error}"))
}

fn source_error(declaration: &str) -> String {
    serde_yaml::from_str::<ValueSource>(declaration)
        .expect_err(&format!("`{declaration}` should not parse"))
        .to_string()
}

#[test]
fn a_dotted_path_means_what_the_component_sequence_means() {
    assert_eq!(parse_path("customer.id"), parse_path("[customer, id]"));

    assert_eq!(
        parse_path("customer.id").0,
        vec!["customer".to_string(), "id".to_string()]
    );

    assert_eq!(parse_path("order_id").0, vec!["order_id".to_string()]);
}

#[test]
fn a_dotted_path_has_no_empty_components() {
    for declaration in ["customer..id", ".id", "customer.", "\"\""] {
        let message = path_error(declaration);

        assert!(
            message.contains("field path"),
            "error should name the offending path, got: {message}"
        );
    }
}

#[test]
fn a_path_naming_nothing_still_reaches_validation() {
    // Whether a path resolves is validation's question, so an empty
    // sequence parses here and fails there, exactly as before.
    assert_eq!(parse_path("[]").0, Vec::<String>::new());
}

#[test]
fn a_value_source_shorthand_means_what_the_tagged_map_means() {
    let shorthand = parse_source("input:input.create_order.request");

    let canonical = parse_source(
        "kind: input
id: input.create_order.request",
    );

    assert_eq!(shorthand, canonical);

    assert_eq!(
        shorthand,
        ValueSource::Input(Id("input.create_order.request".into()))
    );
}

#[test]
fn every_value_source_kind_has_a_shorthand() {
    assert_eq!(parse_source("input:x"), ValueSource::Input(Id("x".into())));

    assert_eq!(
        parse_source("effect:x"),
        ValueSource::Effect(Id("x".into()))
    );

    assert_eq!(
        parse_source("transaction_output:x"),
        ValueSource::TransactionOutput(Id("x".into()))
    );

    assert_eq!(
        parse_source("state_machine_subject:x"),
        ValueSource::StateMachineSubject(Id("x".into()))
    );

    assert_eq!(
        parse_source("transaction_read:x"),
        ValueSource::TransactionRead(Id("x".into()))
    );

    assert_eq!(
        parse_source("effect_result_ok:x"),
        ValueSource::EffectResultOk(Id("x".into()))
    );

    assert_eq!(
        parse_source("effect_result_err:x"),
        ValueSource::EffectResultErr(Id("x".into()))
    );

    // The retired kind is refused rather than read as anything else.
    let message = source_error("invocation_result:x");

    assert!(
        message.contains("is not a value source kind"),
        "error should reject the retired kind, got: {message}"
    );
}

#[test]
fn a_value_source_always_names_its_kind() {
    // The kind is never inferred from the id: the seven variants index
    // six namespaces (the two result-payload kinds share the binding
    // namespace), and an id may be declared in more than one.
    let message = source_error("input.create_order.request");

    assert!(
        message.contains("names its kind"),
        "error should ask for the kind, got: {message}"
    );

    let message = source_error("topic:topic.order_events");

    assert!(
        message.contains("is not a value source kind"),
        "error should reject an unknown kind, got: {message}"
    );

    let message = source_error("\"input:\"");

    assert!(
        message.contains("expected an id"),
        "error should ask for the id, got: {message}"
    );
}

#[test]
fn shorthand_paths_and_sources_serialize_into_the_canonical_form() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let serialized = yaml::serialize(&model).expect("model should serialize");

    // Tooling reads the serialized model, so it keeps the component
    // sequence and the tagged source.
    assert!(
        serialized.contains("kind: input"),
        "serialized model should carry tagged value sources"
    );

    assert!(
        !serialized.contains("path: idempotency_key"),
        "serialized model should carry path components as a sequence, not a dotted name"
    );

    assert!(
        !serialized.contains("source: input:"),
        "serialized model should carry the tagged source, not the shorthand"
    );

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(model, reparsed);
}

fn parse_selector_value(declaration: &str) -> SelectorValue {
    serde_yaml::from_str(declaration)
        .unwrap_or_else(|error| panic!("`{declaration}` should parse: {error}"))
}

fn selector_value_error(declaration: &str) -> String {
    serde_yaml::from_str::<SelectorValue>(declaration)
        .expect_err(&format!("`{declaration}` should not parse"))
        .to_string()
}

#[test]
fn a_selector_value_map_is_a_value_reference() {
    let shorthand = parse_selector_value(
        "source: input:input.create_order.request
path: idempotency_key",
    );

    let canonical = parse_selector_value(
        "kind: value
value:
  source:
    kind: input
    id: input.create_order.request
  path:
    - idempotency_key",
    );

    assert_eq!(shorthand, canonical);

    let SelectorValue::Value(reference) = shorthand else {
        panic!("a map with `source` should be a value reference");
    };

    assert_eq!(
        reference.source,
        ValueSource::Input(Id("input.create_order.request".into()))
    );

    assert_eq!(reference.path.0, vec!["idempotency_key".to_string()]);
}

#[test]
fn a_selector_value_scalar_is_a_literal() {
    assert_eq!(
        parse_selector_value("pending"),
        SelectorValue::Literal(Literal::String("pending".into()))
    );

    assert_eq!(
        parse_selector_value("true"),
        SelectorValue::Literal(Literal::Bool(true))
    );

    assert_eq!(
        parse_selector_value("3"),
        SelectorValue::Literal(Literal::Int(3))
    );

    // A string that YAML would read as another type is quoted, as it
    // is anywhere else in the document.
    assert_eq!(
        parse_selector_value("\"true\""),
        SelectorValue::Literal(Literal::String("true".into()))
    );
}

#[test]
fn a_selector_value_naming_a_value_source_kind_is_rejected() {
    // A reference that lost its path would otherwise read as the
    // string it spells, turning a provenance-bearing comparison into a
    // comparison with a constant.
    let message = selector_value_error("input:input.create_order.request");

    assert!(
        message.contains("reads as a string literal"),
        "error should refuse the ambiguous literal, got: {message}"
    );

    // The canonical form still declares such a string deliberately.
    assert_eq!(
        parse_selector_value(
            "kind: literal
value:
  kind: string
  value: input:input.create_order.request"
        ),
        SelectorValue::Literal(Literal::String("input:input.create_order.request".into()))
    );
}

#[test]
fn selector_value_keys_may_come_in_either_order() {
    assert_eq!(
        parse_selector_value(
            "path: idempotency_key
source: input:input.create_order.request"
        ),
        parse_selector_value(
            "source: input:input.create_order.request
path: idempotency_key"
        )
    );

    assert_eq!(
        parse_selector_value(
            "value: pending
kind: literal"
        ),
        SelectorValue::Literal(Literal::String("pending".into()))
    );
}

#[test]
fn an_unknown_selector_value_key_is_rejected() {
    let message = selector_value_error("reference: input:input.create_order.request");

    assert!(
        message.contains("unknown field `reference`"),
        "error should name the unknown key, got: {message}"
    );
}

#[test]
fn a_literal_shorthand_works_inside_the_canonical_wrapper() {
    assert_eq!(
        parse_selector_value(
            "kind: literal
value: pending"
        ),
        SelectorValue::Literal(Literal::String("pending".into()))
    );
}

#[test]
fn shorthand_selector_values_serialize_into_the_canonical_form() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let serialized = yaml::serialize(&model).expect("model should serialize");

    assert!(
        serialized.contains("kind: value"),
        "serialized model should carry the selector value tag"
    );

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(model, reparsed);
}

// ---------------------------------------------------------------------------
// The two canonical surfaces of the hierarchical model
// ---------------------------------------------------------------------------

/// An L0-only model is a complete Conseqa model: no runtime topology is
/// required, and the collections a model does not use may be omitted.
#[test]
fn an_l0_only_model_parses_with_no_runtime_block() {
    let source = "
dsl: 1
revision: 1

topics:
  topic.order_events:
    messages:
      - schema.OrderCreated
    message_identity:
      kind: keyed
      mapping:
        schema.OrderCreated:
          - event_id
";

    let model = yaml::parse(source).expect("an L0-only model should parse");

    assert!(model.runtime.is_none());
    assert!(model.services.is_empty());
    assert!(model.operations.is_empty());

    // With no runtime there are no transport facts at all, and the
    // topic is in neither declaration scope.
    assert!(model.topic_runtime(&Id("topic.order_events".into())).is_none());
    assert!(!model.topic_scoped_transport(&Id("topic.order_events".into())));
}

/// The canonical runtime-enriched surface: every L1 primitive in one
/// block, in the exact serialized shape the semantics document
/// documents and external tools must be able to read.
#[test]
fn the_canonical_runtime_block_parses_and_round_trips() {
    let source = "
dsl: 1
revision: 1

runtime:
  topics:
    topic.order_events:
      grouping:
        schema.OrderCreated: [order_id]
      ordering: within_group

  execution_pools:
    pool.order_workers:
      member_concurrency:
        kind: bounded
        value: 1
    pool.message_reads:
      member_concurrency:
        kind: bounded
        value: 32

  subscriptions:
    op.process_order:
      input.events:
        delivery: at_least_once
        dispatch:
          pool: pool.order_workers
          routing:
            key: grouping_key
            member_assignment:
              kind: consistent_hash

  routers:
    router.get_messages:
      boundary:
        operation: op.get_messages
        input: input.request
      pool: pool.message_reads
      routing:
        key:
          - channel_id
        member_assignment:
          kind: consistent_hash

    router.health:
      boundary:
        operation: op.health
        input: input.request
      pool: pool.message_reads

  storage_layouts:
    layout.messages:
      object:
        data_model: data.chat
        object: object.message
      partition_key:
        - channel_id
        - bucket
";

    let model = yaml::parse(source).expect("the canonical runtime block should parse");

    let runtime = model.runtime.as_ref().expect("a runtime model");

    assert_eq!(runtime.execution_pools.len(), 2);
    assert_eq!(runtime.routers.len(), 2);
    assert_eq!(runtime.storage_layouts.len(), 1);

    // Routing is optional, and absence is the whole statement: the
    // health boundary names a pool and no member affinity.
    assert!(runtime.routers[&Id("router.health".into())].routing.is_none());

    let get_messages = runtime.routers[&Id("router.get_messages".into())]
        .routing
        .as_ref()
        .expect("a routing declaration");

    assert_eq!(get_messages.key, vec![FieldPath(vec!["channel_id".into()])]);
    assert_eq!(get_messages.member_assignment, MemberAssignment::ConsistentHash);

    // A routing key and a partition key may name the same field without
    // becoming the same concept.
    let layout = &runtime.storage_layouts[&Id("layout.messages".into())];

    assert_eq!(
        layout.partition_key,
        vec![
            FieldPath(vec!["channel_id".into()]),
            FieldPath(vec!["bucket".into()]),
        ]
    );

    // External tools read L1 from the serialized surface alone, so it
    // must round-trip.
    let round_tripped = yaml::parse(&yaml::serialize(&model).expect("serializes"))
        .expect("re-parses");

    assert_eq!(model, round_tripped);
}

/// The second declaration mode on the wire: the topic declares no
/// transport semantics and each subscription declares its own pair,
/// so two subscribers of one channel may group differently.
#[test]
fn subscription_scoped_transport_semantics_parse() {
    let source = "
dsl: 1
revision: 1

schemas:
  schema.Event:
    kind: canonical
    completeness: complete
    fields:
      account_id: uuid
      region_id: uuid

topics:
  topic.events:
    messages:
      - schema.Event
    message_identity:
      kind: unspecified

runtime:
  execution_pools:
    pool.accounts:
      member_concurrency: { kind: bounded, value: 1 }
    pool.regions:
      member_concurrency: { kind: bounded, value: 1 }

  subscriptions:
    op.process_accounts:
      input.events:
        delivery: at_least_once
        grouping:
          schema.Event: [account_id]
        ordering: within_group
        dispatch:
          pool: pool.accounts
          routing:
            key: grouping_key
            member_assignment: { kind: consistent_hash }

    op.process_regions:
      input.events:
        delivery: at_least_once
        grouping:
          schema.Event: [region_id]
        dispatch:
          pool: pool.regions
          routing:
            key: grouping_key
            member_assignment: { kind: consistent_hash }
";

    let model = yaml::parse(source).expect("subscription-scoped semantics should parse");

    // The topic declares nothing, so it is not in topic-scoped mode.
    assert!(!model.topic_scoped_transport(&Id("topic.events".into())));

    let accounts = model
        .subscription_runtime(
            &Id("op.process_accounts".into()),
            &Id("input.events".into()),
        )
        .expect("a subscription runtime");

    assert_eq!(accounts.ordering, Some(OrderingSemantics::WithinGroup));

    // Grouping and ordering are independent: one subscriber orders
    // within its groups, the other only groups.
    let regions = model
        .subscription_runtime(&Id("op.process_regions".into()), &Id("input.events".into()))
        .expect("a subscription runtime");

    assert_eq!(regions.ordering, None);

    assert!(
        regions.grouping.is_some(),
        "grouping without ordering is a complete declaration, not half a pair"
    );

    let round_tripped = yaml::parse(&yaml::serialize(&model).expect("serializes"))
        .expect("re-parses");

    assert_eq!(model, round_tripped);
}

/// Both member assignments are on the wire, and the enum stays tagged
/// so a third stays additive.
#[test]
fn member_assignments_round_trip() {
    for (spelling, expected) in [
        ("consistent_hash", MemberAssignment::ConsistentHash),
        ("round_robin", MemberAssignment::RoundRobin),
    ] {
        let source = format!(
            "
dsl: 1
revision: 1

runtime:
  execution_pools:
    pool.p:
      member_concurrency: {{ kind: bounded, value: 1 }}

  routers:
    router.r:
      boundary: {{ operation: op.x, input: input.request }}
      pool: pool.p
      routing:
        key: [order_id]
        member_assignment: {{ kind: {spelling} }}
"
        );

        let model = yaml::parse(&source).expect("parses");

        let router = &model.runtime.as_ref().expect("a runtime").routers[&Id("router.r".into())];

        assert_eq!(
            router.routing.as_ref().expect("routing").member_assignment,
            expected
        );

        let round_tripped =
            yaml::parse(&yaml::serialize(&model).expect("serializes")).expect("re-parses");

        assert_eq!(model, round_tripped);
    }
}

#[test]
fn parses_asynchronous_effect_steps() {
    let source = r#"
dsl: 1
revision: 1
services:
  service.read:
    kind: backend
schemas:
  schema.Query:
    kind: canonical
    description: A read request.
    completeness: complete
    fields:
      id: uuid
  schema.Row:
    kind: canonical
    description: A read result.
    completeness: complete
    fields:
      id: uuid
  schema.Miss:
    kind: canonical
    description: A read failure.
    completeness: complete
    fields:
      reason: string
data_models: {}
topics: {}
state_machines: {}
operations:
  operation.hedged_read:
    service: service.read
    description: Race a primary and a replica read, then await both.
    inputs:
      input.hedged_read.request:
        kind: request
        schema: schema.Query
        identity:
          kind: unspecified
        result:
          ok: schema.Row
          err: schema.Miss
    program:
      steps:
      - kind: execute_effect_async
        handle: async.primary
        effect_id: effect.hedged_read.primary
        effect:
          kind: external
          name: store-a
          identity:
            kind: unspecified
          idempotency: unspecified
          result_replay: unspecified
          result:
            ok: schema.Row
            err: schema.Miss
        values:
          kind: unspecified
      - kind: execute_effect_async
        handle: async.replica
        effect_id: effect.hedged_read.replica
        effect:
          kind: external
          name: store-b
          identity:
            kind: unspecified
          idempotency: unspecified
          result_replay: unspecified
          result:
            ok: schema.Row
            err: schema.Miss
        values:
          kind: unspecified
      - kind: race
        handles:
        - async.primary
        - async.replica
        bind: result.read
      - kind: join_all
        handles:
        - handle: async.primary
        - handle: async.replica
          bind: result.replica
      - kind: match_result
        result: result.read
        ok:
          steps:
          - kind: return
            request: input.hedged_read.request
            outcome:
              kind: ok
              values:
                kind: deterministic
                from:
                - source: effect_result_ok:result.read
                  path: id
        err:
          steps:
          - kind: return
            request: input.hedged_read.request
            outcome:
              kind: err
              values:
                kind: unspecified
    requirements:
      serialization: []
      ordering: []
      idempotency: []
      recoverability: []
"#;

    let model = yaml::parse(source).expect("the async program should parse");

    let program = &model
        .operations
        .get(&Id("operation.hedged_read".into()))
        .expect("the operation exists")
        .program;

    let OperationStep::ExecuteEffectAsync(primary) = &program.steps[0] else {
        panic!("expected an async launch, found {:?}", program.steps[0]);
    };

    assert_eq!(primary.handle, Id("async.primary".into()));
    assert_eq!(primary.effect_id, Id("effect.hedged_read.primary".into()));
    assert!(matches!(primary.values, Derivation::Unspecified));

    let OperationStep::Race(race) = &program.steps[2] else {
        panic!("expected a race, found {:?}", program.steps[2]);
    };

    assert_eq!(
        race.handles,
        vec![Id("async.primary".into()), Id("async.replica".into())]
    );
    assert_eq!(race.bind, Some(Id("result.read".into())));

    let OperationStep::JoinAll(join) = &program.steps[3] else {
        panic!("expected a join_all, found {:?}", program.steps[3]);
    };

    assert_eq!(join.handles.len(), 2);
    assert_eq!(join.handles[0].handle, Id("async.primary".into()));
    assert_eq!(join.handles[0].bind, None);
    assert_eq!(join.handles[1].bind, Some(Id("result.replica".into())));

    // The async fan-out declares both effect sites.
    assert_eq!(program.effect_declarations().len(), 2);

    // The model survives a serialize/parse round trip unchanged.
    let serialized = yaml::serialize(&model).expect("serializes");
    let reparsed = yaml::parse(&serialized).expect("round trip parses");

    assert_eq!(model, reparsed);
}

// ---------------------------------------------------------------------
// Transactional outboxes
// ---------------------------------------------------------------------

#[test]
fn parses_transactional_outbox_model() {
    let source = read_fixture("transactional_outbox.yaml");

    let model = yaml::parse(&source).expect("transactional outbox fixture should parse");

    // The outbox lives on its data model, with a keyed identity.
    let outbox = model
        .data_models
        .get(&Id("data.orders".into()))
        .unwrap()
        .outboxes
        .get(&Id("outbox.order_events".into()))
        .expect("data.orders should declare the outbox");

    assert!(outbox.messages.contains(&Id("schema.OrderCreated".into())));
    assert!(matches!(outbox.message_identity, MessageIdentity::Keyed(_)));

    // The producer stages the write as a transaction step with the
    // specific transactional contract, not the general effect enum.
    let create = transaction(&model, "operation.create_order", "tx.create_order");

    let write = create
        .steps
        .iter()
        .find_map(|step| match step {
            TransactionStep::WriteOutbox(write) => Some(write),
            _ => None,
        })
        .expect("the transaction should stage an outbox write");

    assert_eq!(write.effect_id, Id("effect.create_order.outbox_created".into()));
    assert_eq!(write.effect.outbox, Id("outbox.order_events".into()));
    assert_eq!(write.effect.schema, Id("schema.OrderCreated".into()));
    assert_eq!(write.effect.idempotency_key_propagation.len(), 1);

    // The relay consumes through an outbox input with an explicit
    // acknowledgement declaration.
    let relay = model
        .operations
        .get(&Id("operation.publish_order_event".into()))
        .unwrap();

    let Some(Input::Outbox(input)) = relay
        .inputs
        .get(&Id("input.publish_order_event.outbox".into()))
    else {
        panic!("the relay should declare an outbox input");
    };

    assert_eq!(input.outbox, Id("outbox.order_events".into()));
    assert!(input.acknowledge_on_success);

    // The subscriber declares the optional companion acknowledgement.
    let Some(Input::Subscription(subscription)) = model
        .operations
        .get(&Id("operation.project_order".into()))
        .unwrap()
        .inputs
        .get(&Id("input.project_order.created".into()))
    else {
        panic!("the projector should subscribe");
    };

    assert_eq!(subscription.acknowledge_on_success, Some(true));

    // The outbox runtime declares all four facts, with an
    // order-preserving batching stage.
    let runtime = model
        .outbox_runtime(
            &Id("operation.publish_order_event".into()),
            &Id("input.publish_order_event.outbox".into()),
        )
        .expect("the relay's outbox runtime should be declared");

    assert_eq!(runtime.delivery, DeliverySemantics::AtLeastOnce);
    assert!(matches!(
        runtime.partitioning,
        conseqa::spec::OutboxPartitioning::Keyed(_)
    ));
    assert_eq!(runtime.ordering, conseqa::spec::OutboxOrdering::Partition);
    assert_eq!(runtime.dispatch.member_assignment, MemberAssignment::ConsistentHash);
    assert_eq!(
        runtime.dispatch.batching.as_ref().map(|batching| batching.ordering),
        Some(conseqa::spec::BatchOrderingPreservation::Preserved)
    );
}

#[test]
fn serializes_and_reparses_transactional_outbox_model() {
    let source = read_fixture("transactional_outbox.yaml");

    let original = yaml::parse(&source).expect("transactional outbox fixture should parse");

    let serialized = yaml::serialize(&original).expect("model should serialize");

    let reparsed = yaml::parse(&serialized).expect("serialized model should parse");

    assert_eq!(original, reparsed);
}

/// A subscription that declares no acknowledgement fact parses with
/// `None` — absence, not a silent negative — and a model without
/// outboxes round-trips without an `outboxes` key appearing.
#[test]
fn absent_acknowledgement_and_outboxes_stay_absent() {
    let source = read_fixture("flash_checkout.yaml");

    let model = yaml::parse(&source).expect("flash checkout fixture should parse");

    let Some(Input::Subscription(subscription)) = model
        .operations
        .get(&Id("operation.reserve_inventory".into()))
        .unwrap()
        .inputs
        .get(&Id("input.reserve_inventory.created".into()))
    else {
        panic!("reserve_inventory should subscribe");
    };

    assert_eq!(subscription.acknowledge_on_success, None);

    let serialized = yaml::serialize(&model).expect("model should serialize");

    assert!(!serialized.contains("acknowledge_on_success"));
    assert!(!serialized.contains("outboxes"));
}

#[test]
fn a_declared_dsl_version_mismatch_is_refused_by_name() {
    let error = yaml::parse("dsl: 2\nrevision: 1\n")
        .expect_err("a future contract version should be refused");

    assert!(
        matches!(
            &error,
            yaml::ParseError::DslVersionMismatch { found } if found.0 == 2
        ),
        "{error:?}"
    );

    let message = error.to_string();

    assert!(message.contains("declares dsl 2"), "{message}");
    assert!(message.contains("this build reads dsl 1"), "{message}");
}

#[test]
fn a_missing_dsl_version_is_refused_as_predating_versioning() {
    let error = yaml::parse("revision: 1\n")
        .expect_err("an unversioned specification should be refused");

    assert!(
        matches!(&error, yaml::ParseError::DslVersionMissing),
        "{error:?}"
    );

    assert!(
        error.to_string().contains("predates versioning"),
        "{error}"
    );
}

#[test]
fn the_superseded_external_surface_fails_schema_validation() {
    // The clean break: the retired mechanism vocabulary is not
    // detected, canonicalized, or aliased — it fails ordinary shape
    // validation like any other unknown form.
    let source = "dsl: 1
revision: 1
operations:
  operation.x:
    service: service.x
    inputs: {}
    program:
      steps:
      - kind: execute_effect
        effect_id: effect.x
        effect:
          kind: external
          name: provider
          idempotency:
            kind: deduplicated_by
            key:
              components: []
          result: null
        values:
          kind: unspecified
      - kind: complete
    requirements:
      serialization: []
      ordering: []
      idempotency: []
      recoverability: []
";

    let error = yaml::parse(source).expect_err("the legacy surface should not parse");

    assert!(matches!(&error, yaml::ParseError::Yaml(_)), "{error:?}");
}

#[test]
fn a_present_condition_parses_and_round_trips() {
    let source = "dsl: 1
revision: 1
schemas:
  schema.Event:
    kind: canonical
    completeness: complete
    fields:
      id: uuid
      note: string?
topics:
  topic.events:
    messages:
    - schema.Event
    message_identity:
      kind: keyed
      mapping:
        schema.Event:
        - - id
operations:
  operation.observe:
    service: service.x
    inputs:
      input.observe.events:
        kind: subscription
        topic: topic.events
        messages:
          kind: all
    program:
      steps:
      - kind: branch
        condition:
          kind: not
          condition:
            kind: present
            value:
              source: input:input.observe.events
              path: note
        then:
          steps:
          - kind: complete
      - kind: complete
    requirements:
      serialization: []
      ordering: []
      idempotency: []
      recoverability: []
services:
  service.x:
    kind: backend
";

    let model = yaml::parse(source).expect("the present condition should parse");

    let operation = model
        .operations
        .get(&Id("operation.observe".into()))
        .expect("operation exists");

    let OperationStep::Branch(branch) = &operation.program.steps[0] else {
        panic!("expected the branch");
    };

    let Condition::Not { condition } = &branch.condition else {
        panic!("expected the negation");
    };

    assert!(matches!(&**condition, Condition::Present { value }
        if value.path.0 == vec!["note".to_string()]));

    let serialized = yaml::serialize(&model).expect("model serializes");
    let reparsed = yaml::parse(&serialized).expect("serialized model parses");

    assert_eq!(model, reparsed);
}
