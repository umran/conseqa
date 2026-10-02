//! The transaction-requirements matrix (§81 of the DSL v4 revision,
//! §39 of the DSL 6 atomic-mutation revision): transaction rejection,
//! transition-scoped outbox effects, strict locks, atomic conditional
//! mutations — compare-and-set, guarded transitions and cursors, upsert
//! — with intrinsic version publication, the serializable-isolation
//! closure, the serialization-graph route, cursors, fences, ordering,
//! and the orthogonality of every transaction proof to the runtime
//! topology. Each test perturbs the flash-checkout fixture, whose
//! `object.order` is versioned and whose `tx.apply_payment` proves
//! serializability by conditioning its cursor advance on the observed
//! version, and ordering by that successor cursor.

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
        CompareAndSet, CompareCondition, CursorAdvanceRule, Derivation, ExecuteTransaction,
        Fence, FieldPath, FieldSelection, Id, IdempotencyGuarantee, Literal, Lock, LockMode,
        LockOrder, MessageIdentity, Model, ObjectSelector, OperationBlock, OperationStep, Outbox,
        OutboxWriteEffect, ResultOutcome, SelectorPredicate, SelectorValue, Transaction,
        TransactionIsolation, TransactionOutcome, TransactionStep, TransitionEffect,
        TransitionEffectApplication, Update, Upsert, ValueRef, ValueSource,
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

    // apply_payment: serializable by its cursor advance conditioned on
    // the observed version, against cancel_order and create_order's
    // insert — no writer declares anything; ordered by the successor
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
            &dependency.evidence,
            verification::CommitOrderEvidence::AtomicConditionalMutation {
                guarded_by,
                mechanism: verification::ConditionalMutationKind::AdvanceCursor,
                guard: verification::GuardCoverage::ObservedVersion { .. },
                ..
            } if guarded_by.transaction == id("tx.apply_payment")
                && dependency.target.transaction == id("tx.cancel_order")
        )),
        "the proof should cite the observed-version guard against cancel_order:\n\
         {dependencies:#?}"
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
    // read committed with neither lock nor guarded mutation — write
    // skew.
    let verdict = serializability(&model, "tx.reserve_inventory");

    let gaps = dependency_gaps(&verdict);

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::LockCoverageMissing { .. })),
        "{gaps:#?}"
    );

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::ObservedStateGuardMissing { .. })),
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

    let TransactionStep::Transition(transition) = &mut transaction.steps[2] else {
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

    let TransactionStep::Transition(transition) = &mut transaction.steps[2] else {
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
// Object versions and atomic conditional mutations
// ---------------------------------------------------------------------

/// A comparison of the order's version against the one `read` observed.
fn observed_version(read: &str) -> CompareCondition {
    CompareCondition {
        field: path(&["version"]),
        expected: SelectorValue::Value(read_ref(read, &["version"])),
    }
}

/// A comparison of a field against the value `read` observed of it.
fn observed(read: &str, field: &str) -> CompareCondition {
    CompareCondition {
        field: path(&[field]),
        expected: SelectorValue::Value(read_ref(read, &[field])),
    }
}

/// The comparisons apply_payment's cursor advance carries.
fn apply_payment_guard(model: &mut Model) -> &mut Vec<CompareCondition> {
    let TransactionStep::AdvanceCursor(advance) =
        &mut transaction_mut(model, "operation.apply_payment", "tx.apply_payment").steps[1]
    else {
        panic!("expected apply_payment's cursor advance");
    };

    &mut advance.compare
}

/// The comparisons cancel_order's transition carries.
fn cancel_order_guard(model: &mut Model) -> &mut Vec<CompareCondition> {
    let TransactionStep::Transition(transition) =
        &mut transaction_mut(model, "operation.cancel_order", "tx.cancel_order").steps[1]
    else {
        panic!("expected cancel_order's transition");
    };

    &mut transition.compare
}

/// Declares `SerializableBy(order_id)` on cancel_order's transaction.
fn require_cancel_order_serializable(model: &mut Model) {
    transaction_mut(model, "operation.cancel_order", "tx.cancel_order")
        .requirements
        .serializability = vec![conseqa::spec::TransactionSerializabilityRequirement {
        key: input_key("input.cancel_order.request", &["order_id"]),
    }];
}

/// Turns reserve_inventory's update of the stock row it read into a
/// compare-and-set of `compare`, and says what a rejection does.
fn reserve_inventory_compares(model: &mut Model, compare: Vec<CompareCondition>) {
    let transaction = transaction_mut(
        model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    );

    let TransactionStep::Update(update) = transaction.steps[1].clone() else {
        panic!("expected reserve_inventory's update");
    };

    transaction.steps[1] = TransactionStep::CompareAndSet(CompareAndSet {
        target: update.target,
        compare,
        fields: update.fields,
        values: update.values,
    });

    execution_mut(model, "operation.reserve_inventory", "tx.reserve_inventory").rejected =
        Some(OperationBlock {
            steps: vec![OperationStep::Complete],
        });
}

/// The stock row a literal identity names.
fn stock_at(warehouse: &str, sku: &str) -> ObjectSelector {
    ObjectSelector {
        object: id("object.stock"),
        predicate: SelectorPredicate::And {
            predicates: vec![
                SelectorPredicate::Eq {
                    field: path(&["warehouse_id"]),
                    value: SelectorValue::Literal(Literal::String(warehouse.into())),
                },
                SelectorPredicate::Eq {
                    field: path(&["sku"]),
                    value: SelectorValue::Literal(Literal::String(sku.into())),
                },
            ],
        },
    }
}

fn upsert_stock(target: ObjectSelector, update_fields: &[&str]) -> TransactionStep {
    TransactionStep::Upsert(Upsert {
        target,
        insert_values: deterministic(vec![input_key(
            "input.reserve_inventory.created",
            &["quantity"],
        )]),
        update_fields: update_fields.iter().map(|field| path(&[field])).collect(),
        update_values: deterministic(vec![input_key(
            "input.reserve_inventory.created",
            &["quantity"],
        )]),
    })
}

/// The constrained dependency evidence of a proven verdict.
fn proof_dependencies(verdict: &TransactionSerializabilityVerdict) -> &[verification::DependencyEvidence] {
    match verdict {
        TransactionSerializabilityVerdict::Proven {
            proof: TransactionSerializabilityProof::ConflictGraph { dependencies, .. },
            ..
        } => dependencies,
        other => panic!("expected a conflict-graph proof: {other:#?}"),
    }
}

#[test]
fn an_application_mutation_may_not_assign_the_version_field() {
    // Comparing the version is what the fixture does, and it validates.
    assert!(validation::validate(&load_flash_checkout()).is_empty());

    let version = BTreeSet::from([path(&["version"])]);

    let values = deterministic(vec![input_key("input.cancel_order.request", &["order_id"])]);

    for assigning in [
        TransactionStep::Update(Update {
            target: order_selector("input.cancel_order.request"),
            fields: version.clone(),
            values: values.clone(),
        }),
        TransactionStep::CompareAndSet(CompareAndSet {
            target: order_selector("input.cancel_order.request"),
            compare: vec![observed_version("read.cancel_order.order")],
            fields: version.clone(),
            values: values.clone(),
        }),
        TransactionStep::Upsert(Upsert {
            target: order_selector("input.cancel_order.request"),
            insert_values: values.clone(),
            update_fields: version.clone(),
            update_values: values.clone(),
        }),
    ] {
        let mut model = load_flash_checkout();

        transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
            .steps
            .insert(2, assigning.clone());

        let errors = validation::validate(&model);

        assert!(
            errors.contains(&ValidationError::DirectWriteToVersionField {
                transaction: id("tx.cancel_order"),
                step: 2,
                object: id("object.order"),
                field: path(&["version"]),
            }),
            "{assigning:?}: {errors:#?}"
        );
    }
}

#[test]
fn a_mutation_of_a_versioned_instance_publishes_its_version_without_a_step() {
    let mut model = load_flash_checkout();

    // An ordinary update of the versioned order: nothing beside it, and
    // nothing missing.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .insert(
            2,
            TransactionStep::Update(Update {
                target: order_selector("input.cancel_order.request"),
                fields: BTreeSet::from([path(&["amount"])]),
                values: deterministic(vec![input_key(
                    "input.cancel_order.request",
                    &["order_id"],
                )]),
            }),
        );

    assert!(validation::validate(&model).is_empty());

    // The conflict index nevertheless has both mutations — the
    // transition and the update — publishing a newer version.
    let index = verification::ConflictIndex::build(&model);

    let template = index
        .templates
        .iter()
        .find(|template| template.reference.transaction == id("tx.cancel_order"))
        .expect("cancel_order is indexed");

    let publications: Vec<usize> = template
        .accesses
        .iter()
        .filter(|access| access.mode == verification::AccessMode::VersionPublish)
        .inspect(|access| {
            assert_eq!(
                access.fields,
                verification::AccessFields::Only(BTreeSet::from([path(&["version"])]))
            )
        })
        .map(|access| access.step)
        .collect();

    assert_eq!(publications, vec![1, 2]);
}

#[test]
fn a_compare_and_set_identifies_one_instance_and_compares_something() {
    let valid = || {
        let mut model = load_flash_checkout();

        reserve_inventory_compares(
            &mut model,
            vec![observed("read.reserve_inventory.stock", "reserved")],
        );

        model
    };

    assert!(validation::validate(&valid()).is_empty());

    let cas = |model: &mut Model| -> CompareAndSet {
        let TransactionStep::CompareAndSet(cas) = transaction_mut(
            model,
            "operation.reserve_inventory",
            "tx.reserve_inventory",
        )
        .steps[1]
            .clone()
        else {
            panic!("expected the compare-and-set");
        };

        cas
    };

    let with = |edit: &dyn Fn(&mut CompareAndSet)| {
        let mut model = valid();
        let mut step = cas(&mut model);

        edit(&mut step);

        transaction_mut(
            &mut model,
            "operation.reserve_inventory",
            "tx.reserve_inventory",
        )
        .steps[1] = TransactionStep::CompareAndSet(step);

        validation::validate(&model)
    };

    let unidentified = ValidationError::CompareAndSetWithoutIdentifiedInstance {
        transaction: id("tx.reserve_inventory"),
        step: 1,
        object: id("object.stock"),
    };

    // Every stock row, and a row named by half its identity: a range.
    assert_eq!(
        with(&|cas| cas.target.predicate = SelectorPredicate::All),
        vec![unidentified.clone()]
    );

    assert_eq!(
        with(&|cas| {
            cas.target.predicate = SelectorPredicate::Eq {
                field: path(&["warehouse_id"]),
                value: SelectorValue::Value(input_key(
                    "input.reserve_inventory.created",
                    &["warehouse_id"],
                )),
            }
        }),
        vec![unidentified]
    );

    assert_eq!(
        with(&|cas| cas.compare.clear()),
        vec![ValidationError::CompareAndSetWithoutComparison {
            transaction: id("tx.reserve_inventory"),
            step: 1,
            object: id("object.stock"),
        }]
    );

    assert_eq!(
        with(&|cas| cas
            .compare
            .push(observed("read.reserve_inventory.stock", "reserved"))),
        vec![ValidationError::DuplicateCompareField {
            transaction: id("tx.reserve_inventory"),
            step: 1,
            object: id("object.stock"),
            field: path(&["reserved"]),
        }]
    );

    // An unknown field is the ordinary path check's.
    let errors = with(&|cas| {
        cas.compare.push(CompareCondition {
            field: path(&["no_such_field"]),
            expected: SelectorValue::Literal(Literal::Int(0)),
        })
    });

    assert!(
        !errors.is_empty() && format!("{errors:?}").contains("no_such_field"),
        "{errors:#?}"
    );
}

#[test]
fn a_compare_and_set_rejects_and_needs_a_rejected_arm() {
    let mut model = load_flash_checkout();

    reserve_inventory_compares(
        &mut model,
        vec![observed("read.reserve_inventory.stock", "reserved")],
    );

    assert!(validation::validate(&model).is_empty());

    execution_mut(&mut model, "operation.reserve_inventory", "tx.reserve_inventory").rejected =
        None;

    assert!(
        validation::validate(&model).iter().any(|error| matches!(
            error,
            ValidationError::MissingTransactionRejectedArm { transaction, step: 1, .. }
                if transaction == &id("tx.reserve_inventory")
        )),
        "a compare-and-set is a commit guard"
    );
}

/// The direct observed-field route, on the unversioned stock row: no
/// version is needed when the compare-and-set compares every field the
/// conflict touches against the value the read observed.
#[test]
fn an_observed_field_compare_and_set_constrains_the_anti_dependency() {
    let mut model = load_flash_checkout();

    assert!(
        model.data_models[&id("data.inventory")].objects[&id("object.stock")]
            .version
            .is_none(),
        "the stock row is unversioned"
    );

    reserve_inventory_compares(
        &mut model,
        vec![
            observed("read.reserve_inventory.stock", "on_hand"),
            observed("read.reserve_inventory.stock", "reserved"),
        ],
    );

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.reserve_inventory");

    assert!(
        proof_dependencies(&verdict).iter().any(|dependency| matches!(
            &dependency.evidence,
            verification::CommitOrderEvidence::AtomicConditionalMutation {
                mechanism: verification::ConditionalMutationKind::CompareAndSet,
                guard: verification::GuardCoverage::ObservedState { .. },
                ..
            }
        )),
        "{verdict:#?}"
    );

    // Compare only `reserved`: transfer_stock's update of `on_hand`, which
    // the read observed too, is no longer covered.
    let mut model = load_flash_checkout();

    reserve_inventory_compares(
        &mut model,
        vec![observed("read.reserve_inventory.stock", "reserved")],
    );

    let verdict = serializability(&model, "tx.reserve_inventory");

    assert!(
        dependency_gaps(&verdict).iter().any(|gap| matches!(
            gap,
            DependencyGap::ObservedStateGuardDoesNotCoverConflict {
                fields: verification::AccessFields::Only(fields),
                ..
            } if fields == &BTreeSet::from([path(&["on_hand"])])
        )),
        "{verdict:#?}"
    );
}

/// The version-token route with a compare-and-set: one comparison of the
/// observed version covers every field of the instance, and the writer —
/// apply_payment's cursor and transition — declares nothing.
#[test]
fn an_observed_version_compare_and_set_needs_no_writer_annotation() {
    let mut model = load_flash_checkout();

    require_cancel_order_serializable(&mut model);

    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order").steps[1] =
        TransactionStep::CompareAndSet(CompareAndSet {
            target: order_selector("input.cancel_order.request"),
            compare: vec![observed_version("read.cancel_order.order")],
            fields: BTreeSet::from([path(&["amount"])]),
            values: deterministic(vec![input_key(
                "input.cancel_order.request",
                &["order_id"],
            )]),
        });

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.cancel_order");

    assert!(
        proof_dependencies(&verdict).iter().any(|dependency| matches!(
            &dependency.evidence,
            verification::CommitOrderEvidence::AtomicConditionalMutation {
                guarded_by,
                mechanism: verification::ConditionalMutationKind::CompareAndSet,
                guard: verification::GuardCoverage::ObservedVersion { .. },
                ..
            } if guarded_by.transaction == id("tx.cancel_order")
                && dependency.kind == verification::DependencyKind::ReadWriteAntiDependency
                && dependency.target.transaction == id("tx.apply_payment")
        )),
        "{verdict:#?}"
    );
}

/// An expected version from outside the transaction's own read — the
/// request's, or a literal — is valid application behaviour, and no
/// evidence that the read stayed true.
#[test]
fn an_expected_value_from_outside_the_read_is_no_observation() {
    for expected in [
        SelectorValue::Value(input_key("input.cancel_order.request", &["expected_version"])),
        SelectorValue::Literal(Literal::Int(7)),
    ] {
        let mut model = load_flash_checkout();

        require_cancel_order_serializable(&mut model);

        let Some(conseqa::spec::Schema::Canonical(request)) = model
            .schemas
            .get_mut(&id("schema.CancelOrderRequest"))
        else {
            panic!("expected the request schema");
        };

        request.fields.insert(
            "expected_version".into(),
            conseqa::spec::Field {
                ty: conseqa::spec::TypeRef::Scalar(conseqa::spec::ScalarType::Int),
                optional: false,
            },
        );

        cancel_order_guard(&mut model)[0].expected = expected.clone();

        assert!(validation::validate(&model).is_empty(), "{expected:?}");

        let verdict = serializability(&model, "tx.cancel_order");

        assert!(
            dependency_gaps(&verdict).iter().any(|gap| matches!(
                gap,
                DependencyGap::ObservedStateGuardMissing { transaction, .. }
                    if transaction.transaction == id("tx.cancel_order")
            )),
            "{expected:?}: {verdict:#?}"
        );
    }
}

#[test]
fn a_transition_carrying_the_observed_version_constrains_its_read() {
    let mut model = load_flash_checkout();

    require_cancel_order_serializable(&mut model);

    let verdict = serializability(&model, "tx.cancel_order");

    assert!(
        proof_dependencies(&verdict).iter().any(|dependency| matches!(
            &dependency.evidence,
            verification::CommitOrderEvidence::AtomicConditionalMutation {
                guarded_by,
                mechanism: verification::ConditionalMutationKind::Transition,
                guard: verification::GuardCoverage::ObservedVersion { .. },
                ..
            } if guarded_by.transaction == id("tx.cancel_order")
        )),
        "{verdict:#?}"
    );

    // Without the comparison nothing else protects the read.
    cancel_order_guard(&mut model).clear();

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.cancel_order");

    assert!(
        dependency_gaps(&verdict).iter().any(|gap| matches!(
            gap,
            DependencyGap::ObservedStateGuardMissing { transaction, step: 0, .. }
                if transaction.transaction == id("tx.cancel_order")
        )),
        "{verdict:#?}"
    );
}

#[test]
fn removing_the_observed_version_guard_leaves_the_anti_dependency_unconstrained() {
    let mut model = load_flash_checkout();

    // apply_payment still reads the version and cancel_order still
    // publishes a newer one; nothing now fixes the order across that
    // edge.
    apply_payment_guard(&mut model).clear();

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.apply_payment");

    let gaps = dependency_gaps(&verdict);

    assert!(
        gaps.iter()
            .any(|gap| matches!(gap, DependencyGap::ObservedStateGuardMissing { .. })),
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

    // Without apply_payment's observed-version guard, its
    // anti-dependency onto create_order.new's insert of `object.order`
    // is unconstrained too — a phantom-shaped conflict, not ordinary
    // write skew, so the prose must say "inserts a matching instance"
    // and not "writes it".
    apply_payment_guard(&mut model).clear();

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

    assert!(message.contains("inserts a matching instance"), "{message}");
    assert!(!message.contains("writes it"), "{message}");
}

/// Insertion establishes a version token afresh. Where the object is
/// ever deleted, an instance inserted after a deletion may carry the very
/// token a reader observed of its predecessor, so the observed-version
/// guard covers no insertion of it.
#[test]
fn an_insertion_after_a_deletion_may_repeat_an_observed_version() {
    let mut model = load_flash_checkout();

    // cancel_order now deletes the order outright.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order").steps[1] =
        TransactionStep::Delete(conseqa::spec::Delete {
            target: order_selector("input.cancel_order.request"),
        });

    execution_mut(&mut model, "operation.cancel_order", "tx.cancel_order").rejected = None;

    let verdict = serializability(&model, "tx.apply_payment");

    assert!(
        dependency_gaps(&verdict).iter().any(|gap| matches!(
            gap,
            DependencyGap::ObservedVersionMayRepeat { deleted_by, .. }
                if deleted_by.transaction == id("tx.cancel_order")
        )),
        "{verdict:#?}"
    );
}

#[test]
fn an_upsert_identifies_one_instance_and_leaves_its_identity_alone() {
    let edit = |step: TransactionStep| {
        let mut model = load_flash_checkout();

        transaction_mut(
            &mut model,
            "operation.reserve_inventory",
            "tx.reserve_inventory",
        )
        .steps[1] = step;

        validation::validate(&model)
    };

    assert!(edit(upsert_stock(stock_selector("input.reserve_inventory.created"), &["reserved"])).is_empty());

    assert_eq!(
        edit(upsert_stock(
            ObjectSelector {
                object: id("object.stock"),
                predicate: SelectorPredicate::All,
            },
            &["reserved"]
        )),
        vec![ValidationError::UpsertWithoutIdentifiedInstance {
            transaction: id("tx.reserve_inventory"),
            step: 1,
            object: id("object.stock"),
        }]
    );

    assert_eq!(
        edit(upsert_stock(
            stock_selector("input.reserve_inventory.created"),
            &["reserved", "sku"]
        )),
        vec![ValidationError::UpsertMutatesIdentity {
            transaction: id("tx.reserve_inventory"),
            step: 1,
            object: id("object.stock"),
            field: path(&["sku"]),
        }]
    );
}

/// Two upserts that may name one identity conflict as atomic identity
/// mutations; upserts of identities proven disjoint do not.
#[test]
fn upserts_of_one_identity_conflict_and_of_disjoint_identities_do_not() {
    let mut model = load_flash_checkout();

    transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    )
    .steps = vec![upsert_stock(stock_at("w-1", "a"), &["reserved"])];

    transaction_mut(&mut model, "operation.transfer_stock", "tx.transfer_stock").steps =
        vec![upsert_stock(stock_at("w-1", "b"), &["reserved"])];

    let index = verification::ConflictIndex::build(&model);

    let upsert_of = |transaction: &str| {
        index
            .templates
            .iter()
            .find(|template| template.reference.transaction == id(transaction))
            .and_then(|template| {
                template
                    .accesses
                    .iter()
                    .find(|access| access.mode == verification::AccessMode::UpsertReadWrite)
            })
            .expect("the upsert is indexed")
    };

    let reserve = upsert_of("tx.reserve_inventory");
    let transfer = upsert_of("tx.transfer_stock");

    // Concurrent executions of one upsert name the same identity.
    assert!(index.conflict(reserve, reserve).is_some());

    // `sku = a` and `sku = b` never name one row.
    assert!(index.conflict(reserve, transfer).is_none());

    // An upsert whose identity comes from its input may name either.
    transaction_mut(&mut model, "operation.transfer_stock", "tx.transfer_stock").steps =
        vec![upsert_stock(stock_selector("input.transfer_stock.request"), &["reserved"])];

    let index = verification::ConflictIndex::build(&model);

    let reserve = index
        .templates
        .iter()
        .find(|template| template.reference.transaction == id("tx.reserve_inventory"))
        .map(|template| &template.accesses[0])
        .expect("indexed");

    let transfer = index
        .templates
        .iter()
        .find(|template| template.reference.transaction == id("tx.transfer_stock"))
        .map(|template| &template.accesses[0])
        .expect("indexed");

    assert!(index.conflict(reserve, transfer).is_some());
}

/// An upsert supplies evidence for its own identity arbitration and
/// mutation alone: it protects no read before it — not even one of the
/// instance it upserts.
#[test]
fn an_upsert_guards_no_earlier_read() {
    let mut model = load_flash_checkout();

    transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    )
    .steps[1] = upsert_stock(stock_selector("input.reserve_inventory.created"), &["reserved"]);

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.reserve_inventory");

    assert!(
        dependency_gaps(&verdict).iter().any(|gap| matches!(
            gap,
            DependencyGap::ObservedStateGuardMissing { transaction, step: 0, .. }
                if transaction.transaction == id("tx.reserve_inventory")
        )),
        "{verdict:#?}"
    );
}

/// The retired `read A; validate_version A; write B` has no translation:
/// a read-only observation is protected by a real mechanism — a lock, or
/// serializable isolation — or the obligation is refused.
#[test]
fn a_read_only_observation_needs_a_real_mechanism() {
    let read_a_write_b = || {
        let mut model = load_flash_checkout();

        let transaction = transaction_mut(
            &mut model,
            "operation.reserve_inventory",
            "tx.reserve_inventory",
        );

        let TransactionStep::Update(update) = &mut transaction.steps[1] else {
            panic!("expected the update");
        };

        update.target = stock_at("w-1", "b");

        model
    };

    let model = read_a_write_b();

    assert!(validation::validate(&model).is_empty());

    assert!(
        dependency_gaps(&serializability(&model, "tx.reserve_inventory"))
            .iter()
            .any(|gap| matches!(gap, DependencyGap::ObservedStateGuardMissing { step: 0, .. })),
        "nothing protects the read of A"
    );

    // A shared lock on A before its read, and an exclusive one on B.
    let mut model = read_a_write_b();

    let transaction = transaction_mut(
        &mut model,
        "operation.reserve_inventory",
        "tx.reserve_inventory",
    );

    transaction.steps.insert(0, lock_stock(LockMode::Shared));
    transaction.steps.insert(
        0,
        TransactionStep::Lock(Lock {
            target: stock_at("w-1", "b"),
            mode: LockMode::Exclusive,
            order: LockOrder::Unspecified,
        }),
    );

    assert!(validation::validate(&model).is_empty());

    assert!(matches!(
        serializability(&model, "tx.reserve_inventory"),
        TransactionSerializabilityVerdict::Proven { .. }
    ));

    // Or serializable isolation across the closure.
    let mut model = read_a_write_b();

    for (operation, transaction) in [
        ("operation.reserve_inventory", "tx.reserve_inventory"),
        ("operation.transfer_stock", "tx.transfer_stock"),
    ] {
        transaction_mut(&mut model, operation, transaction).isolation =
            TransactionIsolation::Serializable;
    }

    assert!(matches!(
        serializability(&model, "tx.reserve_inventory"),
        TransactionSerializabilityVerdict::Proven {
            proof: TransactionSerializabilityProof::SerializableIsolationClosure { .. },
            ..
        }
    ));
}

/// cancel_order reduced to its read of the order: a read-only
/// transaction, with nothing after it that needs what it established.
fn cancel_order_only_reads(model: &mut Model) {
    let program = program_mut(model, "operation.cancel_order");

    program
        .steps
        .retain(|step| !matches!(step, OperationStep::ExecuteEffectIntent(_)));

    let execution = execution_mut(model, "operation.cancel_order", "tx.cancel_order");

    execution.rejected = None;
    execution.transaction.steps.truncate(1);

    require_cancel_order_serializable(model);
}

/// A read-only transaction that observes committed state at one instant
/// serializes at that instant: every write it saw committed before it,
/// every write it missed commits after it. Its anti-dependencies need no
/// lock or comparison — but two reads at different instants are not one
/// observation, unless one snapshot serves both.
#[test]
fn a_read_only_observation_at_one_instant_serializes_at_its_read() {
    let mut model = load_flash_checkout();

    cancel_order_only_reads(&mut model);

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.cancel_order");

    assert!(
        proof_dependencies(&verdict).iter().any(|dependency| matches!(
            dependency.evidence,
            verification::CommitOrderEvidence::ReadOnlyObservation {
                isolation: TransactionIsolation::ReadCommitted
            }
        ) && dependency.source.transaction == id("tx.cancel_order")),
        "{verdict:#?}"
    );

    // A second read under read committed: read skew can pass between
    // the two, so neither is covered.
    let transaction = transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order");

    let TransactionStep::Read(read) = transaction.steps[0].clone() else {
        panic!("expected the read");
    };

    transaction.steps.push(TransactionStep::Read(conseqa::spec::Read {
        bind: id("read.cancel_order.again"),
        ..read
    }));

    assert!(validation::validate(&model).is_empty());

    assert!(
        dependency_gaps(&serializability(&model, "tx.cancel_order"))
            .iter()
            .any(|gap| matches!(gap, DependencyGap::ObservedStateGuardMissing { .. })),
        "two reads at two instants are not one observation"
    );

    // One snapshot serves both reads.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order").isolation =
        TransactionIsolation::Snapshot;

    assert!(matches!(
        serializability(&model, "tx.cancel_order"),
        TransactionSerializabilityVerdict::Proven { .. }
    ));

    // Without declared isolation nothing says the read saw only
    // committed writes.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order").isolation =
        TransactionIsolation::Unspecified;

    assert!(matches!(
        serializability(&model, "tx.cancel_order"),
        TransactionSerializabilityVerdict::Unproven { .. }
    ));
}

/// A reader that locks the instance before observing it, against a
/// writer whose mutation is guarded: the guarded mutation must acquire
/// the instance's write protection, which the reader's lock withholds
/// until it commits — no exclusive lock on the writer is needed.
#[test]
fn a_locked_read_orders_a_guarded_writer() {
    let locked_reader_against_a_guard = || {
        let mut model = load_flash_checkout();

        reserve_inventory_compares(
            &mut model,
            vec![
                observed("read.reserve_inventory.stock", "on_hand"),
                observed("read.reserve_inventory.stock", "reserved"),
            ],
        );

        // transfer_stock, which locks the rows before it reads them,
        // now also reads the `reserved` the compare-and-set writes.
        let transfer =
            transaction_mut(&mut model, "operation.transfer_stock", "tx.transfer_stock");

        let TransactionStep::Read(read) = &mut transfer.steps[2] else {
            panic!("expected transfer_stock's read");
        };

        let FieldSelection::Only(fields) = &mut read.fields else {
            panic!("expected a narrowed read");
        };

        fields.insert(path(&["reserved"]));

        model
    };

    let model = locked_reader_against_a_guard();

    assert!(validation::validate(&model).is_empty());

    let verdict = serializability(&model, "tx.reserve_inventory");

    assert!(
        proof_dependencies(&verdict).iter().any(|dependency| matches!(
            &dependency.evidence,
            verification::CommitOrderEvidence::AtomicConditionalMutation {
                guarded_by,
                guard: verification::GuardCoverage::LockedReader { .. },
                ..
            } if guarded_by.transaction == id("tx.reserve_inventory")
                && dependency.source.transaction == id("tx.transfer_stock")
        )),
        "{verdict:#?}"
    );

    // Without its locks, transfer_stock's read is protected by nothing.
    let mut model = locked_reader_against_a_guard();

    transaction_mut(&mut model, "operation.transfer_stock", "tx.transfer_stock")
        .steps
        .retain(|step| !matches!(step, TransactionStep::Lock(_)));

    assert!(matches!(
        serializability(&model, "tx.reserve_inventory"),
        TransactionSerializabilityVerdict::Unproven { .. }
    ));
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

/// Moves apply_payment's observed-version guard from its cursor advance
/// to its transition, which reads and mutates the same order.
fn guard_apply_payment_on_its_transition(model: &mut Model) {
    let guard = std::mem::take(apply_payment_guard(model));

    let TransactionStep::Transition(transition) =
        &mut transaction_mut(model, "operation.apply_payment", "tx.apply_payment").steps[2]
    else {
        panic!("expected apply_payment's transition");
    };

    transition.compare = guard;
}

#[test]
fn ordering_needs_a_cursor_or_fence() {
    let mut model = load_flash_checkout();

    guard_apply_payment_on_its_transition(&mut model);

    transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment")
        .steps
        .remove(1);

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

    let TransactionStep::AdvanceCursor(advance) = &mut transaction.steps[1] else {
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
            TransactionOrderingObstacle::OrderingPositionMismatch { step: 1, .. }
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
            TransactionOrderingObstacle::OrderingKeyDomainMismatch { step: 1, object }
                if object == &id("object.order")
        )),
        "{verdict:#?}"
    );
}

#[test]
fn an_uncontrolled_writer_of_the_cursor_field_defeats_ordering() {
    let mut model = load_flash_checkout();

    // A direct update of the cursor field elsewhere: refused by
    // validation, and — verification staying total — an ordering
    // obstacle, since accepted positions no longer order every commit.
    transaction_mut(&mut model, "operation.cancel_order", "tx.cancel_order")
        .steps
        .insert(
            2,
            TransactionStep::Update(Update {
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
                compare: Vec::new(),
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

/// Replaces apply_payment's cursor with a fence on the same field. A
/// fence holds no write protection on an equal token, so the observed-
/// version guard moves to the transition first.
fn fence_apply_payment(model: &mut Model) {
    guard_apply_payment_on_its_transition(model);

    let transaction = transaction_mut(model, "operation.apply_payment", "tx.apply_payment");

    transaction.steps[1] = TransactionStep::Fence(Fence {
        target: order_selector("input.apply_payment.captured"),
        field: path(&["last_applied_sequence"]),
        token: input_key("input.apply_payment.captured", &["sequence"]),
        compare: Vec::new(),
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

    // Without the transition's observed-version guard, the fence is all
    // that remains: recorded on the edges between fences, and not
    // enough.
    let TransactionStep::Transition(transition) =
        &mut transaction_mut(&mut model, "operation.apply_payment", "tx.apply_payment").steps[2]
    else {
        panic!("expected the transition");
    };

    transition.compare.clear();

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

