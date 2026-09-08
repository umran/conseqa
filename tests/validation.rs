use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use conseqa::{
    analyzer::validation::{self, ProgramUse, ReferenceKind, ValidationError},
    parser::yaml,
    spec::{
        Arm, Branch, Condition, Derivation, Effect, EstablishTransactionOutput, ExecuteEffect,
        FieldPath, Id, IdempotencyGuarantee, Input, Literal, MessageIdentity, MessageSelector,
        Model, OperationBlock, OperationStep, RequestEffect, RequestIdentity, RequestTarget,
        DataObjectRef, ExecutionPool, MemberAssignment, MemberConcurrency,
        OperationInputRef,
        RequestRouting, ResultOutcome, ResultVariant, RetrySemantics, Return, Router, RuntimeModel,
        Schema, SchemaFragment, SelectorValue, StateTransition, StepHop, StepLocation,
        StorageLayout, SubscriptionRoutingKey, OrderingSemantics, Transaction, TransactionIsolation,
        TransactionStep, TransitionEffectIntent, ValueRef, ValueSource,
    },
};

fn id(value: &str) -> Id {
    Id(value.to_owned())
}

fn path(components: &[&str]) -> FieldPath {
    FieldPath(components.iter().map(|part| (*part).to_owned()).collect())
}

fn input_ref(input: &str, components: &[&str]) -> ValueRef {
    ValueRef {
        source: ValueSource::Input(id(input)),
        path: path(components),
    }
}

/// A step location from `(index, arm entered beneath it)` hops.
fn at(hops: &[(usize, Option<Arm>)]) -> StepLocation {
    StepLocation(
        hops.iter()
            .map(|(step, arm)| StepHop {
                step: *step,
                arm: *arm,
            })
            .collect(),
    )
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

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// The order-events topic's runtime, for tests that perturb the
/// transport grouping domain.
fn topic_runtime(model: &mut Model) -> &mut conseqa::spec::TopicRuntime {
    model
        .runtime
        .as_mut()
        .expect("the fixture declares a runtime model")
        .topics
        .get_mut(&id("topic.order_events"))
        .expect("the order-events topic declares a runtime")
}

fn load_flash_checkout() -> Model {
    let path = fixture_path("flash_checkout.yaml");

    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture `{}`: {error}", path.display()));

    yaml::parse(&source).expect("flash checkout fixture should parse")
}

#[test]
fn flash_checkout_is_valid() {
    let model = load_flash_checkout();

    let errors = validation::validate(&model);

    assert!(
        errors.is_empty(),
        "flash checkout should be valid:\n{errors:#?}"
    );
}

#[test]
fn rejects_duplicate_global_id() {
    let mut model = load_flash_checkout();

    let schema = model
        .schemas
        .get(&id("schema.CreateOrderRequest"))
        .unwrap()
        .clone();

    // Collides with an existing service ID.
    model.schemas.insert(id("service.checkout"), schema);

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("service.checkout")
    ));
}

#[test]
fn rejects_unknown_service_reference() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .service = id("service.missing");

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnknownReference {
            subject: id("operation.create_order"),
            reference: id("service.missing"),
            expected: ReferenceKind::Service,
        }]
    );
}

#[test]
fn rejects_reference_with_wrong_kind() {
    let mut model = load_flash_checkout();

    model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .service = id("schema.CreateOrderRequest");

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidReferenceKind {
            subject: id("operation.create_order"),
            reference: id("schema.CreateOrderRequest"),
            expected: ReferenceKind::Service,
            actual: ReferenceKind::Schema,
        }]
    );
}

#[test]
fn rejects_duplicate_inline_transaction_ids() {
    let mut model = load_flash_checkout();

    // A second inline declaration under an existing transaction ID:
    // one inline transaction declaration is one occurrence, so two
    // sites need two IDs.
    program_mut(&mut model, "operation.transfer_stock")
        .steps
        .insert(
            1,
            OperationStep::Transaction(Transaction {
                id: id("tx.transfer_stock"),
                data_model: None,
                isolation: TransactionIsolation::Unspecified,
                idempotency: IdempotencyGuarantee::Unspecified,
                steps: Vec::new(),
            }),
        );

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("tx.transfer_stock")
    ));
}

#[test]
fn rejects_duplicate_inline_effect_ids() {
    let mut model = load_flash_checkout();

    // Two execution sites declaring one effect_id: distinct sites are
    // distinct effect occurrences and need distinct IDs.
    let program = program_mut(&mut model, "operation.charge_payment");

    let first = program.steps[0].clone();

    let OperationStep::ExecuteEffect(mut step) = first else {
        panic!("expected the card charge");
    };

    step.bind = None;

    program.steps.insert(0, OperationStep::ExecuteEffect(step));

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("effect.charge_payment.card")
    ));
}

#[test]
fn rejects_duplicate_binding_ids() {
    let mut model = load_flash_checkout();

    // Two producers of one binding: a binding is single-producer, so a
    // second output binder under the same name collides.
    let transaction = transaction_mut(&mut model, "operation.create_order", "tx.create_order.new");

    let TransactionStep::EstablishTransactionOutput(establish) = transaction.steps[2].clone()
    else {
        panic!("expected the output binder");
    };

    transaction
        .steps
        .push(TransactionStep::EstablishTransactionOutput(establish));

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("output.create_order")
    ));
}

#[test]
fn rejects_schema_fragment_cycle() {
    let mut model = load_flash_checkout();

    model.schemas.insert(
        id("schema.FragmentA"),
        Schema::Fragment(SchemaFragment {
            source: id("schema.FragmentB"),
            mapping: BTreeMap::new(),
        }),
    );

    model.schemas.insert(
        id("schema.FragmentB"),
        Schema::Fragment(SchemaFragment {
            source: id("schema.FragmentA"),
            mapping: BTreeMap::new(),
        }),
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::FragmentCycle {
            cycle: vec![
                id("schema.FragmentA"),
                id("schema.FragmentB"),
                id("schema.FragmentA"),
            ],
        }]
    );
}

#[test]
fn rejects_subscription_message_not_carried_by_topic() {
    let mut model = load_flash_checkout();

    // Clone OrderCreated so all of the operation's existing field-path
    // requirements still resolve. The only defect is topic membership.
    let shadow_id = id("schema.OrderCreatedShadow");

    let shadow = model
        .schemas
        .get(&id("schema.OrderCreated"))
        .unwrap()
        .clone();

    model.schemas.insert(shadow_id.clone(), shadow);

    let operation = model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap();

    let input = operation
        .inputs
        .get_mut(&id("input.reserve_inventory.created"))
        .unwrap();

    let Input::Subscription(subscription) = input else {
        panic!("expected subscription input");
    };

    let MessageSelector::Only(schemas) = &mut subscription.messages else {
        panic!("expected selective message subscription");
    };

    schemas.insert(shadow_id.clone());

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::SubscriptionMessageNotOnTopic {
            input: id("input.reserve_inventory.created"),
            topic: id("topic.order_events"),
            schema: shadow_id,
        }]
    );
}

#[test]
fn rejects_publication_schema_not_carried_by_topic() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order");

    let TransactionStep::EstablishEffectIntent(establish) = &mut transaction.steps[1] else {
        panic!("expected the intent establishment");
    };

    let conseqa::spec::Effect::Publication(publication) = &mut establish.effect else {
        panic!("expected publication effect");
    };

    publication.schema = id("schema.CancelOrderRequest");

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::PublicationEffectMessageNotOnTopic {
            effect: id("effect.cancel_order.publish_cancelled"),
            topic: id("topic.order_events"),
            schema: id("schema.CancelOrderRequest"),
        }]
    );
}

#[test]
fn rejects_grouping_key_missing_schema_mapping() {
    let mut model = load_flash_checkout();

    let Some(key) = &mut topic_runtime(&mut model).grouping else {
        panic!("expected a keyed grouping");
    };

    key.mapping.remove(&id("schema.PaymentCaptured"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::GroupingKeyMissingSchema {
            subject: id("topic.order_events"),
            topic: id("topic.order_events"),
            schema: id("schema.PaymentCaptured"),
        }]
    );
}

#[test]
fn rejects_grouping_key_for_schema_not_on_topic() {
    let mut model = load_flash_checkout();

    let Some(key) = &mut topic_runtime(&mut model).grouping else {
        panic!("expected a keyed grouping");
    };

    key.mapping.insert(
        id("schema.CancelOrderRequest"),
        vec![FieldPath(vec!["order_id".to_owned()])],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::GroupingKeySchemaNotOnTopic {
            subject: id("topic.order_events"),
            topic: id("topic.order_events"),
            schema: id("schema.CancelOrderRequest"),
        }]
    );
}

#[test]
fn rejects_transaction_access_without_data_model() {
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order").data_model = None;

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionMissingDataModel {
            transaction: id("tx.cancel_order"),
            object: id("object.order"),
        }]
    );
}

#[test]
fn rejects_transaction_access_outside_declared_data_model() {
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order").data_model =
        Some(id("data.inventory"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionObjectOutsideDataModel {
            transaction: id("tx.cancel_order"),
            data_model: id("data.inventory"),
            object: id("object.order"),
        }]
    );
}

#[test]
fn rejects_state_transition_with_wrong_subject_object() {
    let mut model = load_flash_checkout();

    // Create another valid object in the SAME data model with the
    // SAME schema so that ownership and field-path validation still
    // succeed. The only defect is state-machine subject identity.
    let shadow_id = id("object.order_shadow");

    let shadow = model
        .data_models
        .get(&id("data.checkout"))
        .unwrap()
        .objects
        .get(&id("object.order"))
        .unwrap()
        .clone();

    model
        .data_models
        .get_mut(&id("data.checkout"))
        .unwrap()
        .objects
        .insert(shadow_id.clone(), shadow);

    let transaction = transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::Transition(transition) = &mut transaction.steps[1] else {
        panic!("expected transition step");
    };

    transition.subject.object = shadow_id.clone();

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::StateTransitionSubjectMismatch {
            transaction: id("tx.apply_payment"),
            machine: id("machine.order_lifecycle"),
            expected_object: id("object.order"),
            actual_object: shadow_id,
        }]
    );
}

#[test]
fn rejects_invalid_value_ref_field_path() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap();

    operation.requirements.ordering[0].key.path = FieldPath(vec!["does_not_exist".to_owned()]);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("operation.apply_payment"),
            schema: id("schema.PaymentCaptured"),
            path: FieldPath(vec!["does_not_exist".to_owned()]),
        }]
    );
}

#[test]
fn rejects_field_reference_into_untyped_external_effect() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.charge_payment"))
        .unwrap();

    operation.requirements.serialization[0].key.source =
        ValueSource::Effect(id("effect.charge_payment.card"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceHasNoSchema {
            subject: id("operation.charge_payment"),
            source: id("effect.charge_payment.card"),
        }]
    );
}

#[test]
fn transition_transactions_accept_any_idempotency_guarantee() {
    // A transition blocks the natural-replay proof route,
    // but the missing proof fact is the solver's concern, never a
    // structural error. The keyed form is the fixture as declared.
    for idempotency in [
        IdempotencyGuarantee::Unspecified,
        IdempotencyGuarantee::NotDeduplicated,
    ] {
        let mut model = load_flash_checkout();

        transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment").idempotency =
            idempotency;

        let errors = validation::validate(&model);

        assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
    }
}

#[test]
fn transition_effect_intents_coverage_is_independent_of_the_guarantee() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    transaction.idempotency = IdempotencyGuarantee::Unspecified;

    let TransactionStep::Transition(transition) = &mut transaction.steps[1] else {
        panic!("expected the mark_paid transition step");
    };

    transition.effect_intents.clear();

    // Clearing the map removes the binding's one producer, so the
    // intent execution must go with it.
    program_mut(&mut model, "operation.apply_payment")
        .steps
        .remove(1);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransitionEffectIntentsMismatch {
            transaction: id("tx.apply_payment"),
            transition: id("transition.order.mark_paid"),
            missing: vec![id("effect.order.paid")],
            unexpected: vec![],
        }]
    );
}

#[test]
fn rejects_an_inline_effect_shadowing_a_transition_side_effect() {
    let mut model = load_flash_checkout();

    // An explicit establishment whose effect_id collides with the
    // transition-owned declaration: the ID belongs to the state
    // machine's namespace, so the inline site is a duplicate — the
    // application site is the only place a transition side effect's
    // intent is bound.
    let transaction = transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    transaction
        .steps
        .push(TransactionStep::EstablishEffectIntent(
            conseqa::spec::EstablishEffectIntent {
                bind: id("intent.apply_payment.order_paid.shadow"),
                effect_id: id("effect.order.paid"),
                effect: conseqa::spec::Effect::Publication(conseqa::spec::PublicationEffect {
                    topic: id("topic.order_events"),
                    schema: id("schema.OrderPaid"),
                    idempotency_key_propagation: vec![],
                }),
                values: Derivation::Unspecified,
            },
        ));

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("effect.order.paid")
    ));
}

#[test]
fn rejects_direct_execution_shadowing_a_transition_side_effect() {
    let mut model = load_flash_checkout();

    // A direct execution site declaring the transition side effect's
    // ID collides with the state-machine declaration: the effect stays
    // transition-owned, executed only through its bound intent.
    program_mut(&mut model, "operation.apply_payment").steps[1] =
        OperationStep::ExecuteEffect(conseqa::spec::ExecuteEffect {
            effect_id: id("effect.order.paid"),
            effect: conseqa::spec::Effect::Publication(conseqa::spec::PublicationEffect {
                topic: id("topic.order_events"),
                schema: id("schema.OrderPaid"),
                idempotency_key_propagation: vec![],
            }),
            values: Derivation::Unspecified,
            bind: None,
        });

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("effect.order.paid")
    ));
}

#[test]
fn rejects_transaction_read_used_outside_a_transaction() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap();

    operation.requirements.serialization[0].key.source =
        ValueSource::TransactionRead(id("read.reserve_inventory.stock"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadOutsideTransaction {
            subject: id("operation.reserve_inventory"),
            read: id("read.reserve_inventory.stock"),
        }]
    );
}

#[test]
fn rejects_transaction_read_from_another_transaction() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.transfer_stock", "tx.transfer_stock");

    let TransactionStep::Write(write) = &mut transaction.steps[3] else {
        panic!("expected the source-warehouse write");
    };

    let Derivation::Deterministic { from } = &mut write.values else {
        panic!("expected a deterministic derivation");
    };

    from[0].source = ValueSource::TransactionRead(id("read.reserve_inventory.stock"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidReferenceOwner {
            subject: id("tx.transfer_stock"),
            reference: id("read.reserve_inventory.stock"),
            expected_owner: id("tx.transfer_stock"),
            actual_owner: Some(id("tx.reserve_inventory")),
        }]
    );
}

#[test]
fn rejects_transaction_read_referenced_before_the_read() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    );

    // Move the mutation ahead of the read it derives its values from.
    transaction.steps.swap(0, 1);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadOutOfOrder {
            transaction: id("tx.reserve_inventory"),
            read: id("read.reserve_inventory.stock"),
        }]
    );
}

#[test]
fn rejects_reference_to_field_the_read_did_not_select() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    );

    let TransactionStep::Write(write) = &mut transaction.steps[1] else {
        panic!("expected the stock write");
    };

    let Derivation::Deterministic { from } = &mut write.values else {
        panic!("expected a deterministic derivation");
    };

    // `warehouse_id` resolves against the object schema, but the read
    // selects only `on_hand` and `reserved`.
    from[0].path = FieldPath(vec!["warehouse_id".to_owned()]);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadFieldNotSelected {
            transaction: id("tx.reserve_inventory"),
            read: id("read.reserve_inventory.stock"),
            path: FieldPath(vec!["warehouse_id".to_owned()]),
        }]
    );
}

#[test]
fn rejects_transaction_read_bind_colliding_with_another_id() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    );

    let TransactionStep::Read(read) = &mut transaction.steps[0] else {
        panic!("expected the stock read");
    };

    read.bind = id("object.stock");

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("object.stock")
    ));
}

#[test]
fn rejects_data_object_without_identity() {
    let mut model = load_flash_checkout();

    model
        .data_models
        .get_mut(&id("data.checkout"))
        .unwrap()
        .objects
        .get_mut(&id("object.order"))
        .unwrap()
        .identity
        .clear();

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EmptyObjectIdentity {
            object: id("object.order"),
        }]
    );
}

#[test]
fn rejects_a_program_that_falls_through_without_a_terminal() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.charge_payment")
        .steps
        .clear();

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ProgramNotTerminated {
            operation: id("operation.charge_payment"),
        }]
    );

    // A path that falls through one arm of a decision and then off the
    // end of the program is the same defect.
    let mut model = load_flash_checkout();

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    matched.err.steps.pop();

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ProgramNotTerminated {
            operation: id("operation.charge_payment"),
        }]
    );
}

#[test]
fn rejects_invalid_recoverability_key_field_path() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap();

    operation.requirements.recoverability[0].key.components[0].path =
        FieldPath(vec!["does_not_exist".to_owned()]);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("operation.apply_payment"),
            schema: id("schema.PaymentCaptured"),
            path: FieldPath(vec!["does_not_exist".to_owned()]),
        }]
    );
}

#[test]
fn rejects_unknown_reference_in_recoverability_key() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap();

    operation.requirements.recoverability[0].key.components[0].source =
        ValueSource::Input(id("input.missing"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnknownReference {
            subject: id("operation.apply_payment"),
            reference: id("input.missing"),
            expected: ReferenceKind::Input,
        }]
    );
}

#[test]
fn rejects_transaction_read_in_recoverability_key() {
    let mut model = load_flash_checkout();

    let operation = model
        .operations
        .get_mut(&id("operation.reserve_inventory"))
        .unwrap();

    operation.requirements.recoverability[0].key.components[0].source =
        ValueSource::TransactionRead(id("read.reserve_inventory.stock"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadOutsideTransaction {
            subject: id("operation.reserve_inventory"),
            read: id("read.reserve_inventory.stock"),
        }]
    );
}

/// Retarget the first component of `tx.reserve_inventory`'s write
/// derivation, which is the fixture's provenance site.
fn set_reserve_write_source(model: &mut Model, source: ValueSource, path: &[&str]) {
    let transaction = transaction_mut(model, "operation.reserve_inventory", "tx.reserve_inventory");

    let TransactionStep::Write(write) = &mut transaction.steps[1] else {
        panic!("expected the stock write");
    };

    let Derivation::Deterministic { from } = &mut write.values else {
        panic!("expected a deterministic derivation");
    };

    from[0] = ValueRef {
        source,
        path: FieldPath(path.iter().map(|part| (*part).to_owned()).collect()),
    };
}

#[test]
fn rejects_value_ref_to_another_operations_input() {
    let mut model = load_flash_checkout();

    set_reserve_write_source(
        &mut model,
        ValueSource::Input(id("input.create_order.request")),
        &["quantity"],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("tx.reserve_inventory"),
            source: id("input.create_order.request"),
            owner: id("operation.create_order"),
        }]
    );
}

#[test]
fn rejects_value_ref_to_another_operations_transaction_output() {
    let mut model = load_flash_checkout();

    set_reserve_write_source(
        &mut model,
        ValueSource::TransactionOutput(id("output.create_order")),
        &["order_id"],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("tx.reserve_inventory"),
            source: id("output.create_order"),
            owner: id("operation.create_order"),
        }]
    );
}

#[test]
fn rejects_value_ref_to_another_operations_effect_result() {
    let mut model = load_flash_checkout();

    set_reserve_write_source(
        &mut model,
        ValueSource::EffectResultOk(id("result.charge_payment.card")),
        &["authorization_id"],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("tx.reserve_inventory"),
            source: id("result.charge_payment.card"),
            owner: id("operation.charge_payment"),
        }]
    );
}

#[test]
fn rejects_value_ref_to_another_operations_effect() {
    let mut model = load_flash_checkout();

    set_reserve_write_source(
        &mut model,
        ValueSource::Effect(id("effect.create_order.publish_created")),
        &["order_id"],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("tx.reserve_inventory"),
            source: id("effect.create_order.publish_created"),
            owner: id("operation.create_order"),
        }]
    );
}

#[test]
fn rejects_value_ref_to_transition_effect_the_operation_does_not_apply() {
    let mut model = load_flash_checkout();

    // reserve_inventory applies no transition, so the mark_paid side
    // effect's payload is not observable by its invocations.
    set_reserve_write_source(
        &mut model,
        ValueSource::Effect(id("effect.order.paid")),
        &["order_id"],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("tx.reserve_inventory"),
            source: id("effect.order.paid"),
            owner: id("transition.order.mark_paid"),
        }]
    );
}

#[test]
fn accepts_value_ref_to_state_machine_subject_from_any_operation() {
    let mut model = load_flash_checkout();

    // State machines are global; reserve_inventory may address the
    // object one governs even though it applies no transition.
    set_reserve_write_source(
        &mut model,
        ValueSource::StateMachineSubject(id("machine.order_lifecycle")),
        &["status"],
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn rejects_foreign_input_in_a_transaction_commit_key() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.create_order", "tx.create_order.new");

    let IdempotencyGuarantee::DeduplicatedBy { key } = &mut transaction.idempotency else {
        panic!("expected a keyed transaction");
    };

    key.components[0] = ValueRef {
        source: ValueSource::Input(id("input.apply_payment.captured")),
        path: FieldPath(vec!["event_id".to_owned()]),
    };

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("tx.create_order.new"),
            source: id("input.apply_payment.captured"),
            owner: id("operation.apply_payment"),
        }]
    );
}

/// Retarget the card-charge program step's derivation, which is the
/// fixture's direct-execution provenance site.
fn set_charge_card_values(model: &mut Model, values: Derivation) {
    let OperationStep::ExecuteEffect(step) =
        &mut program_mut(model, "operation.charge_payment").steps[0]
    else {
        panic!("expected the card-charge execute_effect step");
    };

    step.values = values;
}

#[test]
fn accepts_operation_scoped_deterministic_execute_effect_values() {
    let mut model = load_flash_checkout();

    set_charge_card_values(
        &mut model,
        Derivation::Deterministic {
            from: vec![ValueRef {
                source: ValueSource::Input(id("input.charge_payment.reserved")),
                path: FieldPath(vec!["amount".to_owned()]),
            }],
        },
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn accepts_unspecified_execute_effect_values() {
    let mut model = load_flash_checkout();

    set_charge_card_values(&mut model, Derivation::Unspecified);

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn rejects_transaction_read_in_execute_effect_values() {
    let mut model = load_flash_checkout();

    // No transaction context exists at program level, so a transaction
    // read can never be a direct-execution provenance root.
    set_charge_card_values(
        &mut model,
        Derivation::Deterministic {
            from: vec![ValueRef {
                source: ValueSource::TransactionRead(id("read.reserve_inventory.stock")),
                path: FieldPath(vec!["on_hand".to_owned()]),
            }],
        },
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadOutsideTransaction {
            subject: id("operation.charge_payment"),
            read: id("read.reserve_inventory.stock"),
        }]
    );
}

#[test]
fn rejects_invalid_field_path_in_execute_effect_values() {
    let mut model = load_flash_checkout();

    set_charge_card_values(
        &mut model,
        Derivation::Deterministic {
            from: vec![ValueRef {
                source: ValueSource::Input(id("input.charge_payment.reserved")),
                path: FieldPath(vec!["does_not_exist".to_owned()]),
            }],
        },
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("operation.charge_payment"),
            schema: id("schema.InventoryReserved"),
            path: FieldPath(vec!["does_not_exist".to_owned()]),
        }]
    );
}

/// The fixture's `tx.apply_payment` transition step, whose
/// `effect_intents` map is the transition binding site.
fn apply_payment_transition(model: &mut Model) -> &mut StateTransition {
    let transaction = transaction_mut(model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::Transition(transition) = &mut transaction.steps[1] else {
        panic!("expected the mark_paid transition step");
    };

    transition
}

#[test]
fn accepts_transition_intent_derivation_from_preceding_read() {
    let mut model = load_flash_checkout();

    let transition = apply_payment_transition(&mut model);

    let Some(TransitionEffectIntent {
        values: Derivation::Deterministic { from },
        ..
    }) = transition.effect_intents.get(&id("effect.order.paid"))
    else {
        panic!("expected a deterministic transition intent derivation");
    };

    assert_eq!(
        from[0].source,
        ValueSource::TransactionRead(id("read.apply_payment.order"))
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn accepts_empty_effect_intents_for_transition_without_side_effects() {
    let model = load_flash_checkout();

    let transaction = model
        .operations
        .get(&id("operation.cancel_order"))
        .unwrap()
        .program
        .transaction(&id("tx.cancel_order"))
        .unwrap();

    let TransactionStep::Transition(transition) = &transaction.steps[0] else {
        panic!("expected the cancel transition step");
    };

    assert!(transition.effect_intents.is_empty());

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn rejects_missing_transition_intent_binding() {
    let mut model = load_flash_checkout();

    apply_payment_transition(&mut model).effect_intents.clear();

    // Clearing the map removes the binding's one producer, so the
    // intent execution must go with it.
    program_mut(&mut model, "operation.apply_payment")
        .steps
        .remove(1);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransitionEffectIntentsMismatch {
            transaction: id("tx.apply_payment"),
            transition: id("transition.order.mark_paid"),
            missing: vec![id("effect.order.paid")],
            unexpected: vec![],
        }]
    );
}

#[test]
fn rejects_extra_transition_intent_binding() {
    let mut model = load_flash_checkout();

    apply_payment_transition(&mut model).effect_intents.insert(
        id("effect.order.unrelated"),
        TransitionEffectIntent {
            bind: id("intent.apply_payment.unrelated"),
            values: Derivation::Unspecified,
        },
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransitionEffectIntentsMismatch {
            transaction: id("tx.apply_payment"),
            transition: id("transition.order.mark_paid"),
            missing: vec![],
            unexpected: vec![id("effect.order.unrelated")],
        }]
    );
}

#[test]
fn rejects_transition_intent_binding_owned_by_another_transition() {
    let mut model = load_flash_checkout();

    // transition.order.cancel declares no side effects, so mark_paid's
    // side effect has no instance here for a binding to establish.
    let transaction = transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order");

    let TransactionStep::Transition(transition) = &mut transaction.steps[0] else {
        panic!("expected the cancel transition step");
    };

    transition.effect_intents.insert(
        id("effect.order.paid"),
        TransitionEffectIntent {
            bind: id("intent.cancel_order.order_paid"),
            values: Derivation::Unspecified,
        },
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransitionEffectIntentsMismatch {
            transaction: id("tx.cancel_order"),
            transition: id("transition.order.cancel"),
            missing: vec![],
            unexpected: vec![id("effect.order.paid")],
        }]
    );
}

#[test]
fn rejects_transition_intent_derivation_referencing_later_read() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    // Move the transition ahead of the read its intent derivation
    // depends on.
    transaction.steps.swap(0, 1);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadOutOfOrder {
            transaction: id("tx.apply_payment"),
            read: id("read.apply_payment.order"),
        }]
    );
}

#[test]
fn rejects_transition_intent_derivation_field_not_selected() {
    let mut model = load_flash_checkout();

    let transition = apply_payment_transition(&mut model);

    let Some(TransitionEffectIntent {
        values: Derivation::Deterministic { from },
        ..
    }) = transition.effect_intents.get_mut(&id("effect.order.paid"))
    else {
        panic!("expected a deterministic transition intent derivation");
    };

    // `status` resolves against the order schema, but the read selects
    // only `order_id`.
    from[0].path = FieldPath(vec!["status".to_owned()]);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionReadFieldNotSelected {
            transaction: id("tx.apply_payment"),
            read: id("read.apply_payment.order"),
            path: FieldPath(vec!["status".to_owned()]),
        }]
    );
}

#[test]
fn rejects_invalid_field_path_in_transition_intent_derivation() {
    let mut model = load_flash_checkout();

    let transition = apply_payment_transition(&mut model);

    let Some(TransitionEffectIntent {
        values: Derivation::Deterministic { from },
        ..
    }) = transition.effect_intents.get_mut(&id("effect.order.paid"))
    else {
        panic!("expected a deterministic transition intent derivation");
    };

    from[1].path = FieldPath(vec!["does_not_exist".to_owned()]);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("tx.apply_payment"),
            schema: id("schema.PaymentCaptured"),
            path: FieldPath(vec!["does_not_exist".to_owned()]),
        }]
    );
}

#[test]
fn rejects_empty_request_identity() {
    let mut model = load_flash_checkout();

    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Keyed(conseqa::spec::RequestIdentityKey {
        fields: Vec::new(),
    });

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EmptyRequestIdentity {
            input: id("input.create_order.request"),
        }]
    );
}

#[test]
fn rejects_unresolvable_request_identity_field() {
    let mut model = load_flash_checkout();

    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.identity = RequestIdentity::Keyed(conseqa::spec::RequestIdentityKey {
        fields: vec![FieldPath(vec!["does_not_exist".to_owned()])],
    });

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("input.create_order.request"),
            schema: id("schema.CreateOrderRequest"),
            path: FieldPath(vec!["does_not_exist".to_owned()]),
        }]
    );
}

fn order_events_message_identity(
    model: &mut Model,
) -> &mut std::collections::BTreeMap<Id, Vec<FieldPath>> {
    let topic = model.topics.get_mut(&id("topic.order_events")).unwrap();

    let MessageIdentity::Keyed(identity) = &mut topic.message_identity else {
        panic!("order_events should declare a keyed message identity");
    };

    &mut identity.mapping
}

#[test]
fn rejects_message_identity_for_uncarried_schema() {
    let mut model = load_flash_checkout();

    // A declared schema, but not one carried by the topic.
    order_events_message_identity(&mut model).insert(
        id("schema.CreateOrderRequest"),
        vec![FieldPath(vec!["idempotency_key".to_owned()])],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::MessageIdentitySchemaNotOnTopic {
            topic: id("topic.order_events"),
            schema: id("schema.CreateOrderRequest"),
        }]
    );
}

#[test]
fn rejects_unknown_schema_in_message_identity() {
    let mut model = load_flash_checkout();

    order_events_message_identity(&mut model).insert(
        id("schema.missing"),
        vec![FieldPath(vec!["event_id".to_owned()])],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnknownReference {
            subject: id("topic.order_events"),
            reference: id("schema.missing"),
            expected: ReferenceKind::Schema,
        }]
    );
}

#[test]
fn rejects_empty_message_identity() {
    let mut model = load_flash_checkout();

    order_events_message_identity(&mut model).insert(id("schema.OrderCreated"), Vec::new());

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EmptyMessageIdentity {
            topic: id("topic.order_events"),
            schema: id("schema.OrderCreated"),
        }]
    );
}

#[test]
fn rejects_message_identity_arity_mismatch() {
    let mut model = load_flash_checkout();

    order_events_message_identity(&mut model).insert(
        id("schema.OrderCreated"),
        vec![
            FieldPath(vec!["event_id".to_owned()]),
            FieldPath(vec!["order_id".to_owned()]),
        ],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::MessageIdentityArityMismatch {
            topic: id("topic.order_events"),
            schema: id("schema.OrderCreated"),
            expected: 1,
            actual: 2,
        }]
    );
}

#[test]
fn rejects_unresolvable_message_identity_field() {
    let mut model = load_flash_checkout();

    order_events_message_identity(&mut model).insert(
        id("schema.OrderCreated"),
        vec![FieldPath(vec!["does_not_exist".to_owned()])],
    );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("topic.order_events"),
            schema: id("schema.OrderCreated"),
            path: FieldPath(vec!["does_not_exist".to_owned()]),
        }]
    );
}

// ---------------------------------------------------------------------------
// Request results, effect results, and the operation program
// ---------------------------------------------------------------------------

#[test]
fn rejects_request_result_schema_that_does_not_exist() {
    let mut model = load_flash_checkout();

    let Some(Input::Request(request)) = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .inputs
        .get_mut(&id("input.create_order.request"))
    else {
        panic!("create_order input should be a request");
    };

    request.result.err.schema = id("schema.missing");

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnknownReference {
            subject: id("input.create_order.request"),
            reference: id("schema.missing"),
            expected: ReferenceKind::Schema,
        }]
    );
}

/// Mutable access to the card charge's inline external contract, which
/// lives at the first step of charge_payment's program.
fn charge_card_mut(model: &mut Model) -> &mut conseqa::spec::ExternalEffect {
    let OperationStep::ExecuteEffect(step) =
        &mut program_mut(model, "operation.charge_payment").steps[0]
    else {
        panic!("expected the card-charge execute_effect step");
    };

    let conseqa::spec::Effect::External(card) = &mut step.effect else {
        panic!("card charge should be an external effect");
    };

    card
}

#[test]
fn rejects_external_result_schema_that_does_not_exist() {
    let mut model = load_flash_checkout();

    charge_card_mut(&mut model).result.as_mut().unwrap().ok = id("schema.missing");

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnknownReference {
            subject: id("effect.charge_payment.card"),
            reference: id("schema.missing"),
            expected: ReferenceKind::Schema,
        }]
    );
}

#[test]
fn rejects_a_result_binding_on_a_publication() {
    let mut model = load_flash_checkout();

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    let OperationStep::ExecuteEffect(captured) = &mut matched.ok.steps[0] else {
        panic!("expected the capture publication");
    };

    captured.bind = Some(id("result.charge_payment.captured"));

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EffectHasNoResult {
            operation: id("operation.charge_payment"),
            location: at(&[(1, Some(Arm::Ok)), (0, None)]),
            effect: id("effect.charge_payment.publish_captured"),
            result: id("result.charge_payment.captured"),
        }]
    );
}

#[test]
fn rejects_a_result_binding_on_an_external_effect_without_a_contract() {
    let mut model = load_flash_checkout();

    charge_card_mut(&mut model).result = None;

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EffectHasNoResult {
            operation: id("operation.charge_payment"),
            location: at(&[(0, None)]),
            effect: id("effect.charge_payment.card"),
            result: id("result.charge_payment.card"),
        }]
    );
}

#[test]
fn accepts_an_ignored_result() {
    let mut model = load_flash_checkout();

    // The provider's result may be ignored: the card charge executes
    // without binding it, and with nothing to match on the program
    // completes directly.
    let program = program_mut(&mut model, "operation.charge_payment");

    let OperationStep::ExecuteEffect(card) = &mut program.steps[0] else {
        panic!("expected the card charge");
    };

    card.bind = None;

    program.steps[1] = OperationStep::Complete;

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn rejects_a_match_on_a_result_no_step_declares() {
    let mut model = load_flash_checkout();

    let OperationStep::ExecuteEffect(card) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[0]
    else {
        panic!("expected the card charge");
    };

    card.bind = None;

    let errors = validation::validate(&model);

    // The match and the err arm's payload reference both name a binding
    // nothing declares.
    assert_eq!(
        errors,
        vec![
            ValidationError::UnknownReference {
                subject: id("operation.charge_payment"),
                reference: id("result.charge_payment.card"),
                expected: ReferenceKind::EffectResult,
            },
            ValidationError::UnknownReference {
                subject: id("operation.charge_payment"),
                reference: id("result.charge_payment.card"),
                expected: ReferenceKind::EffectResult,
            },
        ]
    );
}

#[test]
fn rejects_a_match_before_the_binding_step() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.charge_payment")
        .steps
        .swap(0, 1);

    let errors = validation::validate(&model);

    // The match runs before the card charge binds its result, and the
    // charge itself follows a decision whose every arm terminates.
    assert_eq!(
        errors,
        vec![
            ValidationError::EffectResultNotBound {
                operation: id("operation.charge_payment"),
                location: at(&[(0, None)]),
                result: id("result.charge_payment.card"),
                consumer: ProgramUse::Match,
            },
            ValidationError::UnreachableProgramStep {
                operation: id("operation.charge_payment"),
                location: at(&[(1, None)]),
            },
        ]
    );
}

#[test]
fn rejects_a_variant_payload_outside_its_arm() {
    let mut model = load_flash_checkout();

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    // The failure publication reads the err payload; moving it into the
    // ok arm puts that reference out of scope.
    let failed = matched.err.steps.remove(0);

    matched.ok.steps.insert(0, failed);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EffectResultVariantOutOfScope {
            operation: id("operation.charge_payment"),
            location: at(&[(1, Some(Arm::Ok)), (0, None)]),
            result: id("result.charge_payment.card"),
            variant: ResultVariant::Err,
            consumer: ProgramUse::Effect {
                effect: id("effect.charge_payment.publish_failed"),
            },
        }]
    );
}

#[test]
fn rejects_a_variant_payload_after_the_join() {
    let mut model = load_flash_checkout();

    let program = program_mut(&mut model, "operation.charge_payment");

    let OperationStep::MatchResult(matched) = &mut program.steps[1] else {
        panic!("expected the card match");
    };

    // Both arms fall through; the failure publication follows the join.
    let failed = matched.err.steps.remove(0);

    matched.ok.steps.clear();
    matched.err.steps.clear();

    program.steps.push(failed);
    program.steps.push(OperationStep::Complete);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EffectResultVariantOutOfScope {
            operation: id("operation.charge_payment"),
            location: at(&[(2, None)]),
            result: id("result.charge_payment.card"),
            variant: ResultVariant::Err,
            consumer: ProgramUse::Effect {
                effect: id("effect.charge_payment.publish_failed"),
            },
        }]
    );
}

#[test]
fn rejects_an_intent_executed_before_its_producer_transaction() {
    let mut model = load_flash_checkout();

    // Bindings exist only after their producer: executing the intent
    // ahead of the transaction that establishes it is a
    // use-before-bind error, never resolved by the later producer.
    program_mut(&mut model, "operation.create_order")
        .steps
        .swap(0, 1);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionArtifactNotAvailable {
            operation: id("operation.create_order"),
            location: at(&[(0, None)]),
            artifact: id("intent.create_order.publish_created"),
            consumer: ProgramUse::EffectIntent {
                intent: id("intent.create_order.publish_created"),
            },
        }]
    );
}

#[test]
fn an_output_with_no_producer_site_is_undeclared() {
    let mut model = load_flash_checkout();

    // Removing the binder removes the binding's declaration itself:
    // the return's references no longer resolve at all.
    transaction_mut(&mut model, "operation.create_order", "tx.create_order.new")
        .steps
        .retain(|step| !matches!(step, TransactionStep::EstablishTransactionOutput(_)));

    let errors = validation::validate(&model);

    assert!(!errors.is_empty());

    assert!(
        errors.iter().all(|error| matches!(
            error,
            ValidationError::UnknownReference {
                reference,
                expected: ReferenceKind::TransactionOutput,
                ..
            } if reference == &id("output.create_order")
        )),
        "expected unknown-reference errors for the unproduced binding, got:\n{errors:#?}"
    );
}

/// Wraps the first step of create_order's program — its one inline
/// transaction — in a branch on the request's `sku`: the transaction
/// moves into the `then` arm, and `otherwise` supplies the other arm.
fn branch_create_order(model: &mut Model, otherwise: Option<Vec<OperationStep>>) {
    let program = program_mut(model, "operation.create_order");

    let transaction = program.steps.remove(0);

    program.steps.insert(
        0,
        OperationStep::Branch(Branch {
            condition: Condition::Eq {
                value: input_ref("input.create_order.request", &["sku"]),
                equals: SelectorValue::Literal(Literal::String("bundle".into())),
            },
            then: OperationBlock {
                steps: vec![transaction],
            },
            otherwise: otherwise.map(|steps| OperationBlock { steps }),
        }),
    );
}

#[test]
fn accepts_an_artifact_established_on_every_falling_through_path() {
    let mut model = load_flash_checkout();

    // The other arm terminates, so the join is reached only through
    // the establishing arm: a terminated predecessor imposes no
    // condition on the join.
    branch_create_order(
        &mut model,
        Some(vec![return_err(
            "input.create_order.request",
            Derivation::Unspecified,
        )]),
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn rejects_an_artifact_established_on_one_arm_only() {
    let mut model = load_flash_checkout();

    branch_create_order(&mut model, None);

    let errors = validation::validate(&model);

    // Falling through the branch establishes nothing, so neither the
    // intent execution nor the returned payload is definitely supplied.
    assert_eq!(
        errors,
        vec![
            ValidationError::TransactionArtifactNotAvailable {
                operation: id("operation.create_order"),
                location: at(&[(1, None)]),
                artifact: id("intent.create_order.publish_created"),
                consumer: ProgramUse::EffectIntent {
                    intent: id("intent.create_order.publish_created"),
                },
            },
            ValidationError::TransactionArtifactNotAvailable {
                operation: id("operation.create_order"),
                location: at(&[(2, None)]),
                artifact: id("output.create_order"),
                consumer: ProgramUse::Return {
                    request: id("input.create_order.request"),
                },
            },
        ]
    );
}

#[test]
fn rejects_a_step_after_a_terminal() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.create_order")
        .steps
        .push(OperationStep::Complete);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnreachableProgramStep {
            operation: id("operation.create_order"),
            location: at(&[(3, None)]),
        }]
    );
}

#[test]
fn rejects_a_return_on_a_subscription_input() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.reserve_inventory").steps[2] =
        return_ok("input.reserve_inventory.created", Derivation::Unspecified);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidInputKind {
            subject: id("operation.reserve_inventory"),
            input: id("input.reserve_inventory.created"),
            expected: validation::InputKind::Request,
            actual: validation::InputKind::Subscription,
        }]
    );
}

#[test]
fn rejects_a_return_for_another_operations_request() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.reserve_inventory").steps[2] =
        return_ok("input.create_order.request", Derivation::Unspecified);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidReferenceOwner {
            subject: id("operation.reserve_inventory"),
            reference: id("input.create_order.request"),
            expected_owner: id("operation.reserve_inventory"),
            actual_owner: Some(id("operation.create_order")),
        }]
    );
}

#[test]
fn rejects_a_variant_field_path_that_does_not_resolve() {
    let mut model = load_flash_checkout();

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    let OperationStep::ExecuteEffect(failed) = &mut matched.err.steps[0] else {
        panic!("expected the failure publication");
    };

    let Derivation::Deterministic { from } = &mut failed.values else {
        panic!("expected a deterministic derivation");
    };

    from[2].path = path(&["does_not_exist"]);

    let errors = validation::validate(&model);

    // The err payload resolves against the provider's err schema.
    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("operation.charge_payment"),
            schema: id("schema.ChargeDeclined"),
            path: path(&["does_not_exist"]),
        }]
    );
}

#[test]
fn rejects_a_transaction_output_field_path_that_does_not_resolve() {
    let mut model = load_flash_checkout();

    let OperationStep::Return(returned) =
        &mut program_mut(&mut model, "operation.create_order").steps[2]
    else {
        panic!("expected the return");
    };

    let ResultOutcome::Ok {
        values: Derivation::Deterministic { from },
    } = &mut returned.outcome
    else {
        panic!("expected a deterministic ok outcome");
    };

    from[0].path = path(&["does_not_exist"]);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidFieldPath {
            subject: id("operation.create_order"),
            schema: id("schema.CreateOrderResponse"),
            path: path(&["does_not_exist"]),
        }]
    );
}

#[test]
fn rejects_duplicate_result_bindings() {
    let mut model = load_flash_checkout();

    let program = program_mut(&mut model, "operation.charge_payment");

    // A second execution site binding the same result: the site gets
    // its own effect_id, so the one collision is the binding's.
    let OperationStep::ExecuteEffect(mut step) = program.steps[0].clone() else {
        panic!("expected the card charge");
    };

    step.effect_id = id("effect.charge_payment.card.retry");

    program.steps.insert(0, OperationStep::ExecuteEffect(step));

    let errors = validation::validate(&model);

    assert_eq!(errors.len(), 1);

    assert!(matches!(
        &errors[0],
        ValidationError::DuplicateId { id: duplicate, .. }
            if duplicate == &id("result.charge_payment.card")
    ));
}

#[test]
fn rejects_a_condition_root_out_of_scope() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.create_order")
        .steps
        .insert(
            0,
            OperationStep::Branch(Branch {
                condition: Condition::Eq {
                    value: input_ref("input.cancel_order.request", &["order_id"]),
                    equals: SelectorValue::Literal(Literal::Int(1)),
                },
                then: OperationBlock::default(),
                otherwise: None,
            }),
        );

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::ValueSourceOutOfScope {
            subject: id("operation.create_order"),
            source: id("input.cancel_order.request"),
            owner: id("operation.cancel_order"),
        }]
    );
}

/// A second output for create_order: a new inline transaction whose
/// binder's derivation reads the first output.
fn receipt_transaction() -> OperationStep {
    OperationStep::Transaction(Transaction {
        id: id("tx.create_order.receipt"),
        data_model: None,
        isolation: TransactionIsolation::Unspecified,
        idempotency: IdempotencyGuarantee::Unspecified,
        steps: vec![TransactionStep::EstablishTransactionOutput(
            EstablishTransactionOutput {
                bind: id("output.create_order.receipt"),
                schema: id("schema.CreateOrderResponse"),
                values: Derivation::Deterministic {
                    from: vec![ValueRef {
                        source: ValueSource::TransactionOutput(id("output.create_order")),
                        path: path(&["order_id"]),
                    }],
                },
            },
        )],
    })
}

#[test]
fn accepts_an_output_consumed_by_a_later_transaction() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.create_order")
        .steps
        .insert(1, receipt_transaction());

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

#[test]
fn rejects_an_output_consumed_by_a_transaction_before_it_is_available() {
    let mut model = load_flash_checkout();

    program_mut(&mut model, "operation.create_order")
        .steps
        .insert(0, receipt_transaction());

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionArtifactNotAvailable {
            operation: id("operation.create_order"),
            location: at(&[(0, None)]),
            artifact: id("output.create_order"),
            consumer: ProgramUse::Transaction {
                transaction: id("tx.create_order.receipt"),
            },
        }]
    );
}

#[test]
fn a_same_transaction_output_reference_needs_only_step_order() {
    let mut model = load_flash_checkout();

    let establish = TransactionStep::EstablishTransactionOutput(EstablishTransactionOutput {
        bind: id("output.create_order.receipt"),
        schema: id("schema.CreateOrderResponse"),
        values: Derivation::Deterministic {
            from: vec![ValueRef {
                source: ValueSource::TransactionOutput(id("output.create_order")),
                path: path(&["order_id"]),
            }],
        },
    });

    // After the step that establishes the first output: satisfied by
    // atomicity, whatever the program guarantees at entry.
    let transaction = transaction_mut(&mut model, "operation.create_order", "tx.create_order.new");

    transaction.steps.push(establish.clone());

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");

    // Before it: the reference reads an output no step has produced.
    let transaction = transaction_mut(&mut model, "operation.create_order", "tx.create_order.new");

    transaction.steps.pop();
    transaction.steps.insert(0, establish);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionArtifactNotAvailable {
            operation: id("operation.create_order"),
            location: at(&[(0, None)]),
            artifact: id("output.create_order"),
            consumer: ProgramUse::Transaction {
                transaction: id("tx.create_order.new"),
            },
        }]
    );
}

#[test]
fn accepts_a_variant_payload_in_a_transaction_used_inside_its_arm() {
    let mut model = load_flash_checkout();

    // A transaction reading the provider's ok payload, executed only
    // inside the ok arm.
    let record = OperationStep::Transaction(Transaction {
        id: id("tx.charge_payment.record"),
        data_model: None,
        isolation: TransactionIsolation::Unspecified,
        idempotency: IdempotencyGuarantee::Unspecified,
        steps: vec![TransactionStep::EstablishTransactionOutput(
            EstablishTransactionOutput {
                bind: id("output.charge_payment.authorization"),
                schema: id("schema.ChargeAccepted"),
                values: Derivation::Deterministic {
                    from: vec![ValueRef {
                        source: ValueSource::EffectResultOk(id("result.charge_payment.card")),
                        path: path(&["authorization_id"]),
                    }],
                },
            },
        )],
    });

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    matched.ok.steps.insert(0, record.clone());

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");

    // Executed after the join, the same transaction reads a payload no
    // arm selects.
    let program = program_mut(&mut model, "operation.charge_payment");

    let OperationStep::MatchResult(matched) = &mut program.steps[1] else {
        panic!("expected the card match");
    };

    matched.ok.steps.remove(0);
    matched.ok.steps.pop();
    matched.err.steps.pop();

    program.steps.push(record);
    program.steps.push(OperationStep::Complete);

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::EffectResultVariantOutOfScope {
            operation: id("operation.charge_payment"),
            location: at(&[(2, None)]),
            result: id("result.charge_payment.card"),
            variant: ResultVariant::Ok,
            consumer: ProgramUse::Transaction {
                transaction: id("tx.charge_payment.record"),
            },
        }]
    );
}

#[test]
fn transition_application_evaluates_the_side_effects_declaration_roots() {
    let mut model = load_flash_checkout();

    // The transition-owned effect gains a propagation rooted in an
    // output a later transaction establishes; the contract stays on
    // the state machine, and the applying transaction is where its
    // roots are evaluated — before the producer, so the reference is
    // not definitely available there.
    program_mut(&mut model, "operation.apply_payment")
        .steps
        .insert(
            2,
            OperationStep::Transaction(Transaction {
                id: id("tx.apply_payment.receipt"),
                data_model: None,
                isolation: TransactionIsolation::Unspecified,
                idempotency: IdempotencyGuarantee::Unspecified,
                steps: vec![TransactionStep::EstablishTransactionOutput(
                    EstablishTransactionOutput {
                        bind: id("output.apply_payment.receipt"),
                        schema: id("schema.OrderPaid"),
                        values: Derivation::Deterministic {
                            from: vec![input_ref("input.apply_payment.captured", &["order_id"])],
                        },
                    },
                )],
            }),
        );

    let machine = model
        .state_machines
        .get_mut(&id("machine.order_lifecycle"))
        .unwrap();

    let transition = machine
        .transitions
        .get_mut(&id("transition.order.mark_paid"))
        .unwrap();

    let conseqa::spec::TransitionSideEffect::Publication(paid) = transition
        .side_effects
        .get_mut(&id("effect.order.paid"))
        .unwrap()
    else {
        panic!("effect.order.paid should be a publication");
    };

    paid.idempotency_key_propagation
        .push(conseqa::spec::IdempotencyKeyPropagation {
            source: conseqa::spec::IdempotencyKey {
                components: vec![ValueRef {
                    source: ValueSource::TransactionOutput(id("output.apply_payment.receipt")),
                    path: path(&["order_id"]),
                }],
            },
            target: conseqa::spec::IdempotencyKey {
                components: vec![ValueRef {
                    source: ValueSource::Effect(id("effect.order.paid")),
                    path: path(&["event_id"]),
                }],
            },
        });

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::TransactionArtifactNotAvailable {
            operation: id("operation.apply_payment"),
            location: at(&[(0, None)]),
            artifact: id("output.apply_payment.receipt"),
            consumer: ProgramUse::Transaction {
                transaction: id("tx.apply_payment"),
            },
        }]
    );
}

#[test]
fn a_nested_match_keeps_the_enclosing_arms_variant_selection() {
    let mut model = load_flash_checkout();

    // A redundant nested match on the same binding inside the ok arm;
    // the step after its join is still inside the outer ok arm, so the
    // ok payload stays in scope there.
    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    let OperationStep::ExecuteEffect(captured) = &mut matched.ok.steps[0] else {
        panic!("expected the capture publication");
    };

    let Derivation::Deterministic { from } = &mut captured.values else {
        panic!("expected a deterministic derivation");
    };

    from.push(ValueRef {
        source: ValueSource::EffectResultOk(id("result.charge_payment.card")),
        path: path(&["authorization_id"]),
    });

    matched.ok.steps.insert(
        0,
        OperationStep::MatchResult(conseqa::spec::MatchResult {
            result: id("result.charge_payment.card"),
            ok: OperationBlock::default(),
            err: OperationBlock::default(),
        }),
    );

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "expected no errors, got:\n{errors:#?}");
}

// The draft-commit gate's program-local check (`program_local_diagnostics`)
// reuses these very passes, so it cannot drift from the authority. These
// tests pin that contract: it stays silent on valid programs, reproduces
// the validator's verdict verbatim for operation-local breakages, and
// leaves external-reference errors — which the gate checks separately —
// to the full validator.

#[test]
fn program_local_diagnostics_pass_a_valid_model() {
    let model = load_flash_checkout();

    for operation in model.operations.keys() {
        let diagnostics = validation::program_local_diagnostics(&model, operation);

        assert!(
            diagnostics.is_empty(),
            "`{operation}` is valid but the program-local check reported:\n{diagnostics:#?}"
        );
    }
}

#[test]
fn program_local_diagnostics_reproduce_the_validator_verbatim() {
    let mut model = load_flash_checkout();

    // Executing the intent before the transaction that establishes it is
    // a use-before-bind error — exactly one operation-local verdict.
    program_mut(&mut model, "operation.create_order").steps.swap(0, 1);

    let validator: Vec<String> = validation::validate(&model)
        .into_iter()
        .map(|error| conseqa::analyzer::Diagnostic::from(error).message)
        .collect();

    let gate: Vec<String> =
        validation::program_local_diagnostics(&model, &id("operation.create_order"))
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect();

    assert_eq!(
        gate, validator,
        "the gate must reproduce the validator's operation-local verdict word for word"
    );
    assert!(!gate.is_empty(), "the mutation should produce a verdict");
}

#[test]
fn program_local_diagnostics_flag_a_dangling_effect_intent() {
    let mut model = load_flash_checkout();

    // Remove the establishment, leaving the execution referring to an
    // effect intent nothing produces — the reported "unknown effect
    // intent" failure that used to commit and only fail asynchronously.
    transaction_mut(&mut model, "operation.create_order", "tx.create_order.new")
        .steps
        .retain(|step| !matches!(step, TransactionStep::EstablishEffectIntent(_)));

    let diagnostics = validation::program_local_diagnostics(&model, &id("operation.create_order"));

    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("effect intent")
                && diagnostic.message.contains("intent.create_order.publish_created")
        }),
        "expected a dangling-effect-intent diagnostic, got:\n{diagnostics:#?}"
    );
}

#[test]
fn program_local_diagnostics_leave_external_references_to_the_validator() {
    let mut model = load_flash_checkout();

    // A missing data model is an external-reference error: the full
    // validator flags it, but the gate resolves shared references through
    // its own checks, so the program-local pass stays silent on it rather
    // than double-reporting or risking a false positive over a partial
    // draft model.
    transaction_mut(&mut model, "operation.create_order", "tx.create_order.new").data_model =
        Some(id("data.missing"));

    assert!(
        !validation::validate(&model).is_empty(),
        "the full validator should flag the missing data model"
    );

    let diagnostics = validation::program_local_diagnostics(&model, &id("operation.create_order"));

    assert!(
        diagnostics.is_empty(),
        "the program-local pass should not surface an external-reference error:\n{diagnostics:#?}"
    );
}

#[test]
fn program_local_diagnostics_do_not_fault_a_request_to_an_absent_operation() {
    let mut model = load_flash_checkout();

    // A request effect names another operation and its input. When the
    // gate validates one program against a probe that omits sibling
    // operations, that cross-operation reference is legitimately absent —
    // so it must not be faulted here. The gate resolves callee targets
    // separately (`require_request_target` and read-before-reference).
    // This exclusion is exactly what keeps the single-operation probe
    // sound.
    let request = OperationStep::ExecuteEffect(ExecuteEffect {
        effect_id: id("effect.create_order.call_absent"),
        effect: Effect::Request(RequestEffect {
            target: RequestTarget {
                operation: id("operation.absent"),
                input: id("input.absent"),
            },
            schema: id("schema.CreateOrderRequest"),
            retry: RetrySemantics::Unspecified,
            idempotency_key_propagation: Vec::new(),
        }),
        values: Derivation::Unspecified,
        bind: None,
    });

    program_mut(&mut model, "operation.create_order")
        .steps
        .insert(0, request);

    // The full validator faults the absent operation.
    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::UnknownReference { reference, .. }
                if reference == &id("operation.absent")
        )),
        "the full validator should fault the absent operation"
    );

    // The program-local gate check does not — the reference is external.
    let diagnostics = validation::program_local_diagnostics(&model, &id("operation.create_order"));

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("operation.absent")
                && !diagnostic.message.contains("input.absent")),
        "cross-operation references must be left to the gate's other checks:\n{diagnostics:#?}"
    );
}

// ---------------------------------------------------------------------------
// L1 — runtime topology
//
// Every L1 declaration hangs off an L0 one, and validation checks those
// anchors, the pools routing terminates at, and the field paths keys
// are written against. Whether the declared topology *proves* anything
// is verification's judgment, never validation's.
// ---------------------------------------------------------------------------

fn runtime(model: &mut Model) -> &mut RuntimeModel {
    model.runtime.as_mut().expect("the fixture declares a runtime")
}

#[test]
fn an_l0_only_model_is_structurally_valid() {
    // The refactor's first acceptance criterion: L0 stands alone.
    let mut model = load_flash_checkout();

    model.runtime = None;

    assert!(validation::validate(&model).is_empty());
}

#[test]
fn a_router_must_name_a_request_boundary() {
    let mut model = load_flash_checkout();

    runtime(&mut model).routers.insert(
        id("router.wrong_kind"),
        Router {
            boundary: OperationInputRef {
                operation: id("operation.reserve_inventory"),
                input: id("input.reserve_inventory.created"),
            },
            pool: id("pool.checkout_api"),
            routing: None,
        },
    );

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::InvalidInputKind { input, .. }
                if input == &id("input.reserve_inventory.created")
        )),
        "{errors:#?}"
    );
}

#[test]
fn a_router_key_must_be_non_empty_and_resolve_against_the_request_schema() {
    let mut model = load_flash_checkout();

    runtime(&mut model)
        .routers
        .get_mut(&id("router.create_order"))
        .expect("the fixture routes create_order")
        .routing = Some(RequestRouting {
        key: Vec::new(),
        member_assignment: MemberAssignment::ConsistentHash,
    });

    assert!(
        validation::validate(&model)
            .iter()
            .any(|error| matches!(error, ValidationError::EmptyRoutingKey { .. }))
    );

    runtime(&mut model)
        .routers
        .get_mut(&id("router.create_order"))
        .unwrap()
        .routing = Some(RequestRouting {
        key: vec![path(&["not_a_field"])],
        member_assignment: MemberAssignment::ConsistentHash,
    });

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::InvalidFieldPath { subject, .. }
                if subject == &id("router.create_order")
        ))
    );
}

#[test]
fn one_request_boundary_admits_at_most_one_router() {
    let mut model = load_flash_checkout();

    runtime(&mut model).routers.insert(
        id("router.create_order_again"),
        Router {
            boundary: OperationInputRef {
                operation: id("operation.create_order"),
                input: id("input.create_order.request"),
            },
            pool: id("pool.checkout_api"),
            routing: None,
        },
    );

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::DuplicateRouterForBoundary { first, second, .. }
                if first == &id("router.create_order")
                    && second == &id("router.create_order_again")
        ))
    );
}

#[test]
fn routing_must_terminate_at_a_declared_pool() {
    let mut model = load_flash_checkout();

    runtime(&mut model)
        .routers
        .get_mut(&id("router.create_order"))
        .unwrap()
        .pool = id("pool.missing");

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::UnknownReference { reference, expected, .. }
                if reference == &id("pool.missing")
                    && *expected == ReferenceKind::ExecutionPool
        ))
    );
}

#[test]
fn grouping_key_routing_requires_a_grouping_domain() {
    // `grouping_key` names the effective grouping domain, so one has
    // to exist. A validation error rather than a silent unproven
    // verdict, because the declaration would otherwise refer to
    // nothing.
    let mut model = load_flash_checkout();

    runtime(&mut model)
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap()
        .grouping = None;

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::RoutingWithoutGrouping { topic, .. }
                if topic == &id("topic.order_events")
        ))
    );
}

#[test]
fn an_unordered_transport_may_still_group() {
    // Grouping and ordering are independent facts. A transport that
    // orders nothing may still group by a key — the shape of an
    // unordered queue with consistent-hash workers — and that grouping
    // is enough for serialization to reason about, with no ordering
    // guarantee anywhere.
    let mut model = load_flash_checkout();

    runtime(&mut model)
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap()
        .ordering = Some(OrderingSemantics::None);

    assert!(
        validation::validate(&model).is_empty(),
        "grouping without ordering is a valid declaration"
    );
}

#[test]
fn within_group_requires_a_grouping_at_the_same_scope() {
    let mut model = load_flash_checkout();

    let topic = runtime(&mut model)
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap();

    topic.grouping = None;
    topic.ordering = Some(OrderingSemantics::WithinGroup);

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::WithinGroupWithoutGrouping { subject }
                if subject == &id("topic.order_events")
        ))
    );
}

#[test]
fn transport_semantics_may_not_be_declared_at_both_scopes() {
    // The scopes are exclusive: a topic declaring transport semantics
    // supplies them to every subscription, and none may declare its
    // own. There is no override rule to resolve.
    let mut model = load_flash_checkout();

    let subscription = runtime(&mut model)
        .subscriptions
        .get_mut(&id("operation.reserve_inventory"))
        .and_then(|inputs| inputs.get_mut(&id("input.reserve_inventory.created")))
        .expect("the fixture declares it");

    subscription.ordering = Some(OrderingSemantics::Global);

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::TransportSemanticsAtBothScopes { topic, input, .. }
                if topic == &id("topic.order_events")
                    && input == &id("input.reserve_inventory.created")
        ))
    );
}

#[test]
fn grouping_and_ordering_are_each_present_or_absent() {
    // There is no half a declaration to write. A grouping is its own
    // presence, and an absent ordering is `none`, so "groups without
    // ordering" is a complete statement — the unordered-queue shape —
    // rather than an unpaired one needing a rule to reject it.
    let mut model = load_flash_checkout();

    let topic = runtime(&mut model)
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap();

    topic.ordering = Some(OrderingSemantics::None);

    assert!(validation::validate(&model).is_empty());

    // And the reverse: a precedence with no grouping of its own, which
    // §11 of the patch admits because global order needs no key.
    let topic = runtime(&mut model)
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap();

    topic.grouping = None;
    topic.ordering = Some(OrderingSemantics::Global);

    // Only the routing declaration objects, because `grouping_key`
    // routing has lost the domain it names — not the transport
    // declaration itself.
    let errors = validation::validate(&model);

    assert!(
        errors.iter().all(|error| matches!(
            error,
            ValidationError::RoutingWithoutGrouping { .. }
        )),
        "{errors:#?}"
    );
}

#[test]
fn a_storage_layout_must_name_an_object_and_carry_a_resolving_partition_key() {
    let mut model = load_flash_checkout();

    runtime(&mut model)
        .storage_layouts
        .get_mut(&id("layout.order"))
        .expect("the fixture lays out the order object")
        .partition_key = Vec::new();

    assert!(
        validation::validate(&model)
            .iter()
            .any(|error| matches!(error, ValidationError::EmptyPartitionKey { .. }))
    );

    runtime(&mut model)
        .storage_layouts
        .get_mut(&id("layout.order"))
        .unwrap()
        .partition_key = vec![path(&["not_a_field"])];

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::InvalidFieldPath { subject, .. } if subject == &id("layout.order")
        ))
    );

    runtime(&mut model).storage_layouts.insert(
        id("layout.absent"),
        StorageLayout {
            object: DataObjectRef {
                data_model: id("data.checkout"),
                object: id("object.missing"),
            },
            partition_key: vec![path(&["order_id"])],
        },
    );

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::UnknownReference { reference, expected, .. }
                if reference == &id("object.missing")
                    && *expected == ReferenceKind::DataObject
        ))
    );
}

#[test]
fn one_data_object_admits_at_most_one_storage_layout() {
    let mut model = load_flash_checkout();

    runtime(&mut model).storage_layouts.insert(
        id("layout.order_again"),
        StorageLayout {
            object: DataObjectRef {
                data_model: id("data.checkout"),
                object: id("object.order"),
            },
            partition_key: vec![path(&["order_id"])],
        },
    );

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::DuplicateStorageLayoutForObject { first, second, .. }
                if first == &id("layout.order") && second == &id("layout.order_again")
        ))
    );
}

#[test]
fn l1_identifiers_share_the_one_global_namespace() {
    let mut model = load_flash_checkout();

    let pool = runtime(&mut model)
        .execution_pools
        .remove(&id("pool.checkout_api"))
        .expect("the fixture declares it");

    // A pool taking a topic's ID collides, exactly as two topics would.
    runtime(&mut model)
        .execution_pools
        .insert(id("topic.order_events"), pool);

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::DuplicateId { id: duplicate, .. }
                if duplicate == &id("topic.order_events")
        ))
    );
}

#[test]
fn a_subscription_runtime_must_name_a_subscription_boundary() {
    let mut model = load_flash_checkout();

    let dispatch = conseqa::spec::SubscriptionDispatch {
        pool: id("pool.order_workers"),
        routing: Some(conseqa::spec::SubscriptionRouting {
            key: SubscriptionRoutingKey::GroupingKey,
            member_assignment: MemberAssignment::ConsistentHash,
        }),
    };

    runtime(&mut model)
        .subscriptions
        .entry(id("operation.create_order"))
        .or_default()
        .insert(
            id("input.create_order.request"),
            conseqa::spec::SubscriptionRuntime {
                delivery: conseqa::spec::DeliverySemantics::AtLeastOnce,
                grouping: None,
                ordering: None,
                dispatch,
            },
        );

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::InvalidInputKind { input, .. }
                if input == &id("input.create_order.request")
        ))
    );
}

#[test]
fn a_subscription_groups_only_the_schemas_it_admits() {
    // Regression. The topic-scope coverage rule was applied at
    // subscription scope, forcing a subscription to map a schema its
    // own selector filters out — impossible on a heterogeneous topic
    // where that schema has no comparable field.
    let mut model = load_flash_checkout();

    let topic = runtime(&mut model)
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap();

    topic.grouping = None;
    topic.ordering = None;

    let everything = conseqa::spec::GroupingKey {
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
        ("operation.charge_payment", "input.charge_payment.reserved"),
        ("operation.apply_payment", "input.apply_payment.captured"),
    ] {
        let subscription = runtime(&mut model)
            .subscriptions
            .get_mut(&id(operation))
            .and_then(|inputs| inputs.get_mut(&id(input)))
            .expect("the fixture declares it");

        subscription.grouping = Some(everything.clone());
        subscription.ordering = Some(OrderingSemantics::WithinGroup);
    }

    // reserve_inventory admits only OrderCreated, so that is all its
    // grouping has to place in a group.
    let subscription = runtime(&mut model)
        .subscriptions
        .get_mut(&id("operation.reserve_inventory"))
        .and_then(|inputs| inputs.get_mut(&id("input.reserve_inventory.created")))
        .expect("the fixture declares it");

    subscription.grouping = Some(conseqa::spec::GroupingKey {
        mapping: [(id("schema.OrderCreated"), vec![path(&["order_id"])])]
            .into_iter()
            .collect(),
    });

    subscription.ordering = Some(OrderingSemantics::WithinGroup);

    assert!(
        validation::validate(&model).is_empty(),
        "{:#?}",
        validation::validate(&model)
    );
}

#[test]
fn a_bounded_member_concurrency_of_zero_is_unrepresentable() {
    // `NonZeroU32` carries the rule; there is nothing for validation to
    // check, and nothing an author can write that would need checking.
    let json = serde_json::to_string(&ExecutionPool {
        member_concurrency: MemberConcurrency::Bounded(
            std::num::NonZeroU32::new(1).expect("non-zero"),
        ),
    })
    .expect("serializes");

    assert_eq!(json, r#"{"member_concurrency":{"kind":"bounded","value":1}}"#);

    let zero: Result<ExecutionPool, _> =
        serde_json::from_str(r#"{"member_concurrency":{"kind":"bounded","value":0}}"#);

    assert!(zero.is_err(), "a bound of zero must not deserialize");
}
