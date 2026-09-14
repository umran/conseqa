//! The DSL v4 transaction-requirements matrix (§81 of the revision):
//! transaction rejection, transition-scoped outbox effects, strict
//! locks, the version protocol, the serializable-isolation closure,
//! the serialization-graph route, cursors, fences, ordering, and the
//! orthogonality of every transaction proof to the runtime topology.
//! Each test perturbs the flash-checkout fixture, whose `object.order`
//! is versioned and whose `tx.apply_payment` proves serializability by
//! version validation and ordering by a successor cursor.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use conseqa::{
    analyzer::{
        report::{self, Property, Status},
        validation::{self, ManagedRole, ValidationError, VersionFieldDefect},
        verification::{
            self, DecisionTaken, DependencyGap, IdempotencyProof, IdempotencyVerdict, ProofScope,
            TransactionOrderingObstacle, TransactionOrderingProof, TransactionOrderingVerdict,
            TransactionSerializabilityObstacle, TransactionSerializabilityProof,
            TransactionSerializabilityVerdict,
        },
    },
    parser::yaml,
    spec::{
        BumpVersion, CursorAdvanceRule, Derivation, ExecuteTransaction, Fence, FieldPath,
        FieldSelection, Id, IdempotencyGuarantee, Lock, LockMode, LockOrder, MessageIdentity,
        Model, ObjectSelector, OperationBlock, OperationStep, Outbox, OutboxWriteEffect,
        ResultOutcome, SelectorPredicate, SelectorValue, Transaction, TransactionIsolation,
        TransactionOutcome, TransactionStep, TransitionEffect, TransitionEffectApplication,
        ValueRef, ValueSource, Write,
    },
    viz::graph::{EdgeDetail, extract},
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

fn read_ref(read: &str, components: &[&str]) -> ValueRef {
    ValueRef {
        source: ValueSource::TransactionRead(id(read)),
        path: path(components),
    }
}

fn deterministic(from: Vec<ValueRef>) -> Derivation {
    Derivation::Deterministic { from }
}

fn load_flash_checkout() -> Model {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("flash_checkout.yaml");

    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture `{}`: {error}", path.display()));

    yaml::parse(&source).expect("flash checkout fixture should parse")
}

fn program_mut<'a>(model: &'a mut Model, operation: &str) -> &'a mut OperationBlock {
    &mut model.operations.get_mut(&id(operation)).unwrap().program
}

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

/// Mutable access to the whole transaction step — body and rejected
/// arm — wherever it sits in the program.
fn execute_mut<'a>(
    block: &'a mut OperationBlock,
    transaction: &Id,
) -> Option<&'a mut ExecuteTransaction> {
    for step in &mut block.steps {
        match step {
            OperationStep::Transaction(execute) => {
                if &execute.transaction.id == transaction {
                    return Some(execute);
                }

                if let Some(rejected) = &mut execute.rejected
                    && let Some(found) = execute_mut(rejected, transaction)
                {
                    return Some(found);
                }
            }

            OperationStep::MatchResult(matched) => {
                if let Some(found) = execute_mut(&mut matched.ok, transaction) {
                    return Some(found);
                }

                for arm in matched.errors.values_mut() {
                    if let Some(found) = execute_mut(arm, transaction) {
                        return Some(found);
                    }
                }
            }

            OperationStep::Branch(branch) => {
                if let Some(found) = execute_mut(&mut branch.then, transaction) {
                    return Some(found);
                }

                if let Some(otherwise) = &mut branch.otherwise
                    && let Some(found) = execute_mut(otherwise, transaction)
                {
                    return Some(found);
                }
            }

            _ => {}
        }
    }

    None
}

fn execution_mut<'a>(
    model: &'a mut Model,
    operation: &str,
    transaction: &str,
) -> &'a mut ExecuteTransaction {
    execute_mut(program_mut(model, operation), &id(transaction))
        .unwrap_or_else(|| panic!("`{operation}` should execute `{transaction}`"))
}

fn order_selector(input: &str) -> ObjectSelector {
    ObjectSelector {
        object: id("object.order"),
        predicate: SelectorPredicate::Eq {
            field: path(&["order_id"]),
            value: SelectorValue::Value(input_key(input, &["order_id"])),
        },
    }
}

fn stock_selector(input: &str) -> ObjectSelector {
    ObjectSelector {
        object: id("object.stock"),
        predicate: SelectorPredicate::And {
            predicates: vec![
                SelectorPredicate::Eq {
                    field: path(&["warehouse_id"]),
                    value: SelectorValue::Value(input_key(input, &["warehouse_id"])),
                },
                SelectorPredicate::Eq {
                    field: path(&["sku"]),
                    value: SelectorValue::Value(input_key(input, &["sku"])),
                },
            ],
        },
    }
}

fn serializability(model: &Model, transaction: &str) -> TransactionSerializabilityVerdict {
    verification::verify(model)
        .transaction_serializability
        .into_iter()
        .find(|check| check.transaction == id(transaction))
        .unwrap_or_else(|| panic!("`{transaction}` declares serializability"))
        .verdict
}

fn ordering(model: &Model, transaction: &str) -> TransactionOrderingVerdict {
    verification::verify(model)
        .transaction_ordering
        .into_iter()
        .find(|check| check.transaction == id(transaction))
        .unwrap_or_else(|| panic!("`{transaction}` declares ordering"))
        .verdict
}

fn serializability_obstacles(
    verdict: &TransactionSerializabilityVerdict,
) -> &[TransactionSerializabilityObstacle] {
    match verdict {
        TransactionSerializabilityVerdict::Unproven { obstacles } => obstacles,
        TransactionSerializabilityVerdict::Proven { proof, .. } => {
            panic!("expected an unproven verdict, found {proof:#?}")
        }
    }
}

fn ordering_obstacles(verdict: &TransactionOrderingVerdict) -> &[TransactionOrderingObstacle] {
    match verdict {
        TransactionOrderingVerdict::Unproven { obstacles } => obstacles,
        TransactionOrderingVerdict::Proven { proof, .. } => {
            panic!("expected an unproven verdict, found {proof:#?}")
        }
    }
}

/// Every dependency gap an unproven serializability verdict reports.
fn dependency_gaps(verdict: &TransactionSerializabilityVerdict) -> Vec<&DependencyGap> {
    serializability_obstacles(verdict)
        .iter()
        .flat_map(|obstacle| match obstacle {
            TransactionSerializabilityObstacle::TransactionSerializabilityUnprotectedReadWriteDependency { dependency }
            | TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedDependency { dependency } => {
                dependency.gaps.iter().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect()
}

// ---------------------------------------------------------------------
// The fixture as declared
// ---------------------------------------------------------------------

#[test]
fn flash_checkout_proves_apply_payment_and_leaves_reserve_inventory_unproven() {
    let model = load_flash_checkout();

    // apply_payment: serializable by version validation against
    // cancel_order and create_order's insert; ordered by the successor
    // cursor on the order's last applied sequence.
    let verdict = serializability(&model, "tx.apply_payment");

    let TransactionSerializabilityVerdict::Proven {
        proof:
            TransactionSerializabilityProof::ConflictGraph {
                closure,
                dependencies,
                ..
            },
        scope,
    } = &verdict
    else {
        panic!("expected apply_payment proven by the graph route: {verdict:#?}");
    };

    assert_eq!(*scope, ProofScope::L0Only);

    let members: BTreeSet<&Id> = closure.iter().map(|member| &member.transaction).collect();

    assert!(members.contains(&id("tx.cancel_order")), "{members:?}");
    assert!(members.contains(&id("tx.create_order.new")), "{members:?}");
    assert!(
        !members.contains(&id("tx.reserve_inventory")),
        "{members:?}"
    );

    assert!(
        dependencies
            .iter()
            .all(|dependency| dependency.constrained()),
        "{dependencies:#?}"
    );

    assert!(
        dependencies.iter().any(|dependency| matches!(
            dependency.evidence,
            verification::CommitOrderEvidence::VersionValidation { .. }
        )),
        "the proof should cite version validation:\n{dependencies:#?}"
    );

    let verdict = ordering(&model, "tx.apply_payment");

    assert!(
        matches!(
            &verdict,
            TransactionOrderingVerdict::Proven {
                proof: TransactionOrderingProof::Cursor {
                    rule: CursorAdvanceRule::Successor,
                    ..
                },
                scope: ProofScope::L0Only,
            }
        ),
        "{verdict:#?}"
    );

    // reserve_inventory: the read-then-write of the stock row under
    // read committed with neither lock nor version — write skew.
    let verdict = serializability(&model, "tx.reserve_inventory");

    let gaps = dependency_gaps(&verdict);

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::LockCoverageMissing { .. })),
        "{gaps:#?}"
    );

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::VersionValidationMissing { .. })),
        "{gaps:#?}"
    );

    assert!(
        serializability_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedCycle { cycle }
                if cycle.iter().any(|member| member.transaction == id("tx.transfer_stock"))
        )),
        "the cycle should pass through transfer_stock:\n{verdict:#?}"
    );
}

// ---------------------------------------------------------------------
// Transaction rejection
// ---------------------------------------------------------------------

#[test]
fn a_rejecting_body_requires_a_rejected_arm() {
    let mut model = load_flash_checkout();

    execution_mut(&mut model, "operation.apply_payment", "tx.apply_payment").rejected = None;

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::MissingTransactionRejectedArm { transaction, step: 1, .. }
                if transaction == &id("tx.apply_payment")
        )),
        "{errors:#?}"
    );
}

#[test]
fn a_body_that_cannot_reject_refuses_a_rejected_arm() {
    let mut model = load_flash_checkout();

    execution_mut(&mut model, "operation.create_order", "tx.create_order.new").rejected =
        Some(OperationBlock {
            steps: vec![OperationStep::Complete],
        });

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::UnexpectedTransactionRejectedArm {
            operation: id("operation.create_order"),
            location: conseqa::spec::StepLocation(vec![conseqa::spec::StepHop {
                step: 0,
                arm: None
            }]),
            transaction: id("tx.create_order.new"),
        }]
    );
}

#[test]
fn a_rejectable_transaction_forks_the_path() {
    let model = load_flash_checkout();

    let verdict = verification::verify(&model)
        .idempotency
        .into_iter()
        .find(|check| check.operation == id("operation.apply_payment"))
        .expect("apply_payment declares idempotency")
        .verdict;

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected apply_payment proven: {verdict:#?}");
    };

    let outcomes: BTreeSet<TransactionOutcome> = paths
        .iter()
        .flat_map(|path| path.decisions.iter())
        .filter_map(|decision| match &decision.decision {
            DecisionTaken::Transaction { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect();

    assert_eq!(paths.len(), 2, "{paths:#?}");
    assert_eq!(
        outcomes,
        BTreeSet::from([TransactionOutcome::Committed, TransactionOutcome::Rejected])
    );
}

#[test]
fn a_rejected_transaction_leaves_no_artifact_for_its_arm() {
    let mut model = load_flash_checkout();

    // The transition intent is established only by a committed
    // transaction, so the rejected arm cannot execute it.
    let execute = execution_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    execute.rejected.as_mut().unwrap().steps.insert(
        0,
        OperationStep::ExecuteEffectIntent(conseqa::spec::ExecuteEffectIntent {
            intent: id("intent.apply_payment.order_paid"),
            bind: None,
        }),
    );

    let errors = validation::validate(&model);

    assert!(!errors.is_empty());
    assert!(
        format!("{errors:?}").contains("intent.apply_payment.order_paid"),
        "{errors:#?}"
    );
}

#[test]
fn a_rejected_attempt_does_not_defeat_serializability() {
    // Rejected attempts are not committed executions: the proof over
    // the committed history is unchanged by their existence, and a
    // transaction with no keyed commit still proves.
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment").idempotency =
        IdempotencyGuarantee::Unspecified;

    assert!(validation::validate(&model).is_empty());

    assert!(matches!(
        serializability(&model, "tx.apply_payment"),
        TransactionSerializabilityVerdict::Proven { .. }
    ));
}

// ---------------------------------------------------------------------
// Transition-scoped outbox effects
// ---------------------------------------------------------------------

/// Declares an outbox on the checkout data model, a transition-scoped
/// admission of `OrderPaid` on `mark_paid`, and the applying
/// derivation on apply_payment's transition step.
fn admit_order_paid_through_the_transition(model: &mut Model) {
    model
        .data_models
        .get_mut(&id("data.checkout"))
        .unwrap()
        .outboxes
        .insert(
            id("outbox.order_events"),
            Outbox {
                messages: BTreeSet::from([id("schema.OrderPaid")]),
                message_identity: MessageIdentity::Keyed(conseqa::spec::MessageIdentityKey {
                    mapping: BTreeMap::from([(id("schema.OrderPaid"), vec![path(&["event_id"])])]),
                }),
            },
        );

    model
        .state_machines
        .get_mut(&id("machine.order_lifecycle"))
        .unwrap()
        .transitions
        .get_mut(&id("transition.order.mark_paid"))
        .unwrap()
        .effects
        .insert(
            id("effect.order.paid_admitted"),
            TransitionEffect::OutboxWrite(OutboxWriteEffect {
                outbox: id("outbox.order_events"),
                schema: id("schema.OrderPaid"),
                idempotency_key_propagation: Vec::new(),
            }),
        );

    // The outbox's one consumer: a relay that completes.
    model.operations.insert(
        id("operation.relay_order_paid"),
        conseqa::spec::Operation {
            service: id("service.checkout"),
            description: None,
            inputs: BTreeMap::from([(
                id("input.relay_order_paid.outbox"),
                conseqa::spec::Input::Outbox(conseqa::spec::OutboxInput {
                    outbox: id("outbox.order_events"),
                }),
            )]),
            program: OperationBlock {
                steps: vec![OperationStep::Complete],
            },
            requirements: conseqa::spec::OperationRequirements {
                idempotency: Vec::new(),
                recoverability: Vec::new(),
            },
        },
    );

    let transaction = transaction_mut(model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::Transition(transition) = &mut transaction.steps[3] else {
        panic!("expected the mark_paid transition");
    };

    transition.effects.insert(
        id("effect.order.paid_admitted"),
        TransitionEffectApplication {
            values: deterministic(vec![
                read_ref("read.apply_payment.order", &["order_id"]),
                input_key("input.apply_payment.captured", &["event_id"]),
            ]),
        },
    );
}

#[test]
fn a_transition_admits_an_outbox_message_atomically_with_its_application() {
    let mut model = load_flash_checkout();

    admit_order_paid_through_the_transition(&mut model);

    let errors = validation::validate(&model);

    assert!(errors.is_empty(), "{errors:#?}");

    // The admission is an outbox-write edge of the applying operation,
    // scoped to the transition and staged by the applying transaction.
    let graph = extract(&model);

    assert!(
        graph.edges.iter().any(|edge| matches!(
            &edge.detail,
            EdgeDetail::OutboxWrite {
                operation,
                transaction: Some(transaction),
                via_transition: Some(key),
                ..
            } if operation == &id("operation.apply_payment")
                && transaction == &id("tx.apply_payment")
                && key.transition == id("transition.order.mark_paid")
        )),
        "{:#?}",
        graph.edges
    );

    // The admission is not a database conflict: the serializability
    // proof is unchanged.
    assert!(matches!(
        serializability(&model, "tx.apply_payment"),
        TransactionSerializabilityVerdict::Proven { .. }
    ));
}

#[test]
fn a_transition_effect_needs_its_derivation_at_the_applying_site() {
    let mut model = load_flash_checkout();

    admit_order_paid_through_the_transition(&mut model);

    let transaction = transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::Transition(transition) = &mut transaction.steps[3] else {
        panic!("expected the mark_paid transition");
    };

    transition.effects.clear();

    let errors = validation::validate(&model);

    assert_eq!(
        errors,
        vec![ValidationError::InvalidTransitionOutboxDerivation {
            transaction: id("tx.apply_payment"),
            transition: id("transition.order.mark_paid"),
            missing: vec![id("effect.order.paid_admitted")],
            unexpected: Vec::new(),
        }]
    );
}

#[test]
fn a_transition_effect_targets_a_declared_outbox_of_the_applying_data_model() {
    let mut model = load_flash_checkout();

    admit_order_paid_through_the_transition(&mut model);

    fn effect_mut(model: &mut Model) -> &mut OutboxWriteEffect {
        let TransitionEffect::OutboxWrite(write) = model
            .state_machines
            .get_mut(&id("machine.order_lifecycle"))
            .unwrap()
            .transitions
            .get_mut(&id("transition.order.mark_paid"))
            .unwrap()
            .effects
            .get_mut(&id("effect.order.paid_admitted"))
            .unwrap();

        write
    }

    // An outbox nothing declares.
    let mut unknown = model.clone();
    effect_mut(&mut unknown).outbox = id("outbox.missing");

    assert!(
        validation::validate(&unknown).iter().any(|error| matches!(
            error,
            ValidationError::UnknownTransitionOutbox { outbox, .. } if outbox == &id("outbox.missing")
        )),
        "{:#?}",
        validation::validate(&unknown)
    );

    // A schema the outbox does not admit.
    let mut wrong_schema = model.clone();
    effect_mut(&mut wrong_schema).schema = id("schema.OrderCancelled");

    assert!(
        validation::validate(&wrong_schema)
            .iter()
            .any(|error| matches!(
                error,
                ValidationError::InvalidTransitionOutboxSchema { schema, .. }
                    if schema == &id("schema.OrderCancelled")
            )),
        "{:#?}",
        validation::validate(&wrong_schema)
    );

    // An outbox of another data model than the applying transaction's.
    let mut elsewhere = model.clone();

    let outbox = elsewhere
        .data_models
        .get_mut(&id("data.checkout"))
        .unwrap()
        .outboxes
        .remove(&id("outbox.order_events"))
        .unwrap();

    elsewhere
        .data_models
        .get_mut(&id("data.inventory"))
        .unwrap()
        .outboxes
        .insert(id("outbox.order_events"), outbox);

    assert!(
        validation::validate(&elsewhere)
            .iter()
            .any(|error| matches!(
                error,
                ValidationError::TransitionOutboxOutsideDataModel { data_model, .. }
                    if data_model == &id("data.checkout")
            )),
        "{:#?}",
        validation::validate(&elsewhere)
    );
}

// ---------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------

fn lock_stock(mode: LockMode) -> TransactionStep {
    TransactionStep::Lock(Lock {
        target: stock_selector("input.reserve_inventory.created"),
        mode,
        order: LockOrder::Unspecified,
    })
}

#[test]
fn a_strict_exclusive_lock_before_the_read_constrains_the_write_skew_edge() {
    let mut model = load_flash_checkout();

    transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    )
    .steps
    .insert(0, lock_stock(LockMode::Exclusive));

    assert!(validation::validate(&model).is_empty());

    // transfer_stock already locks both rows exclusively before it
    // touches them, so every edge of the closure is now covered by
    // strict locks and the graph route proves.
    let verdict = serializability(&model, "tx.reserve_inventory");

    let TransactionSerializabilityVerdict::Proven {
        proof: TransactionSerializabilityProof::ConflictGraph { dependencies, .. },
        ..
    } = &verdict
    else {
        panic!("expected reserve_inventory proven by locks: {verdict:#?}");
    };

    assert!(
        dependencies.iter().any(|dependency| matches!(
            dependency.evidence,
            verification::CommitOrderEvidence::StrictLock { .. }
        )),
        "{dependencies:#?}"
    );
}

#[test]
fn a_lock_acquired_after_the_access_covers_nothing() {
    let mut model = load_flash_checkout();

    // Between the read and the write: the read it should protect
    // already happened.
    transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    )
    .steps
    .insert(1, lock_stock(LockMode::Exclusive));

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.reserve_inventory");

    let gaps = dependency_gaps(&verdict);

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::LockAcquiredAfterProtectedAccess { .. })),
        "{gaps:#?}"
    );
}

#[test]
fn a_shared_lock_covers_the_reader_and_not_the_writer() {
    let mut model = load_flash_checkout();

    transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    )
    .steps
    .insert(0, lock_stock(LockMode::Shared));

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.reserve_inventory");

    let gaps = dependency_gaps(&verdict);

    // The writer side of reserve_inventory's own read-then-write holds
    // no exclusive lock.
    assert!(
        gaps.iter().any(|gap| matches!(
            gap,
            DependencyGap::LockCoverageMissing {
                side: verification::DependencySide::Writer,
                ..
            }
        )),
        "{gaps:#?}"
    );

    assert!(
        !gaps.iter().any(|gap| matches!(
            gap,
            DependencyGap::LockCoverageMissing {
                side: verification::DependencySide::Reader,
                ..
            }
        )),
        "the shared lock covers every read:\n{gaps:#?}"
    );
}

// ---------------------------------------------------------------------
// Versioning
// ---------------------------------------------------------------------

#[test]
fn an_ordinary_write_may_not_name_the_version_field() {
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .insert(
            2,
            TransactionStep::Write(Write {
                target: order_selector("input.cancel_order.request"),
                fields: BTreeSet::from([path(&["version"])]),
                values: deterministic(vec![input_key("input.cancel_order.request", &["order_id"])]),
            }),
        );

    let errors = validation::validate(&model);

    assert!(
        errors.contains(&ValidationError::DirectWriteToVersionField {
            transaction: id("tx.cancel_order"),
            step: 2,
            object: id("object.order"),
            field: path(&["version"]),
        }),
        "{errors:#?}"
    );
}

#[test]
fn a_mutation_of_a_versioned_instance_needs_exactly_one_bump() {
    let mut model = load_flash_checkout();

    // Without the bump.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .remove(3);

    assert_eq!(
        validation::validate(&model),
        vec![ValidationError::MissingVersionBump {
            transaction: id("tx.cancel_order"),
            step: 2,
            object: id("object.order"),
        }]
    );

    // With two.
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .insert(
            4,
            TransactionStep::BumpVersion(BumpVersion {
                target: order_selector("input.cancel_order.request"),
            }),
        );

    assert_eq!(
        validation::validate(&model),
        vec![ValidationError::DuplicateVersionBump {
            transaction: id("tx.cancel_order"),
            step: 4,
            object: id("object.order"),
        }]
    );
}

#[test]
fn a_version_validation_needs_an_observed_version() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order");

    let TransactionStep::Read(read) = &mut transaction.steps[0] else {
        panic!("expected the order read");
    };

    read.fields = FieldSelection::Only(BTreeSet::from([path(&["order_id"])]));

    // The validation names no observed version, and the `expected`
    // reference itself reaches a field the read no longer selects.
    assert_eq!(
        validation::validate(&model),
        vec![
            ValidationError::VersionValidationWithoutObservedVersion {
                transaction: id("tx.cancel_order"),
                step: 1,
                object: id("object.order"),
            },
            ValidationError::TransactionReadFieldNotSelected {
                transaction: id("tx.cancel_order"),
                read: id("read.cancel_order.order"),
                path: path(&["version"]),
            },
        ]
    );
}

#[test]
fn the_version_protocol_needs_a_versioned_object() {
    let mut model = load_flash_checkout();

    model
        .data_models
        .get_mut(&id("data.checkout"))
        .unwrap()
        .objects
        .get_mut(&id("object.order"))
        .unwrap()
        .version = None;

    let errors = validation::validate(&model);

    assert!(!errors.is_empty());
    assert!(
        errors.iter().all(|error| matches!(
            error,
            ValidationError::VersionProtocolOnUnversionedObject { object, .. }
                if object == &id("object.order")
        )),
        "{errors:#?}"
    );
}

#[test]
fn the_version_field_is_a_required_int_outside_the_identity() {
    for (field, expected) in [
        ("status", "not an int"),
        ("order_id", "an identity field"),
        ("no_such_field", "unresolved"),
    ] {
        let mut model = load_flash_checkout();

        model
            .data_models
            .get_mut(&id("data.checkout"))
            .unwrap()
            .objects
            .get_mut(&id("object.order"))
            .unwrap()
            .version = Some(conseqa::spec::ObjectVersion {
            field: path(&[field]),
        });

        let errors = validation::validate(&model);

        let defect = errors
            .iter()
            .find_map(|error| match error {
                ValidationError::InvalidObjectVersionField { defect, .. } => Some(defect),
                _ => None,
            })
            .unwrap_or_else(|| panic!("`{field}` should be refused as {expected}: {errors:#?}"));

        match field {
            "status" => assert!(
                matches!(defect, VersionFieldDefect::NotInt { .. }),
                "{defect:?}"
            ),
            "order_id" => assert!(
                matches!(defect, VersionFieldDefect::IdentityField),
                "{defect:?}"
            ),
            _ => assert!(
                matches!(defect, VersionFieldDefect::Unresolved),
                "{defect:?}"
            ),
        }
    }
}

#[test]
fn removing_the_validation_leaves_the_anti_dependency_unconstrained() {
    let mut model = load_flash_checkout();

    // apply_payment still reads the version and cancel_order still
    // bumps it; nothing now fixes the order across that edge.
    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment")
        .steps
        .remove(1);

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.apply_payment");

    let gaps = dependency_gaps(&verdict);

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::VersionValidationMissing { .. })),
        "{gaps:#?}"
    );

    // Ordering presupposes serializability.
    let verdict = ordering(&model, "tx.apply_payment");

    assert!(
        ordering_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionOrderingObstacle::OrderingMissingSerializability { .. }
        )),
        "{verdict:#?}"
    );
}

#[test]
fn an_unconstrained_anti_dependency_into_an_insert_names_the_insert() {
    let mut model = load_flash_checkout();

    // Without apply_payment's version validation, its anti-dependency
    // onto create_order.new's insert of `object.order` is unconstrained
    // too — a phantom-shaped conflict, not ordinary write skew, so the
    // prose must say "inserts a matching instance" and not "writes it".
    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment")
        .steps
        .remove(1);

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.apply_payment");

    let message = serializability_obstacles(&verdict)
        .iter()
        .find_map(|obstacle| match obstacle {
            TransactionSerializabilityObstacle::TransactionSerializabilityUnprotectedReadWriteDependency { dependency }
                if dependency.target.transaction == id("tx.create_order.new") =>
            {
                Some(obstacle.evidence().message)
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "expected an unprotected read-write dependency onto \
                 tx.create_order.new: {verdict:#?}"
            )
        });

    assert!(
        message.contains("inserts a matching instance"),
        "{message}"
    );
    assert!(!message.contains("writes it"), "{message}");
}

// ---------------------------------------------------------------------
// Serializable closure
// ---------------------------------------------------------------------

#[test]
fn serializable_isolation_across_the_closure_proves() {
    let mut model = load_flash_checkout();

    for (operation, transaction) in [
        ("operation.reserve_inventory", "tx.reserve_inventory"),
        ("operation.transfer_stock", "tx.transfer_stock"),
    ] {
        transaction_mut(&mut model, operation, transaction).isolation =
            TransactionIsolation::Serializable;
    }

    let verdict = serializability(&model, "tx.reserve_inventory");

    let TransactionSerializabilityVerdict::Proven {
        proof: TransactionSerializabilityProof::SerializableIsolationClosure { closure, .. },
        scope: ProofScope::L0Only,
    } = &verdict
    else {
        panic!("expected the isolation route: {verdict:#?}");
    };

    let members: BTreeSet<&Id> = closure.iter().map(|member| &member.transaction).collect();

    assert_eq!(
        members,
        BTreeSet::from([&id("tx.reserve_inventory"), &id("tx.transfer_stock")])
    );
}

#[test]
fn one_weaker_transaction_in_the_closure_defeats_the_isolation_route() {
    let mut model = load_flash_checkout();

    transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    )
    .isolation = TransactionIsolation::Serializable;

    let verdict = serializability(&model, "tx.reserve_inventory");

    assert!(
        serializability_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionSerializabilityObstacle::SerializableClosureContainsWeakerIsolation { transactions }
                if transactions.iter().any(|fact| {
                    fact.transaction.transaction == id("tx.transfer_stock")
                        && fact.isolation == TransactionIsolation::ReadCommitted
                })
        )),
        "{verdict:#?}"
    );
}

// ---------------------------------------------------------------------
// Cursors, fences, and ordering
// ---------------------------------------------------------------------

fn apply_payment_ordering_mut(
    model: &mut Model,
) -> &mut conseqa::spec::TransactionOrderingRequirement {
    &mut transaction_mut(model, "operation.apply_payment", "tx.apply_payment")
        .requirements
        .ordering[0]
}

#[test]
fn ordering_needs_a_cursor_or_fence() {
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment")
        .steps
        .remove(2);

    assert!(validation::validate(&model).is_empty());

    let verdict = ordering(&model, "tx.apply_payment");

    assert!(
        ordering_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionOrderingObstacle::OrderingMissingCursorOrFence
        )),
        "{verdict:#?}"
    );
}

#[test]
fn a_monotonic_cursor_proves_ordering_too() {
    let mut model = load_flash_checkout();

    let transaction = transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment");

    let TransactionStep::AdvanceCursor(advance) = &mut transaction.steps[2] else {
        panic!("expected the cursor advance");
    };

    advance.rule = CursorAdvanceRule::MonotonicAfter;

    assert!(validation::validate(&model).is_empty());

    assert!(matches!(
        ordering(&model, "tx.apply_payment"),
        TransactionOrderingVerdict::Proven {
            proof: TransactionOrderingProof::Cursor {
                rule: CursorAdvanceRule::MonotonicAfter,
                ..
            },
            ..
        }
    ));
}

#[test]
fn the_cursor_must_advance_by_the_requirement_position() {
    let mut model = load_flash_checkout();

    apply_payment_ordering_mut(&mut model).position =
        input_key("input.apply_payment.captured", &["amount"]);

    assert!(validation::validate(&model).is_empty());

    let verdict = ordering(&model, "tx.apply_payment");

    assert!(
        ordering_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionOrderingObstacle::OrderingPositionMismatch { step: 2, .. }
        )),
        "{verdict:#?}"
    );
}

#[test]
fn the_cursor_must_be_keyed_by_the_requirement_key() {
    let mut model = load_flash_checkout();

    apply_payment_ordering_mut(&mut model).key =
        input_key("input.apply_payment.captured", &["event_id"]);

    assert!(validation::validate(&model).is_empty());

    let verdict = ordering(&model, "tx.apply_payment");

    assert!(
        ordering_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionOrderingObstacle::OrderingKeyDomainMismatch { step: 2, object }
                if object == &id("object.order")
        )),
        "{verdict:#?}"
    );
}

#[test]
fn an_uncontrolled_writer_of_the_cursor_field_defeats_ordering() {
    let mut model = load_flash_checkout();

    // A direct write of the cursor field elsewhere: refused by
    // validation, and — verification staying total — an ordering
    // obstacle, since accepted positions no longer order every commit.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .insert(
            2,
            TransactionStep::Write(Write {
                target: order_selector("input.cancel_order.request"),
                fields: BTreeSet::from([path(&["last_applied_sequence"])]),
                values: Derivation::Unspecified,
            }),
        );

    let errors = validation::validate(&model);

    assert!(
        errors.contains(&ValidationError::DirectWriteToManagedField {
            transaction: id("tx.cancel_order"),
            step: 2,
            object: id("object.order"),
            field: path(&["last_applied_sequence"]),
            role: ManagedRole::Cursor {
                rule: CursorAdvanceRule::Successor,
            },
        }),
        "{errors:#?}"
    );

    let verdict = ordering(&model, "tx.apply_payment");

    assert!(
        ordering_obstacles(&verdict).iter().any(|obstacle| matches!(
            obstacle,
            TransactionOrderingObstacle::OrderingUncontrolledManagedFieldWriter { transaction, .. }
                if transaction.transaction == id("tx.cancel_order")
        )),
        "{verdict:#?}"
    );
}

#[test]
fn a_managed_field_has_one_role() {
    let mut model = load_flash_checkout();

    // cancel_order fences on the field apply_payment advances as a
    // cursor, with the observed version as its token.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .insert(
            2,
            TransactionStep::Fence(Fence {
                target: order_selector("input.cancel_order.request"),
                field: path(&["last_applied_sequence"]),
                token: read_ref("read.cancel_order.order", &["version"]),
            }),
        );

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::ManagedFieldRoleConflict { field, .. }
                if field == &path(&["last_applied_sequence"])
        )),
        "{errors:#?}"
    );
}

fn fence_apply_payment(model: &mut Model) {
    let transaction = transaction_mut(model, "operation.apply_payment", "tx.apply_payment");

    transaction.steps[2] = TransactionStep::Fence(Fence {
        target: order_selector("input.apply_payment.captured"),
        field: path(&["last_applied_sequence"]),
        token: input_key("input.apply_payment.captured", &["sequence"]),
    });
}

#[test]
fn a_fence_proves_ordering_over_a_serializable_closure() {
    let mut model = load_flash_checkout();

    fence_apply_payment(&mut model);

    assert!(validation::validate(&model).is_empty());

    assert!(matches!(
        ordering(&model, "tx.apply_payment"),
        TransactionOrderingVerdict::Proven {
            proof: TransactionOrderingProof::Fence { .. },
            scope: ProofScope::L0Only,
        }
    ));
}

#[test]
fn a_fence_alone_is_not_commit_order_evidence() {
    let mut model = load_flash_checkout();

    fence_apply_payment(&mut model);

    // Without the version validation, the fence is all that remains on
    // the anti-dependency into cancel_order: recorded, and not enough.
    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment")
        .steps
        .remove(1);

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.apply_payment");

    let recorded = serializability_obstacles(&verdict)
        .iter()
        .any(|obstacle| match obstacle {
            TransactionSerializabilityObstacle::TransactionSerializabilityUnprotectedReadWriteDependency { dependency }
            | TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedDependency { dependency } => {
                dependency.fence.is_some() && !dependency.constrained()
            }
            _ => false,
        });

    assert!(recorded, "{verdict:#?}");

    assert!(
        ordering_obstacles(&ordering(&model, "tx.apply_payment"))
            .iter()
            .any(|obstacle| matches!(
                obstacle,
                TransactionOrderingObstacle::OrderingMissingSerializability { .. }
            ))
    );
}

// ---------------------------------------------------------------------
// Entry availability and error classes
// ---------------------------------------------------------------------

#[test]
fn a_requirement_key_is_available_at_transaction_entry() {
    let mut model = load_flash_checkout();

    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment")
        .requirements
        .serializability[0]
        .key = read_ref("read.apply_payment.order", &["order_id"]);

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::TransactionRequirementKeyUnavailable {
                transaction,
                reason: conseqa::analyzer::validation::EntryUnavailability::TransactionRead { read },
                ..
            } if transaction == &id("tx.apply_payment") && read == &id("read.apply_payment.order")
        )),
        "{errors:#?}"
    );
}

#[test]
fn an_ordering_position_is_an_ordered_scalar() {
    let mut model = load_flash_checkout();

    apply_payment_ordering_mut(&mut model).position =
        input_key("input.apply_payment.captured", &["event_id"]);

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::TransactionOrderingPositionNotOrderedScalar { transaction, .. }
                if transaction == &id("tx.apply_payment")
        )),
        "{errors:#?}"
    );
}

#[test]
fn a_match_covers_exactly_the_declared_error_classes() {
    let mut model = load_flash_checkout();

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    let declined = matched.errors.remove(&id("declined")).unwrap();

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::MissingResultErrorArm { result, error, .. }
                if result == &id("result.charge_payment.card") && error == &id("declined")
        )),
        "{errors:#?}"
    );

    let OperationStep::MatchResult(matched) =
        &mut program_mut(&mut model, "operation.charge_payment").steps[1]
    else {
        panic!("expected the card match");
    };

    matched.errors.insert(id("declined"), declined);
    matched.errors.insert(
        id("throttled"),
        OperationBlock {
            steps: vec![OperationStep::Complete],
        },
    );

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::UnexpectedResultErrorArm { result, error, .. }
                if result == &id("result.charge_payment.card") && error == &id("throttled")
        )),
        "{errors:#?}"
    );
}

#[test]
fn a_return_names_a_declared_error_class() {
    let mut model = load_flash_checkout();

    let execute = execution_mut(&mut model, "operation.cancel_order", "tx.cancel_order");

    let OperationStep::Return(returned) = &mut execute.rejected.as_mut().unwrap().steps[0] else {
        panic!("expected the rejected return");
    };

    let ResultOutcome::Err { error, .. } = &mut returned.outcome else {
        panic!("the rejected arm returns an error");
    };

    *error = id("nope");

    let errors = validation::validate(&model);

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::UnknownResultErrorClass { request, error, .. }
                if request == &id("input.cancel_order.request") && error == &id("nope")
        )),
        "{errors:#?}"
    );
}

// ---------------------------------------------------------------------
// Orthogonality
// ---------------------------------------------------------------------

#[test]
fn no_transaction_verdict_depends_on_the_runtime_topology() {
    let model = load_flash_checkout();

    let mut stripped = model.clone();
    stripped.runtime = None;

    assert!(validation::validate(&stripped).is_empty());

    for transaction in ["tx.apply_payment", "tx.reserve_inventory"] {
        assert_eq!(
            serializability(&stripped, transaction),
            serializability(&model, transaction),
            "{transaction}"
        );
    }

    assert_eq!(
        ordering(&stripped, "tx.apply_payment"),
        ordering(&model, "tx.apply_payment")
    );

    // And every transaction obligation is L0-only, whatever the
    // topology declares.
    let obligations = report::obligations(&model, &verification::verify(&model));

    let transactional: Vec<_> = obligations
        .obligations
        .iter()
        .filter(|obligation| {
            matches!(
                obligation.property,
                Property::TransactionSerializability | Property::TransactionOrdering
            )
        })
        .collect();

    assert_eq!(transactional.len(), 3);

    for obligation in transactional {
        match obligation.status {
            Status::Proven => assert_eq!(
                obligation.scope,
                Some(ProofScope::L0Only),
                "{obligation:#?}"
            ),
            Status::Unknown => assert_eq!(
                obligation.remedy,
                Some(verification::RemedyLayer::Application),
                "{obligation:#?}"
            ),
            Status::Disproven => unreachable!(),
        }
    }
}

#[test]
fn a_serial_pool_and_affinity_prove_nothing_about_a_transaction() {
    // reserve_inventory runs on a serial, consistent-hash-routed pool
    // already, and stays unproven: placement is not commit-order evidence.
    let model = load_flash_checkout();

    let runtime = model
        .runtime
        .as_ref()
        .expect("the fixture declares a runtime");

    let pool = &runtime.execution_pools[&id("pool.order_workers")];

    assert!(matches!(
        pool.member_concurrency,
        conseqa::spec::MemberConcurrency::Bounded(n) if n.get() == 1
    ));

    assert!(matches!(
        serializability(&model, "tx.reserve_inventory"),
        TransactionSerializabilityVerdict::Unproven { .. }
    ));
}
