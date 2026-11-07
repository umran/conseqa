//! Obligation keys, bases, and the durable form of facts (§9, §10, and
//! §17 of the spec change analysis revision; its tests §19.3, §19.13,
//! and §19.14).

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use conseqa::{
    analyzer::{
        basis::{
            self, ArtifactRoute, BasisFact, CanonicalError, Channel, Coverage, DeclarationKey,
            EntryRef, FACT_FORMAT, Fact, FactVersionError, Guarantee, Layer, Mechanism,
            ObligationKey, ObligationProperty, ObligationSubject, RequirementFamily,
            RequirementIdentity, RootRule, Site, TextualKey, TextualKeyError, TransactionId,
        },
        report::{self, ProverReport, Status},
        validation,
        verification::{self, ProofScope},
    },
    parser::yaml,
    spec::{
        CompletionRequirement, CursorAdvanceRule, DeliverySemantics, ExternalIdentity,
        ExternalIdentityKey, FieldPath, Id, IdempotencyKey, IdempotencyRequirement, LockMode,
        Model, ObjectSelector, RecoverabilityRequirement, ResultReplayRequirement,
        SelectorPredicate, SelectorValue, TransactionIsolation, TransactionOrderingRequirement,
        TransactionSerializabilityRequirement, ValueRef, ValueSource,
    },
};
use serde_json::{Value, json};

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every fixture that parses and validates: the models a report is
/// built for.
fn fixtures() -> Vec<(String, Model)> {
    let mut models = Vec::new();

    let mut paths: Vec<PathBuf> = fs::read_dir(manifest().join("tests").join("fixtures"))
        .expect("the fixtures directory reads")
        .map(|entry| entry.expect("a fixture entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("yaml"))
        .collect();

    paths.sort();

    for path in paths {
        let source = fs::read_to_string(&path).expect("a fixture reads");

        let Ok(model) = yaml::parse(&source) else {
            continue;
        };

        if validation::validate(&model).is_empty() {
            models.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                model,
            ));
        }
    }

    assert!(models.len() >= 8, "the valid fixtures load");

    models
}

fn fixture(name: &str) -> Model {
    fixtures()
        .into_iter()
        .find(|(file, _)| file == name)
        .map(|(_, model)| model)
        .unwrap_or_else(|| panic!("`{name}` is a valid fixture"))
}

fn report_of(model: &Model) -> ProverReport {
    report::obligations(model, &verification::verify(model))
}

fn id(text: &str) -> Id {
    Id(text.to_string())
}

fn path(text: &str) -> FieldPath {
    FieldPath(text.split('.').map(str::to_string).collect())
}

fn input_ref(input: &str, field: &str) -> ValueRef {
    ValueRef {
        source: ValueSource::Input(id(input)),
        path: path(field),
    }
}

/// Every fact of an obligation's grounds: its basis, or the present
/// and missing facts of its blocked-on set.
fn grounds_facts(report: &ProverReport) -> Vec<&BasisFact> {
    report
        .obligations
        .iter()
        .flat_map(|obligation| {
            obligation.basis.iter().flatten().chain(
                obligation
                    .blocked_on
                    .iter()
                    .flat_map(|blocked| blocked.present.iter().chain(&blocked.missing)),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------
// §19.13: report format 10
// ---------------------------------------------------------------------

#[test]
fn every_proven_obligation_rests_on_a_basis_and_every_unproven_one_is_blocked() {
    for (name, model) in fixtures() {
        let report = report_of(&model);

        assert_eq!(report.format, 10, "{name}");
        assert_eq!(report.fact_format, FACT_FORMAT, "{name}");

        for obligation in &report.obligations {
            match obligation.status {
                Status::Proven => {
                    let basis = obligation
                        .basis
                        .as_ref()
                        .unwrap_or_else(|| panic!("{name}: `{}` has no basis", obligation.id));

                    assert!(
                        !basis.is_empty(),
                        "{name}: `{}` has an empty basis",
                        obligation.id
                    );
                    assert!(obligation.blocked_on.is_none(), "{name}: {}", obligation.id);

                    // The scope a verifier recorded agrees with the
                    // basis: an L1 fact makes a proof runtime-dependent,
                    // and a runtime-dependent proof consumed one, itself
                    // or through an obligation it relies on.
                    let l1 = basis.iter().any(|fact| fact.layer == Layer::L1);
                    let relies = basis
                        .iter()
                        .any(|fact| matches!(fact.fact, Fact::ReliesOn { .. }));

                    match obligation.scope {
                        Some(ProofScope::L0Only) => {
                            assert!(!l1, "{name}: l0_only `{}` cites an L1 fact", obligation.id)
                        }

                        Some(ProofScope::RuntimeDependent) => assert!(
                            l1 || relies,
                            "{name}: runtime_dependent `{}` cites no L1 fact",
                            obligation.id
                        ),

                        None => panic!("{name}: proven `{}` has no scope", obligation.id),
                    }
                }

                Status::Unknown | Status::Disproven => {
                    let blocked = obligation.blocked_on.as_ref().unwrap_or_else(|| {
                        panic!("{name}: `{}` has no blocked-on set", obligation.id)
                    });

                    assert!(
                        !blocked.is_empty(),
                        "{name}: `{}` has an empty blocked-on set",
                        obligation.id
                    );
                    assert!(obligation.basis.is_none(), "{name}: {}", obligation.id);
                }
            }

            // Every fact states its layer, and states it correctly.
            for fact in obligation.basis.iter().flatten() {
                assert_eq!(fact.layer, fact.fact.layer(), "{name}: {}", obligation.id);
                assert!(
                    !fact.anchors.is_empty(),
                    "{name}: an unanchored fact in {}",
                    obligation.id
                );
            }
        }
    }
}

#[test]
fn keys_and_reports_round_trip_through_their_canonical_form() {
    for (name, model) in fixtures() {
        let report = report_of(&model);

        let keys: BTreeSet<&ObligationKey> = report
            .obligations
            .iter()
            .map(|obligation| &obligation.key)
            .collect();

        assert_eq!(
            keys.len(),
            report.obligations.len(),
            "{name}: keys are distinct"
        );

        for obligation in &report.obligations {
            let written = serde_json::to_value(&obligation.key).expect("a key serializes");
            let read: ObligationKey = basis::read_canonical(&written).expect("a key reads back");

            assert_eq!(read, obligation.key, "{name}: {}", obligation.id);
        }

        let written = serde_json::to_value(&report).expect("the report serializes");

        basis::check_versions(&written).expect("the report's versions are this build's");

        let read: ProverReport = basis::read_canonical(&written).expect("the report reads back");

        assert_eq!(read, report, "{name}");
    }
}

#[test]
fn reports_are_deterministic() {
    for (name, model) in fixtures() {
        let first = serde_json::to_string(&report_of(&model)).unwrap();
        let second = serde_json::to_string(&report_of(&model)).unwrap();

        assert_eq!(first, second, "{name}");
    }
}

/// The viz refuses a report of any other format, so its mirror of the
/// format moves with the report's.
#[test]
fn the_viz_reads_this_report_format() {
    let types = fs::read_to_string(manifest().join("viz/src/types/report.ts"))
        .expect("the viz report types read");

    let declared = types
        .lines()
        .find_map(|line| line.strip_prefix("export const REPORT_FORMAT = "))
        .and_then(|rest| rest.strip_suffix(';'))
        .expect("the viz declares REPORT_FORMAT");

    assert_eq!(declared, report::FORMAT.to_string());
}

// ---------------------------------------------------------------------
// §19.3: reordering is not a change
// ---------------------------------------------------------------------

/// Flash checkout's payment operation, with a second requirement in
/// each of its four lists, so reversing a list moves a requirement.
fn with_second_requirements(model: &Model) -> Model {
    let mut model = model.clone();

    let operation = model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .expect("flash checkout declares apply_payment");

    operation
        .requirements
        .idempotency
        .push(IdempotencyRequirement {
            key: IdempotencyKey {
                components: vec![input_ref("input.apply_payment.captured", "order_id")],
            },
            result: ResultReplayRequirement::Unspecified,
        });

    operation
        .requirements
        .recoverability
        .push(RecoverabilityRequirement {
            key: IdempotencyKey {
                components: vec![input_ref("input.apply_payment.captured", "order_id")],
            },
            completion: CompletionRequirement::Resumable,
        });

    let transaction = operation
        .program
        .transaction_mut(&id("tx.apply_payment"))
        .expect("apply_payment declares tx.apply_payment");

    transaction
        .requirements
        .serializability
        .push(TransactionSerializabilityRequirement {
            key: input_ref("input.apply_payment.captured", "event_id"),
        });

    transaction
        .requirements
        .ordering
        .push(TransactionOrderingRequirement {
            key: input_ref("input.apply_payment.captured", "event_id"),
            position: input_ref("input.apply_payment.captured", "sequence"),
        });

    model
}

fn reversed(model: &Model) -> Model {
    let mut model = model.clone();

    let operation = model
        .operations
        .get_mut(&id("operation.apply_payment"))
        .unwrap();

    operation.requirements.idempotency.reverse();
    operation.requirements.recoverability.reverse();

    let transaction = operation
        .program
        .transaction_mut(&id("tx.apply_payment"))
        .unwrap();

    transaction.requirements.serializability.reverse();
    transaction.requirements.ordering.reverse();

    model
}

#[test]
fn reordering_requirements_changes_no_key_status_or_grounds() {
    let model = with_second_requirements(&fixture("flash_checkout.yaml"));
    let reordered = reversed(&model);

    for model in [&model, &reordered] {
        assert!(
            validation::validate(model).is_empty(),
            "the variant validates"
        );
    }

    let grounds = |report: &ProverReport| -> BTreeMap<ObligationKey, (String, Value)> {
        report
            .obligations
            .iter()
            .map(|obligation| {
                (
                    obligation.key.clone(),
                    (
                        obligation.id.clone(),
                        json!({
                            "status": obligation.status,
                            "scope": obligation.scope,
                            "basis": obligation.basis,
                            "blocked_on": obligation.blocked_on,
                        }),
                    ),
                )
            })
            .collect()
    };

    let before = grounds(&report_of(&model));
    let after = grounds(&report_of(&reordered));

    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "the same obligations, by key"
    );

    let mut moved = 0;

    for (key, (id_before, before)) in &before {
        let (id_after, after) = &after[key];

        assert_eq!(before, after, "{key}");

        if id_before != id_after {
            moved += 1;
        }
    }

    // Two requirements in each of four lists: every one of the eight,
    // and the result replay obligation riding on one, changed address.
    assert!(moved >= 8, "the positional ids moved ({moved} did)");
}

// ---------------------------------------------------------------------
// §19.14: durable facts
// ---------------------------------------------------------------------

#[test]
fn every_extracted_fact_round_trips_through_its_canonical_form() {
    let mut kinds = BTreeSet::new();

    for (name, model) in fixtures() {
        let report = report_of(&model);

        for fact in grounds_facts(&report) {
            let written = serde_json::to_value(&fact.fact).expect("a fact serializes");
            let read: Fact = basis::read_canonical(&written)
                .unwrap_or_else(|error| panic!("{name}: {error}: {written}"));

            assert_eq!(&read, &fact.fact, "{name}");

            // The canonical text spells the same value.
            let text: Value = serde_json::from_str(&fact.fact.canonical()).unwrap();

            assert_eq!(text, written, "{name}");

            let written = serde_json::to_value(fact).expect("a basis fact serializes");
            let read: BasisFact = basis::read_canonical(&written)
                .unwrap_or_else(|error| panic!("{name}: {error}: {written}"));

            assert_eq!(&read, fact, "{name}");

            kinds.insert(kind(&fact.fact));
        }
    }

    // The fixtures exercise the common routes; the goldens cover the
    // rest of the vocabulary.
    for expected in [
        "conflict_closure",
        "cyclic_components",
        "isolation",
        "guarded_mutation",
        "cursor",
        "no_deleters",
        "deduplicated_by",
        "stable_root",
        "artifact_route",
        "delivery",
        "message_identity",
        "relies_on",
    ] {
        assert!(
            kinds.contains(expected),
            "no fixture fact of kind `{expected}`"
        );
    }
}

/// Every fact kind, by the name its durable form carries. Matched
/// exhaustively: a kind added to the vocabulary fails to compile here
/// until it is listed in `KINDS` and sampled in `samples`.
fn kind(fact: &Fact) -> &'static str {
    match fact {
        Fact::Isolation { .. } => "isolation",
        Fact::Lock { .. } => "lock",
        Fact::GuardedMutation { .. } => "guarded_mutation",
        Fact::Cursor { .. } => "cursor",
        Fact::Fence { .. } => "fence",
        Fact::ConflictClosure { .. } => "conflict_closure",
        Fact::CyclicComponents { .. } => "cyclic_components",
        Fact::NoDeleters { .. } => "no_deleters",
        Fact::KeyedRequirement { .. } => "keyed_requirement",
        Fact::AdmitsNoMessages { .. } => "admits_no_messages",
        Fact::NoAdmittedPaths { .. } => "no_admitted_paths",
        Fact::ReturnsNoResult { .. } => "returns_no_result",
        Fact::Terminal { .. } => "terminal",
        Fact::StableRoot { .. } => "stable_root",
        Fact::Delivery { .. } => "delivery",
        Fact::MessageIdentity { .. } => "message_identity",
        Fact::DeduplicatedBy { .. } => "deduplicated_by",
        Fact::NaturallyReplayable { .. } => "naturally_replayable",
        Fact::ArtifactRoute { .. } => "artifact_route",
        Fact::OutboxRedrive { .. } => "outbox_redrive",
        Fact::RetryMayRepeat { .. } => "retry_may_repeat",
        Fact::ExternalGuarantee { .. } => "external_guarantee",
        Fact::Propagation { .. } => "propagation",
        Fact::ReliesOn { .. } => "relies_on",
        Fact::Realization { .. } => "realization",
    }
}

const KINDS: [&str; 25] = [
    "isolation",
    "lock",
    "guarded_mutation",
    "cursor",
    "fence",
    "conflict_closure",
    "cyclic_components",
    "no_deleters",
    "keyed_requirement",
    "admits_no_messages",
    "no_admitted_paths",
    "returns_no_result",
    "terminal",
    "stable_root",
    "delivery",
    "message_identity",
    "deduplicated_by",
    "naturally_replayable",
    "artifact_route",
    "outbox_redrive",
    "retry_may_repeat",
    "external_guarantee",
    "propagation",
    "relies_on",
    "realization",
];

fn transaction_id(operation: &str, transaction: &str) -> TransactionId {
    TransactionId {
        operation: id(operation),
        transaction: id(transaction),
    }
}

fn order_selector() -> ObjectSelector {
    ObjectSelector {
        object: id("object.order"),
        predicate: SelectorPredicate::Eq {
            field: path("order_id"),
            value: SelectorValue::Value(input_ref("input.apply_payment.request", "order_id")),
        },
    }
}

fn payment_key() -> ObligationKey {
    ObligationKey {
        subject: ObligationSubject::Transaction {
            operation: id("operation.apply_payment"),
            transaction: id("tx.apply_payment"),
        },
        property: ObligationProperty::TransactionSerializability,
        requirement_key: vec![input_ref("input.apply_payment.request", "order_id")],
        occurrence: 0,
    }
}

/// Samples of every fact kind, covering every variant of the
/// vocabulary's nested enums, each in the spelling its golden file
/// records.
fn samples() -> Vec<Fact> {
    let operation = || id("operation.apply_payment");
    let transaction = || id("tx.apply_payment");
    let key = || input_ref("input.apply_payment.request", "order_id");

    let guarded = |mechanism: Mechanism, coverage: Coverage| Fact::GuardedMutation {
        operation: operation(),
        transaction: transaction(),
        data_model: id("data.orders"),
        object: id("object.order"),
        mechanism,
        coverage,
        compared_fields: BTreeSet::from([path("version")]),
    };

    let mut samples = vec![
        Fact::Isolation {
            operation: operation(),
            transaction: transaction(),
            level: TransactionIsolation::ReadCommitted,
        },
        Fact::Isolation {
            operation: operation(),
            transaction: transaction(),
            level: TransactionIsolation::Unspecified,
        },
        Fact::Lock {
            operation: operation(),
            transaction: transaction(),
            data_model: id("data.orders"),
            object: id("object.order"),
            selector: order_selector(),
            mode: LockMode::Exclusive,
        },
        guarded(Mechanism::CompareAndSet, Coverage::Atomic),
        guarded(Mechanism::Transition, Coverage::HeldProtection),
        guarded(
            Mechanism::AdvanceCursor,
            Coverage::ObservedState {
                read: id("read.apply_payment.order"),
            },
        ),
        guarded(
            Mechanism::CompareAndSet,
            Coverage::ObservedVersion {
                read: id("read.apply_payment.order"),
                version_field: path("version"),
            },
        ),
        guarded(
            Mechanism::Upsert,
            Coverage::LockedReader {
                selector: order_selector(),
                mode: LockMode::Shared,
            },
        ),
        guarded(Mechanism::Fence, Coverage::Atomic),
        Fact::Cursor {
            data_model: id("data.orders"),
            object: id("object.order"),
            field: path("last_applied_sequence"),
            rule: CursorAdvanceRule::Successor,
        },
        Fact::Cursor {
            data_model: id("data.orders"),
            object: id("object.order"),
            field: path("last_applied_sequence"),
            rule: CursorAdvanceRule::MonotonicAfter,
        },
        Fact::Fence {
            data_model: id("data.orders"),
            object: id("object.order"),
            field: path("fence_token"),
        },
        Fact::ConflictClosure {
            root: transaction_id("operation.apply_payment", "tx.apply_payment"),
            key: key(),
            members: BTreeSet::from([
                transaction_id("operation.apply_payment", "tx.apply_payment"),
                transaction_id("operation.cancel_order", "tx.cancel_order"),
            ]),
        },
        Fact::CyclicComponents {
            root: transaction_id("operation.apply_payment", "tx.apply_payment"),
            key: key(),
            components: BTreeSet::from([BTreeSet::from([
                transaction_id("operation.apply_payment", "tx.apply_payment"),
                transaction_id("operation.cancel_order", "tx.cancel_order"),
            ])]),
        },
        Fact::NoDeleters {
            data_model: id("data.orders"),
            object: id("object.order"),
        },
        Fact::KeyedRequirement {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
            property: ObligationProperty::Idempotency,
        },
        Fact::KeyedRequirement {
            operation: id("operation.quote"),
            input: id("input.quote.request"),
            property: ObligationProperty::ResultReplay,
        },
        Fact::AdmitsNoMessages {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
        },
        Fact::NoAdmittedPaths {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
        },
        Fact::ReturnsNoResult {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
        },
        Fact::Terminal {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
        },
        Fact::Delivery {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
            semantics: DeliverySemantics::AtLeastOnce,
        },
        Fact::Delivery {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
            semantics: DeliverySemantics::AtMostOnce,
        },
        Fact::MessageIdentity {
            channel: Channel::Topic {
                topic: id("topic.payments"),
            },
            schema: id("schema.PaymentCaptured"),
            fields: vec![path("event_id")],
        },
        Fact::MessageIdentity {
            channel: Channel::Outbox {
                data_model: id("data.orders"),
                outbox: id("outbox.order_events"),
            },
            schema: id("schema.OrderPaid"),
            fields: vec![path("order_id"), path("sequence")],
        },
        Fact::DeduplicatedBy {
            operation: operation(),
            transaction: transaction(),
            key: vec![input_ref("input.apply_payment.request", "event_id")],
        },
        Fact::NaturallyReplayable {
            operation: operation(),
            transaction: transaction(),
        },
        Fact::ArtifactRoute {
            operation: operation(),
            transaction: transaction(),
            artifact: id("output.apply_payment"),
            route: ArtifactRoute::Recovered,
        },
        Fact::ArtifactRoute {
            operation: operation(),
            transaction: transaction(),
            artifact: id("intent.apply_payment.notify"),
            route: ArtifactRoute::Reconstructed,
        },
        Fact::OutboxRedrive {
            operation: id("operation.relay"),
            input: id("input.relay.outbox"),
            data_model: id("data.orders"),
            outbox: id("outbox.order_events"),
        },
        Fact::RetryMayRepeat {
            site: Site::EffectSite {
                operation: id("operation.checkout"),
                effect: id("effect.checkout.pay"),
            },
        },
        Fact::RetryMayRepeat {
            site: Site::TransitionEffect {
                machine: id("machine.order"),
                transition: id("transition.order.pay"),
                effect: id("effect.order.charge"),
            },
        },
        Fact::ExternalGuarantee {
            operation: id("operation.charge"),
            effect: id("effect.charge.card"),
            guarantee: Guarantee::Idempotent,
            identity: ExternalIdentity::Keyed {
                key: ExternalIdentityKey {
                    components: vec![input_ref("input.charge.request", "payment_id")],
                },
            },
        },
        Fact::ExternalGuarantee {
            operation: id("operation.charge"),
            effect: id("effect.charge.quote"),
            guarantee: Guarantee::SideEffectFree,
            identity: ExternalIdentity::Unspecified,
        },
        Fact::ExternalGuarantee {
            operation: id("operation.charge"),
            effect: id("effect.charge.card"),
            guarantee: Guarantee::ReplayStable,
            identity: ExternalIdentity::Keyed {
                key: ExternalIdentityKey {
                    components: vec![input_ref("input.charge.request", "payment_id")],
                },
            },
        },
        Fact::Propagation {
            producer: Site::EffectSite {
                operation: id("operation.charge"),
                effect: id("effect.charge.publish_captured"),
            },
            schema: id("schema.PaymentCaptured"),
            channel: Channel::Topic {
                topic: id("topic.payments"),
            },
        },
        Fact::Propagation {
            producer: Site::TransitionEffect {
                machine: id("machine.order"),
                transition: id("transition.order.pay"),
                effect: id("effect.order.paid"),
            },
            schema: id("schema.OrderPaid"),
            channel: Channel::Outbox {
                data_model: id("data.orders"),
                outbox: id("outbox.order_events"),
            },
        },
        Fact::ReliesOn {
            obligation: payment_key(),
        },
        Fact::ReliesOn {
            obligation: ObligationKey {
                subject: ObligationSubject::Operation {
                    operation: id("operation.reserve"),
                },
                property: ObligationProperty::ResultReplay,
                requirement_key: vec![input_ref("input.reserve.request", "idempotency_key")],
                occurrence: 1,
            },
        },
        Fact::Realization {
            declaration: TextualKey::new(DeclarationKey::Transaction {
                operation: operation(),
                transaction: transaction(),
            })
            .unwrap(),
            entry: EntryRef {
                id: "postgres.database".to_string(),
                version: 1,
            },
            realizes: TextualKey::new(DeclarationKey::DataModel(id("data.orders"))).unwrap(),
            requirement: "conditional-update".to_string(),
        },
    ];

    for rule in [
        RootRule::KeyComponent,
        RootRule::IdentifiedPayload,
        RootRule::RecoveredArtifact,
        RootRule::ReconstructedArtifact,
        RootRule::ReplayStableExternalResult,
    ] {
        samples.push(Fact::StableRoot {
            operation: operation(),
            rule,
            root: input_ref("input.apply_payment.request", "event_id"),
        });
    }

    samples
}

/// A golden file per kind records its samples and the fact format they
/// were written at. A sample whose spelling changed while
/// `FACT_FORMAT` did not fails, and blessing refuses to rewrite it:
/// durability must not depend on someone noticing a serde change.
#[test]
fn fact_goldens_change_only_with_a_format_bump() {
    let bless = std::env::var_os("CONSEQA_BLESS").is_some();
    let directory = manifest().join("tests").join("golden").join("facts");

    let mut by_kind: BTreeMap<&str, Vec<Value>> = BTreeMap::new();

    for fact in samples() {
        let written = serde_json::to_value(&fact).expect("a sample serializes");

        assert_eq!(written["kind"], kind(&fact), "the tag names the kind");

        let read: Fact =
            basis::read_canonical(&written).unwrap_or_else(|error| panic!("{error}: {written}"));

        assert_eq!(read, fact, "a sample round-trips");

        by_kind.entry(kind(&fact)).or_default().push(written);
    }

    assert_eq!(
        by_kind.keys().copied().collect::<BTreeSet<_>>(),
        KINDS.into_iter().collect::<BTreeSet<_>>(),
        "every kind has samples"
    );

    if bless {
        fs::create_dir_all(&directory).expect("the golden directory");
    }

    for (kind, facts) in &by_kind {
        let file = directory.join(format!("{kind}.json"));

        let current = json!({ "fact_format": FACT_FORMAT, "facts": facts });

        let stored: Option<Value> = fs::read_to_string(&file)
            .ok()
            .map(|text| serde_json::from_str(&text).expect("a golden parses"));

        let recorded = stored
            .as_ref()
            .and_then(|stored| stored["fact_format"].as_u64());

        match &stored {
            Some(stored) if stored["facts"] == current["facts"] => {
                if recorded == Some(FACT_FORMAT.0) {
                    continue;
                }
            }

            Some(_) => assert_ne!(
                recorded,
                Some(FACT_FORMAT.0),
                "the durable form of `{kind}` changed while FACT_FORMAT stayed {FACT_FORMAT}; \
                 bump FACT_FORMAT in src/analyzer/basis/mod.rs, then re-bless with \
                 `CONSEQA_BLESS=1 cargo test --test basis`"
            ),

            None => {}
        }

        assert!(
            bless,
            "tests/golden/facts/{kind}.json is missing or records another fact format; \
             re-bless with `CONSEQA_BLESS=1 cargo test --test basis`"
        );

        let text = serde_json::to_string_pretty(&current).expect("a golden serializes");

        fs::write(&file, format!("{text}\n")).expect("a golden writes");
    }

    let stray: Vec<String> = fs::read_dir(&directory)
        .expect("the golden directory reads")
        .map(|entry| {
            entry
                .expect("a golden entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| !by_kind.keys().any(|kind| name == &format!("{kind}.json")))
        .collect();

    assert!(stray.is_empty(), "golden files for no fact kind: {stray:?}");
}

#[test]
fn non_canonical_spellings_are_refused_by_name() {
    let canonical = serde_json::to_value(Fact::ConflictClosure {
        root: transaction_id("operation.apply_payment", "tx.apply_payment"),
        key: input_ref("input.apply_payment.request", "order_id"),
        members: BTreeSet::from([
            transaction_id("operation.apply_payment", "tx.apply_payment"),
            transaction_id("operation.cancel_order", "tx.cancel_order"),
        ]),
    })
    .unwrap();

    let refused = |value: Value| -> CanonicalError {
        basis::read_canonical::<Fact>(&value).expect_err("refused")
    };

    // A value source written as its `kind:id` shorthand.
    let mut shorthand = canonical.clone();
    shorthand["key"]["source"] = json!("input:input.apply_payment.request");

    assert_eq!(
        refused(shorthand),
        CanonicalError::NotCanonical {
            path: "key.source".to_string()
        }
    );

    // A field path written dotted.
    let mut dotted = canonical.clone();
    dotted["key"]["path"] = json!("order_id");

    assert_eq!(
        refused(dotted),
        CanonicalError::NotCanonical {
            path: "key.path".to_string()
        }
    );

    // A set out of order.
    let mut unsorted = canonical.clone();
    unsorted["members"].as_array_mut().unwrap().reverse();

    assert_eq!(
        refused(unsorted),
        CanonicalError::NotCanonical {
            path: "members[0].operation".to_string()
        }
    );

    // An unknown kind, named.
    let error = refused(json!({ "kind": "consensus_quorum", "operation": "operation.x" }));

    assert!(error.to_string().contains("consensus_quorum"), "{error}");

    // An unknown field, named, on a kind with fields and on one without.
    let mut extra = canonical.clone();
    extra["witness"] = json!(true);

    let error = refused(extra);

    assert!(error.to_string().contains("witness"), "{error}");

    let mut coverage = serde_json::to_value(&samples()[3]).unwrap();
    coverage["coverage"]["witness"] = json!(true);

    assert_eq!(
        refused(coverage),
        CanonicalError::UnknownField {
            path: "coverage.witness".to_string()
        }
    );
}

#[test]
fn records_of_another_version_are_refused_by_name() {
    assert_eq!(
        basis::check_versions(&json!({ "dsl": 6, "fact_format": 1 })),
        Ok(())
    );

    let refused = |record: Value| basis::check_versions(&record).expect_err("refused");

    let error = refused(json!({ "dsl": 5, "fact_format": 1 }));

    assert_eq!(
        error,
        FactVersionError::DslMismatch {
            found: conseqa::spec::DslVersion(5)
        }
    );
    assert!(error.to_string().contains("dsl 5") && error.to_string().contains("dsl 6"));

    let error = refused(json!({ "dsl": 6, "fact_format": 2 }));

    assert_eq!(
        error,
        FactVersionError::FactFormatMismatch {
            found: basis::FactFormat(2)
        }
    );
    assert!(
        error.to_string().contains("fact format 2") && error.to_string().contains("fact format 1")
    );

    assert_eq!(
        refused(json!({ "fact_format": 1 })),
        FactVersionError::DslMissing
    );
    assert_eq!(
        refused(json!({ "dsl": 6 })),
        FactVersionError::FactFormatMissing
    );
}

/// One key of every member kind with a textual form, over ids that
/// need escaping.
fn textual_keys() -> Vec<DeclarationKey> {
    let odd = || id("a/b c%d");
    let dotted = || id(".hidden");
    let plain = || id("operation.apply_payment");

    vec![
        DeclarationKey::Service(id("service.é")),
        DeclarationKey::Schema(id("schema.OrderRecord")),
        DeclarationKey::DataModel(odd()),
        DeclarationKey::DataObject {
            data_model: id("data.orders"),
            object: dotted(),
        },
        DeclarationKey::Outbox {
            data_model: odd(),
            outbox: id("outbox.events"),
        },
        DeclarationKey::Topic(id("topic.payment_captured")),
        DeclarationKey::StateMachine(id("machine.order")),
        DeclarationKey::Transition {
            machine: id("machine.order"),
            transition: odd(),
        },
        DeclarationKey::Operation(plain()),
        DeclarationKey::Input {
            operation: plain(),
            input: id("input.apply_payment.request"),
        },
        DeclarationKey::Program(plain()),
        DeclarationKey::Transaction {
            operation: plain(),
            transaction: id("tx.apply_payment"),
        },
        DeclarationKey::EffectSite {
            operation: plain(),
            effect: dotted(),
        },
        DeclarationKey::TopicRuntime(id("topic.payment_captured")),
        DeclarationKey::SubscriptionRuntime {
            operation: id("operation.ship"),
            input: id("input.ship.paid"),
        },
        DeclarationKey::OutboxRuntime {
            operation: id("operation.relay"),
            input: odd(),
        },
        DeclarationKey::ExecutionPool(id("pool.workers")),
        DeclarationKey::Router(id("router.api")),
        DeclarationKey::StorageLayout(id("layout.orders")),
        DeclarationKey::PromptObligation(id("obligation.no double charge")),
    ]
}

#[test]
fn textual_keys_round_trip_and_refuse_every_other_spelling() {
    for key in textual_keys() {
        let text = key.textual().expect("a textual form");

        assert_eq!(text.matches(' ').count(), 1, "{text}");
        assert_eq!(DeclarationKey::parse(&text), Ok(key.clone()), "{text}");
        assert_eq!(key.to_string(), text);

        let wrapped = TextualKey::new(key.clone()).expect("textual");
        let written = serde_json::to_value(&wrapped).unwrap();

        assert_eq!(written, json!(text));
        assert_eq!(
            serde_json::from_value::<TextualKey>(written).unwrap(),
            wrapped
        );

        // The structured form round-trips too.
        let structured = serde_json::to_value(&key).unwrap();
        let read: DeclarationKey = basis::read_canonical(&structured).expect("structured");

        assert_eq!(read, key);
    }

    assert_eq!(
        DeclarationKey::parse("transaction operation.apply_payment/tx.apply_payment"),
        Ok(DeclarationKey::Transaction {
            operation: id("operation.apply_payment"),
            transaction: id("tx.apply_payment"),
        })
    );

    assert_eq!(
        DeclarationKey::parse("data_model a%2Fb%20c%25d"),
        Ok(DeclarationKey::DataModel(id("a/b c%d")))
    );

    let refused = |text: &str| DeclarationKey::parse(text).expect_err(text);

    assert!(matches!(
        refused("transaction"),
        TextualKeyError::Malformed { .. }
    ));
    assert!(matches!(
        refused("table data.orders"),
        TextualKeyError::UnknownKind { .. }
    ));
    assert!(matches!(
        refused("requirement x"),
        TextualKeyError::NoTextualForm { .. }
    ));
    assert_eq!(
        refused("transaction operation.apply_payment"),
        TextualKeyError::WrongArity {
            kind: "transaction".to_string(),
            expected: 2,
            found: 1
        }
    );

    // Lowercase escapes, an escaped dot, a bare space, and an empty id
    // each spell an id other than canonically.
    for text in [
        "data_model a%2fb",
        "data_model a%2Eb",
        "transaction  operation.x/tx.y",
        "transaction operation.x/",
        "data_model .hidden",
    ] {
        assert!(
            matches!(refused(text), TextualKeyError::NotCanonical { .. }),
            "{text}"
        );
    }

    // A requirement has no textual form.
    let requirement = DeclarationKey::Requirement(RequirementIdentity {
        subject: ObligationSubject::Operation {
            operation: id("operation.ship"),
        },
        family: RequirementFamily::Recoverability,
        requirement_key: vec![input_ref("input.ship.paid", "event_id")],
        occurrence: 0,
    });

    assert_eq!(requirement.textual(), None);
    assert_eq!(TextualKey::new(requirement), None);
}

#[test]
fn catalogue_entries_are_written_id_at_version() {
    assert_eq!(
        EntryRef::parse("kafka.keyed-topic@1"),
        Some(EntryRef {
            id: "kafka.keyed-topic".to_string(),
            version: 1
        })
    );

    for text in [
        "kafka",
        "@1",
        "kafka@",
        "kafka@01",
        "kafka@+1",
        "kafka keyed@1",
        "a@b@1",
    ] {
        assert_eq!(EntryRef::parse(text), None, "{text}");
    }
}

// ---------------------------------------------------------------------
// Reliance a proof uses without stating it (§18.4)
// ---------------------------------------------------------------------

/// Flash checkout's observed-version guards cover the insertion
/// `tx.create_order.new` makes, which holds only because no transaction
/// deletes an order: the absence is a fact of the basis, read from the
/// order object.
#[test]
fn an_observed_version_guard_over_an_insertion_rests_on_no_deleters() {
    let report = report_of(&fixture("flash_checkout.yaml"));

    let obligation = report
        .obligations
        .iter()
        .find(|obligation| {
            obligation.id
                == "oblig.operation.apply_payment.tx.apply_payment.transaction_serializability.0"
        })
        .expect("the payment's serializability obligation");

    assert_eq!(obligation.status, Status::Proven);

    let basis = obligation.basis.as_ref().unwrap();

    let no_deleters = basis
        .iter()
        .find(|fact| {
            fact.fact
                == Fact::NoDeleters {
                    data_model: id("data.checkout"),
                    object: id("object.order"),
                }
        })
        .expect("no_deleters(object.order) is in the basis");

    assert!(no_deleters.anchors.contains(&DeclarationKey::DataObject {
        data_model: id("data.checkout"),
        object: id("object.order"),
    }));

    // Only dependencies inside a cyclic component cite evidence, and the
    // closure is stated with every member.
    let closure = basis
        .iter()
        .find_map(|fact| match &fact.fact {
            Fact::ConflictClosure { members, .. } => Some(members.len()),
            _ => None,
        })
        .expect("the closure is in the basis");

    assert!(closure >= 2);
}
