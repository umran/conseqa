//! Realization profiles: needs, the catalogue, the stack, and choice
//! (§6–§10 and §14 of the realization profiles revision; its tests
//! §20.1, §20.2, §20.3, and §20.7).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use conseqa::analyzer::basis::{Fact, TextualKey};
use conseqa::analyzer::validation;
use conseqa::design::change::diff::{DiffDocument, Gate, diff};
use conseqa::design::change::realization::RealizationChanges;
use conseqa::design::change::render;
use conseqa::design::change::side::{Side, SideName, Source as SideSource};
use conseqa::design::{RunId, RunMetadata, WorkspaceState};
use conseqa::parser::yaml;
use conseqa::realization::RealizationError;
use conseqa::realization::capability::{Because, Capability, needs};
use conseqa::realization::catalogue::{Catalogue, Entry, Source, standard_files};
use conseqa::realization::choice::{
    self, ChooseError, ChooseRequest, Explanation, Problem, Profile, Refusal, State, Status,
};
use conseqa::realization::document;
use conseqa::spec::{
    FieldPath, Id, Lock, LockMode, LockOrder, MemberConcurrency, Model, OrderingSemantics,
    TransactionStep, ValueRef, ValueSource,
};

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every fixture that parses and validates.
fn fixtures() -> Vec<(String, Model)> {
    let mut paths: Vec<PathBuf> = fs::read_dir(manifest().join("tests/fixtures"))
        .expect("the fixtures directory reads")
        .map(|entry| entry.expect("a fixture entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("yaml"))
        .collect();

    paths.sort();

    paths
        .into_iter()
        .filter_map(|path| {
            let model = yaml::parse(&fs::read_to_string(&path).ok()?).ok()?;

            validation::validate(&model).is_empty().then(|| {
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    model,
                )
            })
        })
        .collect()
}

fn fixture(name: &str) -> Model {
    fixtures()
        .into_iter()
        .find(|(file, _)| file == name)
        .map(|(_, model)| model)
        .unwrap_or_else(|| panic!("`{name}` is a valid fixture"))
}

/// What each realizable declaration needs, as `axis: value` text.
fn needs_of(model: &Model) -> BTreeMap<String, Vec<String>> {
    needs(model)
        .into_iter()
        .map(|(key, needs)| {
            (
                key.to_string(),
                needs
                    .iter()
                    .map(|need| need.capability.to_string())
                    .collect(),
            )
        })
        .collect()
}

fn key(text: &str) -> TextualKey {
    text.parse()
        .unwrap_or_else(|error| panic!("`{text}`: {error}"))
}

fn id(text: &str) -> Id {
    Id(text.to_string())
}

/// The flash checkout with only the topology PostgreSQL and Kafka
/// realize: no pool bounds its members' concurrency, and no router
/// routes, so nothing needs a proxy or a worker runtime.
fn postgres_and_kafka_checkout() -> Model {
    let mut model = fixture("flash_checkout.yaml");
    let runtime = model
        .runtime
        .as_mut()
        .expect("the checkout declares a runtime");

    for pool in runtime.execution_pools.values_mut() {
        pool.member_concurrency = MemberConcurrency::Unbounded;
    }

    for router in runtime.routers.values_mut() {
        router.routing = None;
    }

    model
}

/// A directory holding a realization profile and nothing else: the
/// profile reads without a spec.
struct ProfileDir(PathBuf);

impl ProfileDir {
    fn new(technologies: &[&str]) -> Self {
        let dir = Self(
            std::env::temp_dir().join(format!("conseqa-realization-{}", uuid::Uuid::new_v4())),
        );

        dir.put("stack.yaml", &stack(technologies));

        dir
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Writes a file under `.conseqa/realization/`.
    fn put(&self, relative: &str, text: &str) {
        let path = self.0.join(".conseqa/realization").join(relative);

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(&self) -> Result<Option<Profile>, RealizationError> {
        Profile::read(&self.0)
    }

    fn profile(&self) -> Profile {
        self.read()
            .unwrap_or_else(|error| panic!("{error}"))
            .expect("a stack profile")
    }

    /// Why the profile does not load.
    fn refusal(&self) -> String {
        match self.read() {
            Ok(_) => panic!("the profile loads"),
            Err(error) => error.to_string(),
        }
    }
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stack(technologies: &[&str]) -> String {
    let mut text = "format: 1\ncatalogue: conseqa-standard@1\ntechnologies:\n".to_string();

    for technology in technologies {
        text.push_str(&format!("  - {technology}\n"));
    }

    if technologies.is_empty() {
        text = "format: 1\ncatalogue: conseqa-standard@1\ntechnologies: []\n".to_string();
    }

    text
}

fn state<'s>(status: &'s Status, declaration: &str) -> &'s State {
    &status
        .declarations
        .iter()
        .find(|realized| realized.declaration.to_string() == declaration)
        .unwrap_or_else(|| panic!("`{declaration}` is realizable"))
        .state
}

/// The entry and options a declaration is realized by, as text.
fn realization(status: &Status, declaration: &str) -> (String, String, Vec<String>) {
    let state = state(status, declaration);

    let (entry, options) = state
        .realization()
        .unwrap_or_else(|| panic!("`{declaration}` is {}", state.name()));

    (
        state.name().to_string(),
        entry.to_string(),
        options.to_vec(),
    )
}

fn choose(
    dir: &ProfileDir,
    model: &Model,
    declaration: &str,
    entry: &str,
    options: &[&str],
) -> Result<choice::Chosen, Refusal> {
    let request = ChooseRequest {
        declaration: key(declaration),
        entry: entry.to_string(),
        options: options.iter().map(ToString::to_string).collect(),
        reason: Some("chosen in a test".to_string()),
    };

    match choice::choose(dir.path(), model, &request) {
        Ok(chosen) => Ok(chosen),
        Err(ChooseError::Refused(refusal)) => Err(*refusal),
        Err(ChooseError::Failed(error)) => panic!("{error}"),
    }
}

// ---- §20.1 Needs ------------------------------------------------------

#[test]
fn every_fixtures_needs_match_the_hand_written_expectation() {
    let expected: BTreeMap<String, BTreeMap<String, Vec<String>>> =
        serde_yaml::from_str(include_str!("realization/needs.yaml"))
            .expect("the expectation reads");

    let fixtures = fixtures();

    assert_eq!(
        fixtures
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>(),
        expected.keys().cloned().collect::<BTreeSet<_>>(),
        "every valid fixture has an expectation, and every expectation a fixture"
    );

    for (name, model) in &fixtures {
        assert_eq!(needs_of(model), expected[name], "{name}");
    }
}

#[test]
fn adding_a_lock_step_adds_lock_exclusive_to_its_data_model_and_nothing_else() {
    let before = fixture("flash_checkout.yaml");
    let mut after = before.clone();

    // The transfer's exclusive lock, moved onto the order the
    // cancellation reads.
    let lock = after.operations[&id("operation.transfer_stock")]
        .program
        .transaction(&id("tx.transfer_stock"))
        .expect("the transfer")
        .steps
        .iter()
        .find(|step| matches!(step, TransactionStep::Lock(_)))
        .cloned()
        .expect("the transfer locks");

    let cancel = after
        .operations
        .get_mut(&id("operation.cancel_order"))
        .unwrap()
        .program
        .transaction_mut(&id("tx.cancel_order"))
        .expect("the cancellation");

    let TransactionStep::Read(read) = &cancel.steps[0] else {
        panic!("the cancellation reads first");
    };

    let TransactionStep::Lock(mut lock) = lock else {
        unreachable!()
    };

    lock.target = read.target.clone();
    cancel.steps.insert(0, TransactionStep::Lock(lock));

    let mut expected = needs_of(&before);

    assert_eq!(
        expected.insert(
            "data_model data.checkout".to_string(),
            [
                "durability: committed",
                "isolation: read_committed",
                "transaction: interactive",
                "lock: exclusive",
                "mutation: conditional",
                "uniqueness: identity",
            ]
            .map(str::to_string)
            .to_vec(),
        ),
        Some(
            [
                "durability: committed",
                "isolation: read_committed",
                "transaction: interactive",
                "mutation: conditional",
                "uniqueness: identity",
            ]
            .map(str::to_string)
            .to_vec()
        )
    );

    assert_eq!(needs_of(&after), expected);
}

#[test]
fn every_need_names_the_declarations_that_ask_for_it() {
    let needs = needs(&fixture("flash_checkout.yaml"));
    let inventory = &needs[&key("data_model data.inventory")];

    let sources = |capability: Capability| -> Vec<String> {
        inventory
            .iter()
            .find(|need| need.capability == capability)
            .unwrap_or_else(|| panic!("{capability} is needed"))
            .sources
            .iter()
            .map(ToString::to_string)
            .collect()
    };

    assert_eq!(
        sources(Capability::DurabilityCommitted),
        ["data_model data.inventory: it persists what its transactions commit"]
    );

    assert_eq!(
        sources(Capability::LockExclusive),
        [
            "transaction operation.transfer_stock/tx.transfer_stock: step 1 (lock object.stock)",
            "transaction operation.transfer_stock/tx.transfer_stock: step 2 (lock object.stock)",
        ]
    );

    assert_eq!(
        sources(Capability::TransactionMultiInstance),
        [
            "transaction operation.transfer_stock/tx.transfer_stock: writes several instances \
             in one commit: step 4 (update object.stock), step 5 (update object.stock)"
        ]
    );

    assert_eq!(
        sources(Capability::TransactionInteractive),
        [
            "transaction operation.reserve_inventory/tx.reserve_inventory: step 2 (update \
             object.stock) uses `transaction_read:read.reserve_inventory.stock`, read in the \
             same transaction",
            "transaction operation.transfer_stock/tx.transfer_stock: step 4 (update \
             object.stock) uses `transaction_read:read.transfer_stock.source_stock`, read in \
             the same transaction",
        ]
    );

    let runtime = &needs[&key("storage_layout layout.stock")];

    assert_eq!(runtime[0].capability, Capability::PartitioningComposite);
    assert_eq!(
        runtime[0].sources[0].because,
        Because::Declares {
            field: "partition_key"
        }
    );
}

#[test]
fn a_value_read_from_a_state_machine_subject_is_a_decision_on_what_was_read() {
    let mut model = fixture("flash_checkout.yaml");
    let machine = model
        .state_machines
        .keys()
        .next()
        .expect("the checkout has a state machine")
        .clone();

    let create = model
        .operations
        .get_mut(&id("operation.create_order"))
        .unwrap()
        .program
        .transaction_mut(&id("tx.create_order.new"))
        .expect("the creation");

    let TransactionStep::Insert(insert) = &mut create.steps[0] else {
        panic!("the creation inserts first");
    };

    let conseqa::spec::Derivation::Deterministic { from } = &mut insert.values else {
        panic!("the insert's derivation is deterministic");
    };

    from.push(ValueRef {
        source: ValueSource::StateMachineSubject(machine.clone()),
        path: FieldPath(vec!["status".to_string()]),
    });

    let needs = needs(&model);

    let interactive = needs[&key("data_model data.checkout")]
        .iter()
        .find(|need| need.capability == Capability::TransactionInteractive)
        .expect("the checkout reads, then decides");

    assert!(
        interactive.sources.iter().any(|source| {
            source.declaration == key("transaction operation.create_order/tx.create_order.new")
                && matches!(
                    &source.because,
                    Because::ReadsThenDecides { source, .. }
                        if *source == format!("state_machine_subject:{machine}")
                )
        }),
        "{:#?}",
        interactive.sources
    );
}

// ---- §20.2 Choice -----------------------------------------------------

#[test]
fn postgres_and_kafka_realize_every_declaration_automatically() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7"]);
    let status = Status::of(&dir.profile(), &postgres_and_kafka_checkout());

    assert_eq!(status.failing().count(), 0, "{}", status.render());

    for realized in &status.declarations {
        assert!(
            matches!(realized.state, State::Automatic { .. } | State::Unneeded),
            "{} is {}",
            realized.declaration,
            realized.state.name()
        );
    }

    let automatic = |entry: &str, options: &[&str]| {
        (
            "automatic".to_string(),
            entry.to_string(),
            options.iter().map(ToString::to_string).collect::<Vec<_>>(),
        )
    };

    assert_eq!(
        realization(&status, "data_model data.checkout"),
        automatic("postgres.database@1", &[])
    );
    assert_eq!(
        realization(&status, "data_model data.inventory"),
        automatic("postgres.database@1", &[])
    );
    assert_eq!(
        realization(&status, "topic topic.order_events"),
        automatic("kafka.topic@1", &[])
    );
    assert_eq!(
        realization(&status, "topic_runtime topic.order_events"),
        automatic("kafka.keyed-topic@1", &[])
    );
    assert_eq!(
        realization(
            &status,
            "subscription_runtime operation.charge_payment/input.charge_payment.reserved"
        ),
        automatic("kafka.consumer-group@1", &["commit_after_processing"])
    );
    assert_eq!(
        realization(&status, "storage_layout layout.stock"),
        automatic("postgres.partitioned-table@1", &[])
    );

    for unneeded in [
        "execution_pool pool.order_workers",
        "router router.create_order",
    ] {
        assert_eq!(state(&status, unneeded), &State::Unneeded);
    }
}

#[test]
fn adding_sqs_makes_the_topic_runtimes_ambiguous_until_chosen() {
    let model = postgres_and_kafka_checkout();
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);
    let status = Status::of(&dir.profile(), &model);

    let State::Ambiguous { valid } = state(&status, "topic_runtime topic.order_events") else {
        panic!("{}", status.render());
    };

    assert_eq!(
        valid
            .iter()
            .map(|valid| (valid.entry.to_string(), valid.options.clone()))
            .collect::<Vec<_>>(),
        [
            ("kafka.keyed-topic@1".to_string(), Vec::<String>::new()),
            ("sqs.fifo@1".to_string(), Vec::new()),
        ]
    );

    // Only the topic runtime has a choice to make.
    assert_eq!(
        status
            .failing()
            .map(|realized| realized.declaration.to_string())
            .collect::<Vec<_>>(),
        ["topic_runtime topic.order_events"]
    );

    let chosen = choose(
        &dir,
        &model,
        "topic_runtime topic.order_events",
        "kafka.keyed-topic",
        &[],
    )
    .expect("a valid choice");

    assert!(chosen.written && !chosen.replaced);
    assert_eq!(
        chosen.path,
        ".conseqa/realization/choices/topic_runtime/topic.order_events.yaml"
    );

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        state(&status, "topic_runtime topic.order_events"),
        &State::Chosen {
            entry: conseqa::realization::catalogue::entry_ref("kafka.keyed-topic@1").unwrap(),
            options: Vec::new(),
            reason: Some("chosen in a test".to_string()),
        }
    );
    assert_eq!(status.failing().count(), 0, "{}", status.render());
}

#[test]
fn a_dynamodb_stack_makes_an_interactive_data_model_infeasible() {
    let dir = ProfileDir::new(&["dynamodb@2012.08.10", "kafka@3.7"]);
    let status = Status::of(&dir.profile(), &postgres_and_kafka_checkout());

    let State::Infeasible { gaps } = state(&status, "data_model data.checkout") else {
        panic!("{}", status.render());
    };

    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].entry.to_string(), "dynamodb.table@1");

    let missing: Vec<(String, bool)> = gaps[0]
        .missing
        .iter()
        .map(|missing| (missing.capability.to_string(), missing.reason.is_some()))
        .collect();

    // Interactive transactions are what DynamoDB says it cannot
    // provide; read committed isolation it simply does not list.
    assert_eq!(
        missing,
        [
            ("isolation: read_committed".to_string(), false),
            ("transaction: interactive".to_string(), true),
        ]
    );

    let report = status.render();

    for line in [
        "infeasible     data_model data.checkout",
        "  needs transaction: interactive    transaction operation.cancel_order/tx.cancel_order: \
         step 2 (transition object.order) uses `transaction_read:read.cancel_order.order`, read \
         in the same transaction",
        "  dynamodb.table@1  cannot provide transaction: interactive: A DynamoDB transaction is \
         one TransactWriteItems call, whose writes and condition checks are fixed when it is \
         sent, or one TransactGetItems call, which only reads: no transaction spans a read, a \
         decision and a write. Express the decision as conditions on the writes.",
        "  dynamodb.table@1  does not provide isolation: read_committed",
    ] {
        assert!(report.contains(line), "missing {line:?} in\n{report}");
    }
}

#[test]
fn postgres_before_9_5_is_no_candidate_for_a_serializable_data_model() {
    let model = fixture("video_streaming.yaml");

    let old = ProfileDir::new(&["postgres@9.0"]);
    let status = Status::of(&old.profile(), &model);

    assert_eq!(
        state(&status, "data_model data.videos"),
        &State::Infeasible { gaps: Vec::new() },
        "no candidate at all"
    );

    let explanation = Explanation::of(&old.profile(), &model, &key("data_model data.videos"))
        .expect("the data model is realizable");

    assert!(explanation.candidates.is_empty());
    assert!(
        explanation
            .outside
            .iter()
            .any(|outside| outside.entry.to_string() == "postgres.database@1"
                && outside.stack.as_deref() == Some("9.0")
                && outside.versions == ">=9.5"),
        "{}",
        explanation.render()
    );

    let current = ProfileDir::new(&["postgres@16"]);
    let status = Status::of(&current.profile(), &model);

    assert_eq!(
        realization(&status, "data_model data.videos").1,
        "postgres.database@1"
    );
}

#[test]
fn oracle_is_never_valid_for_serializable_isolation() {
    let dir = ProfileDir::new(&["oracle@19"]);

    let status = Status::of(&dir.profile(), &fixture("video_streaming.yaml"));

    for serializable in ["data_model data.videos", "data_model data.transcoder"] {
        let State::Infeasible { gaps } = state(&status, serializable) else {
            panic!("{}", status.render());
        };

        let missing: Vec<_> = gaps
            .iter()
            .flat_map(|gap| gap.missing.iter())
            .filter(|missing| missing.capability == Capability::IsolationSerializable)
            .collect();

        assert_eq!(missing.len(), 1, "{serializable}");
        assert!(
            missing[0].reason.as_deref().unwrap().contains("write skew"),
            "{:?}",
            missing[0].reason
        );
    }

    // At read committed, Oracle is the one candidate.
    let status = Status::of(&dir.profile(), &fixture("flash_checkout.yaml"));

    assert_eq!(
        realization(&status, "data_model data.checkout"),
        (
            "automatic".to_string(),
            "oracle.database@1".to_string(),
            Vec::new()
        )
    );
}

// ---- §20.3 Options ----------------------------------------------------

#[test]
fn automatic_choice_takes_single_partition_only_where_global_order_is_needed() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7"]);
    let mut model = postgres_and_kafka_checkout();

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        realization(&status, "topic_runtime topic.order_events").2,
        Vec::<String>::new(),
        "per-key order needs no single partition"
    );

    model
        .runtime
        .as_mut()
        .unwrap()
        .topics
        .get_mut(&id("topic.order_events"))
        .unwrap()
        .ordering = Some(OrderingSemantics::Global);

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        realization(&status, "topic_runtime topic.order_events"),
        (
            "automatic".to_string(),
            "kafka.keyed-topic@1".to_string(),
            vec!["single_partition".to_string()]
        )
    );
}

#[test]
fn choosing_an_option_adds_its_own_requirements_and_nothing_else() {
    let catalogue = Catalogue::standard().unwrap();
    let topic = catalogue.platform("kafka.keyed-topic").unwrap();

    let ids = |options: &[&str]| -> Vec<String> {
        topic
            .requirements_with(&options.iter().map(ToString::to_string).collect())
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect()
    };

    assert_eq!(
        ids(&[]),
        ["record-key", "idempotent-producer", "stable-partitions"]
    );
    assert_eq!(
        ids(&["single_partition"]),
        [
            "record-key",
            "idempotent-producer",
            "stable-partitions",
            "single-partition"
        ]
    );

    // The architect may choose the option where it is not needed.
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7"]);
    let model = postgres_and_kafka_checkout();

    choose(
        &dir,
        &model,
        "topic_runtime topic.order_events",
        "kafka.keyed-topic@1",
        &["single_partition"],
    )
    .expect("a valid choice");

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        realization(&status, "topic_runtime topic.order_events"),
        (
            "chosen".to_string(),
            "kafka.keyed-topic@1".to_string(),
            vec!["single_partition".to_string()]
        )
    );
}

#[test]
fn options_that_exclude_each_other_are_refused_together() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7"]);
    let model = postgres_and_kafka_checkout();

    let refusal = choose(
        &dir,
        &model,
        "subscription_runtime operation.charge_payment/input.charge_payment.reserved",
        "kafka.consumer-group",
        &["commit_after_processing", "commit_before_processing"],
    )
    .expect_err("the commit modes exclude each other");

    let Refusal::Invalid { problems, .. } = &refusal else {
        panic!("{refusal}");
    };

    assert!(
        problems.contains(&Problem::ExcludedOptions {
            option: "commit_after_processing".to_string(),
            excludes: "commit_before_processing".to_string(),
        }),
        "{refusal}"
    );
}

// ---- Choices ----------------------------------------------------------

#[test]
fn an_invalid_choice_names_what_it_lacks_and_fails_the_gate() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);
    let model = postgres_and_kafka_checkout();

    let problems = |entry: &str| -> String {
        match choose(&dir, &model, "topic_runtime topic.order_events", entry, &[]) {
            Err(refusal @ Refusal::Invalid { .. }) => refusal.to_string(),
            other => panic!("{other:?}"),
        }
    };

    assert_eq!(
        problems("sqs.standard"),
        "`sqs.standard@1` is not a valid realization: it does not provide `grouping: keyed`, \
         `ordering: within_group` (A standard queue makes only a best-effort attempt to keep \
         the order messages are sent in, and messages may arrive out of order.)"
    );
    assert_eq!(
        problems("postgres.database"),
        "`postgres.database@1` is not a valid realization: `postgres.database` realizes a \
         `data_model`, and the declaration is a `topic_runtime`"
    );
    assert_eq!(
        problems("envoy.ring-hash@1"),
        "`envoy.ring-hash@1` is not a valid realization: `envoy.ring-hash` realizes a \
         `router`, and the declaration is a `topic_runtime`"
    );
    assert_eq!(
        problems("kafka.keyed-topic@2"),
        "`kafka.keyed-topic@2` is not a valid realization: `kafka.keyed-topic` is at version 1 \
         now, and the choice names version 2, whose claims may differ"
    );
    assert_eq!(
        problems("acme.queue"),
        "`acme.queue` is not a valid realization: no catalogue holds `acme.queue`"
    );

    // A choice file the stack no longer supports is an invalid choice,
    // which fails the gate until re-chosen.
    choose(
        &dir,
        &model,
        "topic_runtime topic.order_events",
        "sqs.fifo",
        &[],
    )
    .expect("SQS FIFO orders within groups");

    dir.put("stack.yaml", &stack(&["postgres@16", "kafka@3.7"]));

    let status = Status::of(&dir.profile(), &model);

    let State::InvalidChoice { problems, .. } = state(&status, "topic_runtime topic.order_events")
    else {
        panic!("{}", status.render());
    };

    assert_eq!(
        problems,
        &[Problem::NotInStack {
            technology: "sqs".to_string(),
            versions: ">=2012.11.5".to_string(),
        }]
    );
    assert_eq!(status.failing().count(), 1);
}

#[test]
fn a_choice_of_an_entry_version_the_catalogue_no_longer_holds_is_invalid() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7", "acme@1"]);
    let model = postgres_and_kafka_checkout();

    let entry = |version: u32| {
        format!(
            "format: 1\nid: acme.log\nversion: {version}\n\
             technology: {{ name: acme, versions: \">=1\" }}\n\
             realizes: {{ kind: topic_runtime }}\n\
             provides:\n  - grouping: keyed\n  - ordering: within_group\n"
        )
    };

    dir.put("catalogue/acme.log.yaml", &entry(1));

    choose(
        &dir,
        &model,
        "topic_runtime topic.order_events",
        "acme.log",
        &[],
    )
    .expect("a valid choice");

    // The entry's claims change: what was checked against version 1
    // holds no longer.
    dir.put("catalogue/acme.log.yaml", &entry(2));

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        state(&status, "topic_runtime topic.order_events"),
        &State::InvalidChoice {
            entry: conseqa::realization::catalogue::entry_ref("acme.log@1").unwrap(),
            options: Vec::new(),
            problems: vec![Problem::OtherVersion {
                entry: "acme.log".to_string(),
                chosen: 1,
                current: 2,
            }],
        }
    );
}

#[test]
fn an_identical_choice_writes_nothing_and_a_new_one_replaces_it() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);
    let model = postgres_and_kafka_checkout();
    let runtime = "topic_runtime topic.order_events";

    let first = choose(&dir, &model, runtime, "sqs.fifo", &[]).unwrap();
    let path = dir.path().join(&first.path);
    let written = fs::read_to_string(&path).unwrap();

    assert_eq!(
        written,
        "format: 1\ndeclaration: topic_runtime topic.order_events\nentry: sqs.fifo@1\n\
         reason: chosen in a test\n"
    );

    let again = choose(&dir, &model, runtime, "sqs.fifo@1", &[]).unwrap();

    assert!(!again.written && !again.replaced);

    let other = choose(&dir, &model, runtime, "kafka.keyed-topic", &[]).unwrap();

    assert!(other.written && other.replaced);
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("kafka.keyed-topic@1")
    );
}

#[test]
fn unneeded_declarations_fail_nothing_and_may_still_be_chosen() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7", "nomad@1.8"]);
    let model = postgres_and_kafka_checkout();

    dir.put(
        "catalogue/nomad.job.yaml",
        "format: 1\nid: nomad.job\nversion: 1\n\
         technology: { name: nomad, versions: \">=1.6\" }\n\
         realizes: { kind: execution_pool }\n\
         provides:\n  - concurrency: bounded\n",
    );

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        state(&status, "execution_pool pool.order_workers"),
        &State::Unneeded,
        "an unbounded pool needs nothing, whatever the stack offers"
    );

    choose(
        &dir,
        &model,
        "execution_pool pool.order_workers",
        "nomad.job",
        &[],
    )
    .expect("a valid choice");

    let status = Status::of(&dir.profile(), &model);

    assert_eq!(
        realization(&status, "execution_pool pool.order_workers").0,
        "chosen"
    );

    // A bounded pool needs what the project's entry provides.
    let bounded = Status::of(&dir.profile(), &fixture("flash_checkout.yaml"));

    assert_eq!(
        realization(&bounded, "execution_pool pool.payment_workers"),
        (
            "automatic".to_string(),
            "nomad.job@1".to_string(),
            Vec::new()
        )
    );
}

#[test]
fn a_choice_whose_declaration_is_gone_is_orphaned_and_fails_nothing() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7"]);

    dir.put(
        "choices/router/router.gone.yaml",
        "format: 1\ndeclaration: router router.gone\nentry: envoy.ring-hash@1\n",
    );

    let status = Status::of(&dir.profile(), &postgres_and_kafka_checkout());

    assert_eq!(
        status
            .orphaned
            .iter()
            .map(|orphaned| (orphaned.declaration.to_string(), orphaned.entry.to_string()))
            .collect::<Vec<_>>(),
        [(
            "router router.gone".to_string(),
            "envoy.ring-hash@1".to_string()
        )]
    );
    assert_eq!(status.failing().count(), 0);
}

#[test]
fn what_is_not_realizable_is_refused_by_name() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7"]);
    let profile = dir.profile();
    let model = postgres_and_kafka_checkout();

    let refused = |declaration: &str| {
        Explanation::of(&profile, &model, &key(declaration))
            .expect_err("not explained")
            .to_string()
    };

    assert_eq!(
        refused("data_object data.checkout/object.order"),
        "`data_object data.checkout/object.order` is realized by its data model's store: see \
         `data_model data.checkout`"
    );
    assert!(
        refused("transaction operation.cancel_order/tx.cancel_order").starts_with(
            "`transaction operation.cancel_order/tx.cancel_order` is not of a realizable kind"
        )
    );
    assert_eq!(
        refused("data_model data.nowhere"),
        "the spec has no declaration `data_model data.nowhere`"
    );

    let empty = ProfileDir::new(&[]);

    fs::remove_file(empty.path().join(".conseqa/realization/stack.yaml")).unwrap();

    let request = ChooseRequest {
        declaration: key("data_model data.checkout"),
        entry: "postgres.database".to_string(),
        options: Vec::new(),
        reason: None,
    };

    assert!(matches!(
        choice::choose(empty.path(), &model, &request),
        Err(ChooseError::Refused(refusal)) if *refusal == Refusal::NoStack
    ));
}

#[test]
fn without_a_stack_nothing_is_realized() {
    let dir = ProfileDir::new(&[]);

    fs::remove_file(dir.path().join(".conseqa/realization/stack.yaml")).unwrap();

    let status = Status::derive(dir.path(), &postgres_and_kafka_checkout()).unwrap();

    assert_eq!(status.profile, None);
    assert!(status.declarations.is_empty());
    assert_eq!(status.failing().count(), 0);
    assert!(status.render().starts_with("no stack profile"));
}

// ---- §20.7 Strictness and versions ------------------------------------

/// A minimal valid project entry, to break one rule at a time.
const ENTRY: &str = "format: 1
id: acme.log
version: 1
technology: { name: acme, versions: \">=1\" }
realizes: { kind: topic_runtime }
provides:
  - grouping: keyed
  - capability: { ordering: global }
    with: { option: one_partition }
cannot_provide:
  - capability: { ordering: global }
    unless: { option: one_partition }
    reason: It orders within a partition only.
options:
  one_partition: { description: One partition. }
requirements:
  - id: keyed
    of: producers
    statement: Producers key every record.
evidence:
  keyed: { look_for: the producer call }
checkers:
  - id: producer-key
    kind: properties
    covers: [keyed]
    assert: { key: key.serializer, equals: string }
";

#[test]
fn a_valid_project_entry_loads_beside_the_standard_catalogue() {
    let dir = ProfileDir::new(&["acme@1"]);

    dir.put("catalogue/acme.log.yaml", ENTRY);

    let profile = dir.profile();
    let entry = profile
        .catalogue
        .get("acme.log")
        .expect("the project entry");

    assert_eq!(entry.source, Source::Project);
    assert!(matches!(entry.entry, Entry::Platform(_)));
    assert_eq!(
        profile.catalogue.get("kafka.keyed-topic").unwrap().source,
        Source::Standard
    );
}

#[test]
fn the_catalogue_reader_refuses_each_broken_rule_by_name() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "an unknown field",
            "version: 1\n",
            "version: 1\nflavour: strong\n",
        ),
        (
            "an unknown capability value",
            "  - grouping: keyed\n",
            "  - grouping: hashed\n",
        ),
        (
            "an unknown capability axis",
            "  - grouping: keyed\n",
            "  - colour: blue\n",
        ),
        (
            "a kind that is not realizable",
            "realizes: { kind: topic_runtime }",
            "realizes: { kind: data_object }",
        ),
        (
            "an option referenced but not defined",
            "    with: { option: one_partition }",
            "    with: { option: two_partitions }",
        ),
        (
            "a requirement of no neighbour relation",
            "    of: producers\n",
            "    of: callers\n",
        ),
        (
            "evidence for no requirement",
            "  keyed: { look_for: the producer call }",
            "  keyed: { look_for: the producer call }\n  unkeyed: { look_for: nothing }",
        ),
        (
            "a checker covering no requirement",
            "    covers: [keyed]",
            "    covers: [unkeyed]",
        ),
        (
            "a checker assertion outside its kind's shape",
            "    assert: { key: key.serializer, equals: string }",
            "    assert: { resource: kafka_topic, present: true }",
        ),
        (
            "a provision the entry also excludes",
            "  - grouping: keyed\n",
            "  - grouping: keyed\n  - ordering: global\n",
        ),
    ];

    let expected = [
        "unknown field `flavour`",
        "`grouping` has no value `hashed`; its values are keyed",
        "`colour` is not a capability axis",
        "unknown variant `data_object`",
        "names the option `two_partitions`, which the entry does not define",
        "the requirement `keyed` applies to its `callers`, but a `topic_runtime` has none; its \
         relations are self, producers, consumers",
        "`evidence` names the requirement `unkeyed`, which the entry does not define",
        "the checker `producer-key` covers `unkeyed`, which the entry does not define",
        "checker `producer-key`: its `assert` is not a `properties` assertion",
        "it provides `ordering: global` and says it cannot",
    ];

    for ((case, from, to), expected) in cases.iter().zip(expected) {
        assert!(
            ENTRY.contains(from),
            "{case}: the base entry holds {from:?}"
        );

        let dir = ProfileDir::new(&["acme@1"]);

        dir.put("catalogue/acme.log.yaml", &ENTRY.replacen(from, to, 1));

        let refusal = dir.refusal();

        assert!(
            refusal.contains("acme.log.yaml does not load") && refusal.contains(expected),
            "{case}: {refusal}"
        );
    }
}

#[test]
fn files_out_of_place_are_refused_by_name() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "a project entry with a standard entry's id",
            "catalogue/kafka.topic.yaml",
            "`kafka.topic` is a standard entry's id",
        ),
        (
            "an entry under another id's name",
            "catalogue/acme.other.yaml",
            "it is `acme.log`, but its file names `acme.other`",
        ),
    ];

    for (case, path, expected) in cases {
        let dir = ProfileDir::new(&["acme@1"]);
        let entry = if path.contains("kafka.topic") {
            ENTRY.replace("id: acme.log", "id: kafka.topic")
        } else {
            ENTRY.to_string()
        };

        dir.put(path, &entry);

        let refusal = dir.refusal();

        assert!(refusal.contains(expected), "{case}: {refusal}");
    }

    let dir = ProfileDir::new(&["acme@1"]);

    dir.put("notes.txt", "remember the milk");

    assert!(
        dir.refusal().contains(
            "notes.txt: unexpected entry; it holds `stack.yaml`, `catalogue/`, and `choices/`"
        ),
        "{}",
        dir.refusal()
    );

    let dir = ProfileDir::new(&["acme@1"]);

    dir.put(
        "catalogue/acme.idiom.yaml",
        "format: 1\nid: acme.idiom\nversion: 1\n",
    );

    assert!(
        dir.refusal().contains(
            "an entry is a platform entry, with `realizes`, or an idiom entry, with `primitive`"
        ),
        "{}",
        dir.refusal()
    );

    let dir = ProfileDir::new(&["acme@1"]);

    dir.put(
        "catalogue/acme.idiom.yaml",
        "format: 1\nid: acme.idiom\nversion: 1\n\
         technology: { name: acme, versions: \">=1\" }\n\
         platforms: [mysql.database]\nprimitive: { mutation: conditional }\n\
         template: UPDATE t SET v = 1 WHERE id = 1 AND version = 2\n\
         evidence: { look_for: the guarded update }\n",
    );

    assert!(
        dir.refusal()
            .contains("it names the platform `mysql.database`, which no catalogue holds"),
        "{}",
        dir.refusal()
    );
}

#[test]
fn a_stack_and_a_choice_of_the_wrong_shape_are_refused_by_name() {
    let refused = |technologies: &str| {
        let dir = ProfileDir::new(&[]);

        dir.put(
            "stack.yaml",
            &format!("format: 1\ncatalogue: conseqa-standard@1\ntechnologies: [{technologies}]\n"),
        );

        dir.refusal()
    };

    assert!(refused("postgres").contains(
        "`postgres` names no version; write it as `postgres@<version>`, since entries are \
             true of version ranges"
    ));
    assert!(refused("postgres@16, postgres@15").contains("`postgres` is named twice"));
    assert!(refused("postgres@sixteen").contains("`sixteen` is not a version"));
    assert!(refused("Postgres@16").contains("`Postgres` is not a technology name"));

    let dir = ProfileDir::new(&[]);

    dir.put(
        "stack.yaml",
        "format: 1\ncatalogue: conseqa-standard@0\ntechnologies: []\n",
    );

    assert!(
        dir.refusal().contains(
            "names the catalogue `conseqa-standard@0`, but this build ships `conseqa-standard@1`"
        ),
        "{}",
        dir.refusal()
    );

    let dir = ProfileDir::new(&["postgres@16"]);

    dir.put(
        "choices/data_model/data.checkout.yaml",
        "format: 2\ndeclaration: data_model data.checkout\nentry: postgres.database@1\n",
    );

    assert!(
        dir.refusal()
            .contains("data.checkout.yaml is realization format 2, but this build reads format 1"),
        "{}",
        dir.refusal()
    );

    for (choice, expected) in [
        (
            "format: 1\ndeclaration: data_model data.orders\nentry: postgres.database@1\n",
            "it declares `data_model data.orders`, but its path names `data_model data.checkout`",
        ),
        (
            "format: 1\ndeclaration: data_model data.checkout\nentry: postgres.database@1\n\
             chosen_by: Ana\n",
            "unknown field `chosen_by`",
        ),
        (
            "format: 1\ndeclaration: data_model data.checkout\nentry: postgres.database\n",
            "is not a catalogue entry written `id@version`",
        ),
        (
            "format: 1\ndeclaration: data_model data.checkout\nentry: postgres.database@1\n\
             options: [a, a]\n",
            "it names the option `a` twice",
        ),
    ] {
        let dir = ProfileDir::new(&["postgres@16"]);

        dir.put("choices/data_model/data.checkout.yaml", choice);

        assert!(dir.refusal().contains(expected), "{}", dir.refusal());
    }

    let dir = ProfileDir::new(&["postgres@16"]);

    dir.put(
        "choices/data_object/data.checkout/object.order.yaml",
        "format: 1\ndeclaration: data_object data.checkout/object.order\nentry: postgres.database@1\n",
    );

    assert!(
        dir.refusal()
            .contains("it holds one directory per realizable kind"),
        "{}",
        dir.refusal()
    );
}

// ---- The standard catalogue (§8.3) ------------------------------------

#[test]
fn every_standard_claim_cites_the_vendor_documentation_it_rests_on() {
    let catalogue = Catalogue::standard().unwrap();

    let mut files: Vec<String> = fs::read_dir(manifest().join("catalogue"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|name| name.strip_suffix(".yaml").map(str::to_string))
        .collect();

    files.sort();

    assert_eq!(
        files,
        standard_files().map(str::to_string).collect::<Vec<_>>(),
        "every file in catalogue/ is embedded, and nothing else"
    );

    for catalogued in catalogue.entries() {
        let Entry::Platform(entry) = &catalogued.entry else {
            continue;
        };

        let id = &entry.id;
        let citations: BTreeSet<&String> = entry.citations.iter().collect();
        let mut cited = BTreeSet::new();

        let claims = entry
            .provides
            .iter()
            .map(|provision| (provision.capability.to_string(), &provision.cite))
            .chain(
                entry
                    .cannot_provide
                    .iter()
                    .map(|exclusion| (format!("not {}", exclusion.capability), &exclusion.cite)),
            )
            .chain(
                entry
                    .requirements
                    .iter()
                    .map(|requirement| (requirement.id.clone(), &requirement.cite)),
            );

        for (claim, cite) in claims {
            assert!(!cite.is_empty(), "{id}: `{claim}` cites nothing");

            for url in cite {
                assert!(url.starts_with("https://"), "{id}: `{claim}` cites {url}");
                assert!(
                    citations.contains(url),
                    "{id}: `{claim}` cites {url}, which `citations` does not list"
                );

                cited.insert(url);
            }
        }

        assert_eq!(cited, citations, "{id}: every citation is cited");

        for requirement in &entry.requirements {
            assert!(
                entry.evidence.contains_key(&requirement.id),
                "{id}: `{}` says where its evidence is",
                requirement.id
            );
        }
    }
}

#[test]
fn kafka_consumer_groups_hold_keys_to_members_and_never_rotate_them() {
    let catalogue = Catalogue::standard().unwrap();
    let group = catalogue.platform("kafka.consumer-group").unwrap();

    let none = BTreeSet::new();

    assert!(
        group
            .provides_with(&none)
            .contains(&Capability::AssignmentConsistentHash)
    );
    assert!(group.exclusion(Capability::AssignmentRoundRobin).is_some());

    let consumer_group = group
        .requirements
        .iter()
        .find(|requirement| requirement.id == "consumer-group")
        .expect("the consumer-group requirement");

    assert!(consumer_group.statement.contains("never a share group"));
}

// ---- Documents --------------------------------------------------------

#[test]
fn realization_documents_name_their_format_and_kind_and_are_byte_identical() {
    let dir = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);
    let model = postgres_and_kafka_checkout();

    let first = document::status(&Status::of(&dir.profile(), &model)).to_json();
    let second = document::status(&Status::of(&dir.profile(), &model)).to_json();

    assert_eq!(first, second);

    let value: serde_json::Value = serde_json::from_str(&first).unwrap();

    assert_eq!(value["format"], 1);
    assert_eq!(value["document"], "realization_status");
    assert_eq!(value["dsl"], 6);
    assert_eq!(value["realization_format"], 1);
    assert_eq!(value["profile"]["catalogue"], "conseqa-standard@1");

    let runtime = value["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|realized| realized["declaration"] == "topic_runtime topic.order_events")
        .unwrap();

    assert_eq!(runtime["state"], "ambiguous");
    assert_eq!(
        runtime["needs"][0]["capability"],
        serde_json::json!({ "grouping": "keyed" })
    );
    assert_eq!(runtime["needs"][0]["sources"][0]["because"], "declares");
    assert_eq!(runtime["valid"][1]["entry"], "sqs.fifo@1");

    let explanation = Explanation::of(
        &dir.profile(),
        &model,
        &key("topic_runtime topic.order_events"),
    )
    .unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&document::explain(&explanation).to_json()).unwrap();

    assert_eq!(value["document"], "realization_explain");
    assert_eq!(value["candidates"][2]["entry"], "sqs.standard@1");
    assert_eq!(value["candidates"][2]["option_sets"], serde_json::json!([]));
}

// ---- Part I §19.15: realization changes ---------------------------------

fn side(name: SideName, workspace: WorkspaceState, profile: Option<Profile>) -> Side {
    Side {
        name,
        source: SideSource::WorkingTree,
        commit: None,
        merge_base: false,
        workspace: Some(workspace),
        realization: profile,
    }
}

fn workspace(model: &Model) -> WorkspaceState {
    WorkspaceState::from_model(model, RunMetadata::new(RunId("realization".to_string())))
}

/// The realization section of a diff between two specs, each read with
/// its own profile.
fn realization_diff(
    base: (&Model, &ProfileDir),
    head: (&Model, &ProfileDir),
) -> (DiffDocument, RealizationChanges) {
    let document = diff(
        &side(SideName::Base, workspace(base.0), Some(base.1.profile())),
        &side(SideName::Head, workspace(head.0), Some(head.1.profile())),
    );

    let realization = document
        .realization
        .clone()
        .expect("a stack on both sides gives a realization section");

    (document, realization)
}

#[test]
fn a_lock_on_a_dynamodb_data_model_makes_it_infeasible_at_the_spec_gate() {
    let base = fixture("video_streaming.yaml");
    let mut head = base.clone();

    // A lock in the transcoder's failure, whose transactions DynamoDB
    // realizes until then: they write with conditions and never read.
    let fail = head
        .operations
        .get_mut(&id("operation.transcode_video"))
        .unwrap()
        .program
        .transaction_mut(&id("tx.transcode_video.fail"))
        .expect("the failure");

    let TransactionStep::Transition(transition) = &fail.steps[0] else {
        panic!("the failure transitions first");
    };

    let target = transition.subject.clone();

    fail.steps.insert(
        0,
        TransactionStep::Lock(Lock {
            target,
            mode: LockMode::Exclusive,
            order: LockOrder::Unspecified,
        }),
    );

    let dir = ProfileDir::new(&["dynamodb@2012.08.10", "kafka@3.7"]);
    let (document, realization) = realization_diff((&base, &dir), (&head, &dir));

    let transcoder = realization
        .declarations
        .iter()
        .find(|change| change.declaration.to_string() == "data_model data.transcoder")
        .expect("the transcoder's realization changed");

    assert_eq!(texts(&transcoder.needs.added), ["lock: exclusive"]);
    assert_eq!(
        transcoder.base.as_ref().map(|base| (
            base.state.as_str(),
            base.entry.as_ref().map(ToString::to_string)
        )),
        Some(("automatic", Some("dynamodb.table@1".to_string())))
    );
    assert_eq!(
        transcoder.head.as_ref().map(|head| head.state.as_str()),
        Some("infeasible")
    );

    let reported = realization
        .unrealizable
        .iter()
        .find(|unrealizable| unrealizable.declaration.to_string() == "data_model data.transcoder")
        .expect("reported unrealizable on head");

    assert_eq!(reported.state, "infeasible");
    assert!(
        reported.gaps[0]
            .missing
            .iter()
            .any(|missing| missing.capability == Capability::LockExclusive)
    );

    assert_eq!(
        document.failed_gates(&[Gate::Infeasible]),
        [Gate::Infeasible]
    );
    assert!(render::render(&document).contains("! infeasible    data_model data.transcoder"));
}

#[test]
fn adding_sqs_to_a_kafka_stack_makes_the_topic_runtime_ambiguous_at_the_spec_gate() {
    let checkout = postgres_and_kafka_checkout();

    let kafka = ProfileDir::new(&["postgres@16", "kafka@3.7"]);
    let both = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);

    let (document, realization) = realization_diff((&checkout, &kafka), (&checkout, &both));

    assert_eq!(realization.stack.added, ["sqs@2012.11.05"]);
    assert!(realization.stack.removed.is_empty());

    let runtime = realization
        .declarations
        .iter()
        .find(|change| change.declaration.to_string() == "topic_runtime topic.order_events")
        .expect("the topic runtime's realization changed");

    assert_eq!(runtime.base.as_ref().unwrap().state, "automatic");
    assert_eq!(runtime.head.as_ref().unwrap().state, "ambiguous");

    assert_eq!(
        realization
            .unrealizable
            .iter()
            .map(|unrealizable| (
                unrealizable.declaration.to_string(),
                unrealizable.state.clone()
            ))
            .collect::<Vec<_>>(),
        [(
            "topic_runtime topic.order_events".to_string(),
            "ambiguous".to_string()
        )]
    );

    assert_eq!(document.failed_gates(&[Gate::Ambiguous]), [Gate::Ambiguous]);
    assert!(document.failed_gates(&[Gate::Infeasible]).is_empty());

    // The topic runtime's own requirement is withdrawn while nothing
    // realizes it.
    assert!(
        realization
            .requirements
            .withdrawn
            .iter()
            .any(|fact| matches!(
                fact,
                Fact::Realization { requirement, .. } if requirement == "stable-partitions"
            ))
    );
}

#[test]
fn choosing_a_keyed_topic_introduces_its_record_key_on_every_producer() {
    let checkout = postgres_and_kafka_checkout();

    let open = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);
    let chosen = ProfileDir::new(&["postgres@16", "kafka@3.7", "sqs@2012.11.05"]);

    chosen.put(
        "choices/topic_runtime/topic.order_events.yaml",
        "format: 1\ndeclaration: topic_runtime topic.order_events\nentry: kafka.keyed-topic@1\n",
    );

    let (document, realization) = realization_diff((&checkout, &open), (&checkout, &chosen));

    let record_keys: Vec<String> = realization
        .requirements
        .introduced
        .iter()
        .filter_map(|fact| match fact {
            Fact::Realization {
                declaration,
                requirement,
                ..
            } if requirement == "record-key" => Some(declaration.to_string()),
            _ => None,
        })
        .collect();

    assert_eq!(
        record_keys,
        [
            "effect_site operation.cancel_order/effect.cancel_order.publish_cancelled",
            "effect_site operation.charge_payment/effect.charge_payment.publish_captured",
            "effect_site operation.charge_payment/effect.charge_payment.publish_failed",
            "effect_site operation.create_order/effect.create_order.publish_created",
            "effect_site operation.reserve_inventory/effect.reserve_inventory.publish_reserved",
            "transition machine.order_lifecycle/transition.order.mark_paid",
        ]
    );
    assert!(realization.requirements.withdrawn.is_empty());
    assert!(
        document
            .failed_gates(&[Gate::Ambiguous, Gate::Infeasible])
            .is_empty()
    );

    let text = render::render(&document);

    assert!(
        text.contains(
            "  ~ topic_runtime topic.order_events  ambiguous -> chosen kafka.keyed-topic@1"
        ),
        "{text}"
    );
    assert!(text.contains("  + requires realization("), "{text}");

    // The document reads back as itself.
    let json = serde_json::to_string_pretty(&document).unwrap();
    let read: DiffDocument = serde_json::from_str(&json).unwrap();

    assert_eq!(serde_json::to_string_pretty(&read).unwrap(), json);
}

#[test]
fn without_a_stack_a_diff_has_no_realization_section_and_no_gate_fails() {
    let checkout = postgres_and_kafka_checkout();

    let document = diff(
        &side(SideName::Base, workspace(&checkout), None),
        &side(SideName::Head, workspace(&checkout), None),
    );

    assert_eq!(document.realization, None);
    assert!(
        document
            .failed_gates(&[Gate::Infeasible, Gate::Ambiguous])
            .is_empty()
    );
    assert!(
        !serde_json::to_string(&document)
            .unwrap()
            .contains("\"realization\"")
    );
}

fn texts<T: ToString>(values: &[T]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}
