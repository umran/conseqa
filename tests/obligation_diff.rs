//! The obligation diff, compared in memory (§11–§14 of the spec change
//! analysis revision; its tests §19.2–§19.9): the explanation corpus,
//! and the cases the test plan names one by one.

use std::{collections::BTreeMap, fs, path::Path};

use serde_json::Value;

use conseqa::{
    analyzer::{
        self,
        basis::{
            Coverage, DeclarationKey, Fact, Layer, Mechanism, ObligationKey, ObligationProperty,
            ObligationSubject,
        },
        report::{self, Obligation, Status},
    },
    design::{
        OperationDraftStage, RunId, RunMetadata, WorkspaceState,
        change::{
            diff::{
                Change, ClassCounts, DiffDocument, Gate, Grounds, ObligationClass, ObligationDiff,
                ObligationEntry, RelianceDelta, UnexplainedEntry, WithdrawnReliance, diff,
            },
            mutation::{self, Mutation},
            side::{Side, SideName, Source},
        },
    },
    parser::yaml,
    spec::{
        CompletionRequirement, DeliverySemantics, FieldPath, Id, IdempotencyKey,
        IdempotencyRequirement, ResultReplayRequirement, TransactionIsolation,
        TransactionSerializabilityRequirement, ValueRef, ValueSource,
    },
};

const FIXTURES: [&str; 9] = [
    "minimal.yaml",
    "flash_checkout.yaml",
    "payment_capture.yaml",
    "keyed_topic.yaml",
    "video_streaming.yaml",
    "shop.yaml",
    "transactional_outbox.yaml",
    "tenant_ledger.yaml",
    "hedged_read.yaml",
];

fn fixture(name: &str) -> WorkspaceState {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);

    let model = yaml::parse(&fs::read_to_string(path).unwrap())
        .unwrap_or_else(|error| panic!("{name}: {error}"));

    WorkspaceState::from_model(&model, RunMetadata::new(RunId(name.to_string())))
}

fn side(name: SideName, workspace: WorkspaceState) -> Side {
    Side {
        name,
        source: Source::WorkingTree,
        commit: None,
        merge_base: false,
        workspace: Some(workspace),
    }
}

fn compare(base: &WorkspaceState, head: &WorkspaceState) -> DiffDocument {
    diff(
        &side(SideName::Base, base.clone()),
        &side(SideName::Head, head.clone()),
    )
}

/// The obligations of a valid workspace, by key.
fn obligations(workspace: &WorkspaceState) -> BTreeMap<ObligationKey, Obligation> {
    let model = workspace.assemble_model().expect("the workspace assembles");

    assert!(
        analyzer::validate(&model).is_empty(),
        "the workspace validates"
    );

    let verification = analyzer::verification::verify(&model);

    report::obligations(&model, &verification)
        .obligations
        .into_iter()
        .map(|obligation| (obligation.key.clone(), obligation))
        .collect()
}

fn entries(document: &DiffDocument) -> &[ObligationEntry] {
    match &document.obligations {
        ObligationDiff::Available { entries, .. } => entries,
        ObligationDiff::Unavailable { reason, .. } => panic!("unavailable: {reason}"),
    }
}

fn kind(mutation: &Mutation) -> &'static str {
    match mutation {
        Mutation::RemoveLock { .. } => "remove a lock step",
        Mutation::SetIsolation { .. } => "drop or weaken isolation",
        Mutation::CompareAndSetToUpdate { .. } => "replace a compare_and_set with an update",
        Mutation::AddDeleter { .. } => "add a transaction deleting a versioned object",
        Mutation::SetDelivery { .. } => "change a subscription's delivery",
        Mutation::RemoveDeduplicatedBy { .. } => "remove a deduplicated_by",
        Mutation::SetExternalIdempotency { .. } => "change an external effect's idempotency",
        Mutation::SetExternalResultReplay { .. } => "unspecify an external result replay",
        Mutation::UnspecifyRetry { .. } => "unspecify a request's retry",
        Mutation::UnspecifyMessageIdentity { .. } => "unspecify a message identity",
        Mutation::WidenSelector { .. } => "widen a selector",
        Mutation::RemoveRequirement { .. } => "remove a requirement",
    }
}

/// §19.2: every mutation of the catalogue, at every site of every
/// fixture, changes a declaration; every obligation whose basis held a
/// fact the mutation falsifies changes class; and no entry is
/// unexplained (§12.2).
#[test]
fn every_changed_verdict_in_the_corpus_is_explained() {
    let mut failures = Vec::new();
    let mut verified: BTreeMap<&str, usize> = BTreeMap::new();
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();

    for name in FIXTURES {
        let base = fixture(name);
        let base_obligations = obligations(&base);

        for mutation in mutation::catalogue(&base) {
            let Some(head) = mutation.apply(&base) else {
                failures.push(format!("{name}: {mutation}: does not apply"));
                continue;
            };

            let document = compare(&base, &head);

            if document.declarations.is_empty() {
                failures.push(format!("{name}: {mutation}: changed no declaration"));
            }

            let ObligationDiff::Available {
                entries,
                unexplained,
                ..
            } = &document.obligations
            else {
                *skipped.entry(kind(&mutation)).or_default() += 1;
                continue;
            };

            *verified.entry(kind(&mutation)).or_default() += 1;

            for entry in unexplained {
                failures.push(format!(
                    "{name}: {mutation}: unexplained {:?} {}\n    base: {}\n    head: {}",
                    entry.class,
                    entry.key,
                    serde_json::to_string(&entry.base).unwrap(),
                    serde_json::to_string(&entry.head).unwrap(),
                ));
            }

            // Every proof relying on a falsified fact changes class.
            for (key, obligation) in &base_obligations {
                let relies = obligation
                    .basis
                    .iter()
                    .flatten()
                    .any(|fact| mutation.falsifies(&fact.fact));

                if !relies {
                    continue;
                }

                match entries.iter().find(|entry| &entry.key == key) {
                    Some(entry) if entry.class != ObligationClass::Held => {}

                    Some(entry) => failures.push(format!(
                        "{name}: {mutation}: {key} relied on a falsified fact but is {:?}",
                        entry.class
                    )),

                    None => failures.push(format!(
                        "{name}: {mutation}: {key} relied on a falsified fact but is quiet"
                    )),
                }
            }

            // A removed requirement's obligations are removed.
            if let Mutation::RemoveRequirement { requirement } = &mutation {
                for key in base_obligations.keys() {
                    if key.requirement() == *requirement
                        && !entries.iter().any(|entry| {
                            &entry.key == key && entry.class == ObligationClass::Removed
                        })
                    {
                        failures.push(format!("{name}: {mutation}: {key} is not removed"));
                    }
                }
            }
        }
    }

    eprintln!("verified mutants by kind: {verified:#?}");
    eprintln!("mutants without an obligation diff: {skipped:#?}");

    for kind in [
        "remove a lock step",
        "drop or weaken isolation",
        "replace a compare_and_set with an update",
        "add a transaction deleting a versioned object",
        "change a subscription's delivery",
        "remove a deduplicated_by",
        "change an external effect's idempotency",
        "widen a selector",
        "remove a requirement",
    ] {
        if !verified.contains_key(kind) {
            failures.push(format!("no fixture verified a mutant of `{kind}`"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn id(text: &str) -> Id {
    Id(text.to_string())
}

fn entry<'a>(document: &'a DiffDocument, key: &ObligationKey) -> Option<&'a ObligationEntry> {
    entries(document).iter().find(|entry| &entry.key == key)
}

fn counts(document: &DiffDocument) -> ClassCounts {
    match &document.obligations {
        ObligationDiff::Available { counts, .. } => *counts,
        ObligationDiff::Unavailable { reason, .. } => panic!("unavailable: {reason}"),
    }
}

/// The key of the one obligation of `property` on `subject`'s
/// operation (and transaction), for a fixture's report.
fn key_of(
    obligations: &BTreeMap<ObligationKey, Obligation>,
    property: ObligationProperty,
    transaction: Option<&str>,
    operation: &str,
) -> ObligationKey {
    let matching: Vec<&ObligationKey> = obligations
        .keys()
        .filter(|key| key.property == property)
        .filter(|key| match (&key.subject, transaction) {
            (ObligationSubject::Operation { operation: subject }, None) => subject.0 == operation,

            (
                ObligationSubject::Transaction {
                    operation: subject,
                    transaction: declared,
                },
                Some(transaction),
            ) => subject.0 == operation && declared.0 == transaction,

            _ => false,
        })
        .collect();

    assert_eq!(
        matching.len(),
        1,
        "{property:?} on {operation}: {matching:?}"
    );

    matching[0].clone()
}

/// Reverses every requirement list of the workspace: an operation's and
/// each inline transaction's.
fn reverse_requirements(workspace: &mut WorkspaceState) {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                if let Some(Value::Object(requirements)) = fields.get_mut("requirements") {
                    for family in ["serializability", "ordering"] {
                        if let Some(Value::Array(list)) = requirements.get_mut(family) {
                            list.reverse();
                        }
                    }
                }

                fields.values_mut().for_each(walk);
            }

            Value::Array(items) => items.iter_mut().for_each(walk),

            _ => {}
        }
    }

    for draft in workspace.operations.values_mut() {
        draft.requirements.idempotency.reverse();
        draft.requirements.recoverability.reverse();

        if let Some(program) = &mut draft.program {
            let mut value = serde_json::to_value(&*program).unwrap();

            walk(&mut value);

            *program = serde_json::from_value(value).unwrap();
        }
    }
}

/// Flash checkout with two requirements in an operation's idempotency
/// list and in a transaction's serializability list, so that reversing
/// them renumbers obligations.
fn doubled_requirements() -> WorkspaceState {
    let mut workspace = fixture("flash_checkout.yaml");

    let captured = |field: &str| ValueRef {
        source: ValueSource::Input(id("input.apply_payment.captured")),
        path: FieldPath(vec![field.to_string()]),
    };

    let draft = workspace
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap();

    draft.requirements.idempotency.push(IdempotencyRequirement {
        key: IdempotencyKey {
            components: vec![captured("order_id")],
        },
        result: ResultReplayRequirement::Unspecified,
    });

    draft
        .program
        .as_mut()
        .and_then(|program| program.transaction_mut(&id("tx.apply_payment")))
        .unwrap()
        .requirements
        .serializability
        .push(TransactionSerializabilityRequirement {
            key: captured("event_id"),
        });

    workspace
}

/// §19.3: reversing every requirement list renames obligations by
/// position and changes nothing the diff reports.
#[test]
fn reordering_requirements_is_not_a_change() {
    let mut renumbered = 0;

    let workspaces = FIXTURES
        .iter()
        .map(|name| (name.to_string(), fixture(name)))
        .chain([("doubled".to_string(), doubled_requirements())]);

    for (name, base) in workspaces {
        let mut head = base.clone();

        reverse_requirements(&mut head);

        let document = compare(&base, &head);

        assert!(
            document.declarations.changes.is_empty(),
            "{name}: {document:#?}"
        );
        assert!(entries(&document).is_empty(), "{name}: {document:#?}");
        assert_eq!(document.reliance, Some(RelianceDelta::default()), "{name}");

        let counts = counts(&document);

        assert_eq!(counts.added + counts.removed, 0, "{name}");
        assert_eq!(counts.matched(), obligations(&base).len(), "{name}");

        let (base, head) = (obligations(&base), obligations(&head));

        renumbered += base
            .iter()
            .filter(|(key, obligation)| head[*key].id != obligation.id)
            .count();
    }

    assert!(renumbered > 0, "some positional id changed");
}

/// A shop with only `operation.place_order`, whose one transaction
/// reserves stock under an exclusive lock and a compare_and_set of the
/// version it read.
fn place_order_only() -> WorkspaceState {
    let mut shop = fixture("shop.yaml");

    shop.operations
        .retain(|operation, _| operation.0 == "operation.place_order");
    shop.runtime
        .routers
        .retain(|_, router| router.boundary.operation.0 == "operation.place_order");

    let lock = mutation::catalogue(&shop)
        .into_iter()
        .find(|mutation| matches!(mutation, Mutation::RemoveLock { .. }))
        .expect("the reserve transaction locks the product");

    lock.apply(&shop).expect("the lock is removed")
}

/// §19.4 and §2.2: serializable isolation replaced by a read-committed
/// compare_and_set of the version the read observed.
#[test]
fn a_rebased_proof_lists_what_it_withdrew_and_introduced() {
    let head = place_order_only();

    let operation = id("operation.place_order");
    let transaction = id("tx.place_order.reserve");

    let serializable = Mutation::SetIsolation {
        operation: operation.clone(),
        transaction: transaction.clone(),
        isolation: TransactionIsolation::Serializable,
    }
    .apply(&head)
    .unwrap();

    let base = Mutation::CompareAndSetToUpdate {
        operation: operation.clone(),
        transaction: transaction.clone(),
        object: id("object.product"),
    }
    .apply(&serializable)
    .unwrap();

    let document = compare(&base, &head);
    let base_obligations = obligations(&base);

    let key = key_of(
        &base_obligations,
        ObligationProperty::TransactionSerializability,
        Some("tx.place_order.reserve"),
        "operation.place_order",
    );

    let rebased = entry(&document, &key).expect("the obligation changed");

    assert_eq!(rebased.class, ObligationClass::Rebased, "{rebased:#?}");
    assert_eq!(rebased.restated, Some(false));

    let isolation = |level| Fact::Isolation {
        operation: operation.clone(),
        transaction: transaction.clone(),
        level,
    };

    let withdrawn: Vec<&Fact> = rebased.withdrawn.iter().map(|fact| &fact.fact).collect();
    let introduced: Vec<&Fact> = rebased.introduced.iter().map(|fact| &fact.fact).collect();

    assert!(
        withdrawn.contains(&&isolation(TransactionIsolation::Serializable)),
        "{withdrawn:#?}"
    );

    let guard = introduced
        .iter()
        .find_map(|fact| match fact {
            Fact::GuardedMutation {
                transaction: guarded,
                object,
                mechanism: Mechanism::CompareAndSet,
                coverage:
                    Coverage::ObservedVersion {
                        read,
                        version_field,
                    },
                ..
            } if *guarded == transaction && object.0 == "object.product" => {
                Some((read.0.as_str(), version_field.to_string()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no observed-version guard introduced: {introduced:#?}"));

    assert_eq!(guard, ("read.place_order.product", "version".to_string()));

    assert!(rebased.explained_by.contains(&DeclarationKey::Transaction {
        operation: operation.clone(),
        transaction: transaction.clone(),
    }));

    // The reliance delta says the same, model-wide.
    let reliance = document.reliance.as_ref().unwrap();

    assert!(
        reliance
            .withdrawn
            .iter()
            .any(|fact| fact.fact == isolation(TransactionIsolation::Serializable))
    );
    assert!(reliance.introduced.iter().any(|fact| matches!(
        &fact.fact,
        Fact::GuardedMutation {
            mechanism: Mechanism::CompareAndSet,
            ..
        }
    )));
}

/// §19.5: unspecifying a subscription's delivery regresses the
/// guaranteed recoverability resting on it, and withdraws the delivery
/// fact at L1.
#[test]
fn a_runtime_change_regresses_what_relied_on_it() {
    let base = fixture("flash_checkout.yaml");

    let (operation, input) = (
        id("operation.apply_payment"),
        id("input.apply_payment.captured"),
    );

    let head = Mutation::SetDelivery {
        operation: operation.clone(),
        input: input.clone(),
        delivery: DeliverySemantics::Unspecified,
    }
    .apply(&base)
    .unwrap();

    let document = compare(&base, &head);

    let key = key_of(
        &obligations(&base),
        ObligationProperty::Recoverability,
        None,
        "operation.apply_payment",
    );

    let regressed = entry(&document, &key).expect("the recoverability changed");

    assert_eq!(regressed.class, ObligationClass::Regressed);
    assert_eq!(regressed.base_status, Some(Status::Proven));
    assert_eq!(regressed.head_status, Some(Status::Unknown));
    assert_eq!(
        regressed.explained_by,
        vec![DeclarationKey::SubscriptionRuntime {
            operation: operation.clone(),
            input: input.clone(),
        }]
    );

    let reliance = document.reliance.as_ref().unwrap();

    assert_eq!(
        reliance.withdrawn,
        vec![WithdrawnReliance {
            fact: Fact::Delivery {
                operation,
                input,
                semantics: DeliverySemantics::AtLeastOnce,
            },
            layer: Layer::L1,
        }]
    );
    assert!(reliance.introduced.is_empty());
}

/// §19.6: a sketch, a stage, and a description are authoring alone.
#[test]
fn authoring_changes_are_not_semantic() {
    let base = fixture("flash_checkout.yaml");
    let mut head = base.clone();

    let draft = head
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap();

    draft.description = Some("Applies a captured payment.".to_string());
    draft.sketch = Some(
        serde_json::from_value(serde_json::json!({ "steps": [], "returns": ["order_id"] }))
            .unwrap(),
    );
    draft.stage = OperationDraftStage::Planned;

    let document = compare(&base, &head);

    assert!(document.declarations.changes.is_empty(), "{document:#?}");
    assert_eq!(document.declarations.authoring.len(), 3, "{document:#?}");
    assert!(entries(&document).is_empty(), "{document:#?}");
    assert_eq!(document.reliance, Some(RelianceDelta::default()));
}

/// §19.7: a deleter of a versioned object falsifies the absence the
/// observed-version guard rested on, and explains every proof that
/// rested on it.
#[test]
fn an_added_deleter_explains_what_rested_on_no_deleters() {
    let base = fixture("flash_checkout.yaml");

    let deleter = mutation::catalogue(&base)
        .into_iter()
        .find(|mutation| {
            matches!(mutation, Mutation::AddDeleter { selector, .. } if selector.object.0 == "object.order")
        })
        .expect("a transaction selects the order");

    let head = deleter.apply(&base).unwrap();
    let document = compare(&base, &head);

    let added: Vec<&DeclarationKey> = document
        .declarations
        .changes
        .iter()
        .filter(|change| change.change == Change::Added)
        .map(|change| &change.declaration)
        .collect();

    let [DeclarationKey::Transaction { .. }] = added.as_slice() else {
        panic!("one transaction is added: {added:#?}");
    };

    let no_deleters = Fact::NoDeleters {
        data_model: id("data.checkout"),
        object: id("object.order"),
    };

    let resting: Vec<ObligationKey> = obligations(&base)
        .into_iter()
        .filter(|(_, obligation)| {
            obligation
                .basis
                .iter()
                .flatten()
                .any(|fact| fact.fact == no_deleters)
        })
        .map(|(key, _)| key)
        .collect();

    assert!(
        !resting.is_empty(),
        "a proof rests on no_deleters(object.order)"
    );

    for key in resting {
        let changed = entry(&document, &key).expect("the obligation changed");

        assert_ne!(changed.class, ObligationClass::Held, "{changed:#?}");
        assert!(
            changed.explained_by.contains(added[0]),
            "explained by the added transaction: {changed:#?}"
        );
    }
}

/// §19.8: a renamed transaction's obligations are removed and added,
/// and nothing pairs them.
#[test]
fn a_renamed_transaction_takes_its_obligations_with_it() {
    let base = fixture("flash_checkout.yaml");
    let mut head = base.clone();

    let operation = id("operation.apply_payment");
    let (old, new) = (id("tx.apply_payment"), id("tx.apply_payment.v2"));

    head.operations
        .get_mut(&operation)
        .and_then(|draft| draft.program.as_mut())
        .and_then(|program| program.transaction_mut(&old))
        .unwrap()
        .id = new.clone();

    let document = compare(&base, &head);
    let counts = counts(&document);

    assert_eq!((counts.removed, counts.added), (2, 2), "{counts:?}");

    for entry in entries(&document) {
        if let ObligationSubject::Transaction { transaction, .. } = &entry.key.subject {
            let expected = if *transaction == old {
                ObligationClass::Removed
            } else {
                ObligationClass::Added
            };

            assert_eq!(entry.class, expected, "{entry:#?}");
        }
    }

    let ObligationDiff::Available { removed, .. } = &document.obligations else {
        unreachable!()
    };

    assert_eq!(removed.len(), 1, "{removed:#?}");
    assert_eq!(
        removed[0].declaration,
        DeclarationKey::Transaction {
            operation,
            transaction: old,
        }
    );
    assert_eq!(removed[0].obligations.len(), 2);
}

/// §19.9: a changed completion restates the recoverability obligation;
/// a result turned replay-consistent adds the result replay obligation
/// and leaves the idempotency one matched.
#[test]
fn restated_terms_and_result_replay() {
    let base = fixture("flash_checkout.yaml");

    let (operation, draft) = base
        .operations
        .iter()
        .find(|(_, draft)| {
            draft
                .requirements
                .recoverability
                .iter()
                .any(|requirement| requirement.completion == CompletionRequirement::Resumable)
        })
        .expect("a resumable recoverability requirement");

    let index = draft
        .requirements
        .recoverability
        .iter()
        .position(|requirement| requirement.completion == CompletionRequirement::Resumable)
        .unwrap();

    let mut head = base.clone();

    head.operations
        .get_mut(operation)
        .unwrap()
        .requirements
        .recoverability[index]
        .completion = CompletionRequirement::Guaranteed;

    let document = compare(&base, &head);

    let key = key_of(
        &obligations(&base),
        ObligationProperty::Recoverability,
        None,
        &operation.0,
    );

    let restated = entry(&document, &key).expect("the restated obligation is listed");

    assert_eq!(restated.restated, Some(true), "{restated:#?}");
    assert_eq!(counts(&document).added + counts(&document).removed, 0);

    // An idempotency requirement whose result is unspecified.
    let (operation, draft) = base
        .operations
        .iter()
        .find(|(_, draft)| {
            draft
                .requirements
                .idempotency
                .iter()
                .any(|requirement| requirement.result == ResultReplayRequirement::Unspecified)
        })
        .expect("an idempotency requirement without a result requirement");

    let index = draft
        .requirements
        .idempotency
        .iter()
        .position(|requirement| requirement.result == ResultReplayRequirement::Unspecified)
        .unwrap();

    let mut head = base.clone();

    head.operations
        .get_mut(operation)
        .unwrap()
        .requirements
        .idempotency[index]
        .result = ResultReplayRequirement::ReplayConsistent;

    let document = compare(&base, &head);
    let idempotency = key_of(
        &obligations(&base),
        ObligationProperty::Idempotency,
        None,
        &operation.0,
    );

    // Matched, and not restated: its statement does not depend on the
    // result.
    if let Some(matched) = entry(&document, &idempotency) {
        assert_ne!(matched.class, ObligationClass::Added);
        assert_ne!(matched.class, ObligationClass::Removed);
        assert_eq!(matched.restated, Some(false));
    }

    let result_replay = ObligationKey {
        property: ObligationProperty::ResultReplay,
        ..idempotency.clone()
    };

    let added = entry(&document, &result_replay).expect("the result replay obligation");

    assert_eq!(added.class, ObligationClass::Added);
    assert_eq!(
        added.explained_by,
        vec![DeclarationKey::Requirement(idempotency.requirement())],
        "explained by the modified requirement"
    );
}

/// §19.12: the same two trees give the same bytes.
#[test]
fn the_document_is_deterministic() {
    for name in ["flash_checkout.yaml", "shop.yaml", "video_streaming.yaml"] {
        let base = fixture(name);

        for mutation in mutation::catalogue(&base).into_iter().take(12) {
            let head = mutation.apply(&base).unwrap();

            let first = serde_json::to_string_pretty(&compare(&base, &head)).unwrap();
            let second = serde_json::to_string_pretty(&compare(&base, &head)).unwrap();

            assert_eq!(first, second, "{name}: {mutation}");

            // The document reads back as itself.
            let read: DiffDocument = serde_json::from_str(&first).unwrap();

            assert_eq!(serde_json::to_string_pretty(&read).unwrap(), first);
        }
    }
}

/// A side that does not assemble leaves the obligation diff
/// unavailable, naming the side, and every gate fails (§11.1, §15.3).
#[test]
fn an_unassemblable_side_makes_the_obligation_diff_unavailable() {
    let base = fixture("flash_checkout.yaml");
    let mut head = base.clone();

    head.operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap()
        .program = None;

    let document = compare(&base, &head);

    let ObligationDiff::Unavailable {
        side,
        reason,
        other_side,
    } = &document.obligations
    else {
        panic!("available: {document:#?}");
    };

    assert_eq!(*side, SideName::Head);
    assert!(reason.contains("operation.apply_payment"), "{reason}");
    assert_eq!(
        other_side.map(|counts| counts.proven + counts.unknown + counts.disproven),
        Some(obligations(&base).len())
    );
    assert!(document.impact.is_none() && document.reliance.is_none());
    assert!(!document.declarations.changes.is_empty());

    let all = [
        Gate::Regressed,
        Gate::Removed,
        Gate::AddedUnproven,
        Gate::Rebased,
        Gate::Unexplained,
    ];

    assert_eq!(document.failed_gates(&all), all.to_vec());
    assert!(document.failed_gates(&[]).is_empty());

    // Both sides failing names the base, with no other side to count.
    let document = compare(&head, &head);

    let ObligationDiff::Unavailable {
        side, other_side, ..
    } = &document.obligations
    else {
        panic!("available: {document:#?}");
    };

    assert_eq!((*side, *other_side), (SideName::Base, None));
}

/// §6.4: an empty base adds every declaration and every obligation.
#[test]
fn an_empty_base_adds_everything() {
    let head = fixture("flash_checkout.yaml");

    let empty = Side {
        name: SideName::Base,
        source: Source::Commit("HEAD".to_string()),
        commit: Some("0".repeat(40)),
        merge_base: false,
        workspace: None,
    };

    let document = diff(&empty, &side(SideName::Head, head.clone()));

    assert!(document.base.empty);
    assert!(
        document
            .declarations
            .changes
            .iter()
            .all(|change| change.change == Change::Added)
    );

    let counts = counts(&document);

    assert_eq!(counts.added, obligations(&head).len());
    assert_eq!(counts.added, entries(&document).len());
}

/// §15.3: each gate fails exactly when its class occurs.
#[test]
fn each_gate_fails_exactly_on_its_class() {
    let base = fixture("flash_checkout.yaml");

    let regressed = Mutation::SetDelivery {
        operation: id("operation.apply_payment"),
        input: id("input.apply_payment.captured"),
        delivery: DeliverySemantics::Unspecified,
    }
    .apply(&base)
    .unwrap();

    let document = compare(&base, &regressed);

    assert_eq!(
        document.failed_gates(&[
            Gate::Regressed,
            Gate::Removed,
            Gate::AddedUnproven,
            Gate::Rebased,
            Gate::Unexplained,
        ]),
        vec![Gate::Regressed]
    );

    // Removing a requirement removes its obligations; adding it back is
    // an added obligation, unproven here when the base has it unproven.
    let requirement = mutation::catalogue(&base)
        .into_iter()
        .find_map(|mutation| match mutation {
            Mutation::RemoveRequirement { requirement } => Some(requirement),
            _ => None,
        })
        .unwrap();

    let removed = Mutation::RemoveRequirement {
        requirement: requirement.clone(),
    }
    .apply(&base)
    .unwrap();

    let document = compare(&base, &removed);

    assert!(document.failed_gates(&[Gate::Removed]) == vec![Gate::Removed]);
    assert!(document.failed_gates(&[Gate::AddedUnproven]).is_empty());

    let document = compare(&removed, &base);
    let added_unproven = entries(&document).iter().any(|entry| {
        entry.class == ObligationClass::Added && entry.head_status != Some(Status::Proven)
    });

    assert_eq!(
        document.failed_gates(&[Gate::AddedUnproven]).is_empty(),
        !added_unproven
    );
    assert!(document.failed_gates(&[Gate::Removed]).is_empty());

    // Rebased.
    let rebased = Mutation::SetIsolation {
        operation: id("operation.apply_payment"),
        transaction: id("tx.apply_payment"),
        isolation: TransactionIsolation::Serializable,
    }
    .apply(&base)
    .unwrap();

    let document = compare(&base, &rebased);

    assert_eq!(document.failed_gates(&[Gate::Rebased]), vec![Gate::Rebased]);
    assert!(document.failed_gates(&[Gate::Regressed]).is_empty());

    // Unexplained: no real diff has one, so plant one.
    let mut document = document;

    if let ObligationDiff::Available {
        unexplained,
        entries,
        ..
    } = &mut document.obligations
    {
        assert!(unexplained.is_empty());

        unexplained.push(UnexplainedEntry {
            key: entries[0].key.clone(),
            class: entries[0].class,
            base: Grounds {
                basis: None,
                blocked_on: None,
            },
            head: Grounds {
                basis: None,
                blocked_on: None,
            },
        });
    }

    assert_eq!(
        document.failed_gates(&[Gate::Unexplained]),
        vec![Gate::Unexplained]
    );
}
