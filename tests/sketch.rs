//! Operation sketches compile into programs the checker admits: the
//! five programs of the `shop` fixture, recompiled from sketches as a
//! coordinator would write them, validate and prove what the authored
//! programs proved.

use std::path::PathBuf;

use conseqa::confluence::sketch::{self, OperationSketch, Settled, Symbols};
use conseqa::confluence::{DraftOperation, RunId, RunMetadata, WorkspaceState};
use conseqa::spec::Id;

mod common;

use common::sketches::{
    flash_checkout_sketches, hedged_read_sketches, payment_capture_sketches, shop_sketches,
    tenant_ledger_sketches, transactional_outbox_sketches, video_streaming_sketches,
};

fn id(value: &str) -> Id {
    Id(value.to_string())
}

fn authored() -> conseqa::spec::Model {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop.yaml"),
    )
    .expect("fixture readable");

    conseqa::parser::yaml::parse(&source).expect("fixture parses")
}

/// `shop` with every program recompiled from its sketch.
fn recompiled() -> conseqa::spec::Model {
    let mut model = authored();

    let workspace = WorkspaceState::from_model(&model, RunMetadata::new(RunId("sketch".into())));
    let symbols = Symbols::of(&workspace);

    for (operation, sketch) in shop_sketches() {
        let mut draft: DraftOperation = workspace.operations[&id(operation)].clone();

        draft.sketch = Some(serde_json::from_value::<OperationSketch>(sketch).expect("parses"));

        let program = sketch::compile(&id(operation), &draft, &symbols, &Settled::default())
            .unwrap_or_else(|error| panic!("{operation}: {error}"));

        model
            .operations
            .get_mut(&id(operation))
            .expect("declared")
            .program = program;
    }

    model
}

/// The gold test of the sketch architecture: every program of `shop`
/// compiled from a sketch of a few lines, with no session and no model,
/// validates — and with the authors' own requirements declared on it
/// (the operation-level ones as authored, the serializability keys on
/// the compiled transactions), every obligation is proven: the version
/// protocol for the conflicts over shared stock and orders, keyed
/// commits for retries, and the inspect-then-decide shape for the
/// guarded transitions' result replay.
#[test]
fn shop_compiles_from_sketches_and_proves_what_its_authors_proved() {
    let mut model = recompiled();

    assert_eq!(conseqa::analyzer::validate(&model), Vec::new());

    let authored = authored();

    for (operation, _) in shop_sketches() {
        let keys: Vec<_> = authored.operations[&id(operation)]
            .program
            .transactions()
            .into_iter()
            .flat_map(|(_, transaction)| transaction.requirements.serializability.clone())
            .take(1)
            .collect();

        assert_eq!(keys.len(), 1, "{operation} declared one key");

        let name = operation.strip_prefix("operation.").expect("prefixed");

        model
            .operations
            .get_mut(&id(operation))
            .expect("declared")
            .program
            .transaction_mut(&id(&format!("tx.{name}")))
            .expect("the compiled main transaction")
            .requirements
            .serializability = keys;
    }

    let checked = conseqa::analyzer::verification::verify(&model);

    assert!(checked.all_proven(), "{:#?}", checked.diagnostics());

    let counts = |report: &conseqa::analyzer::verification::VerificationReport| {
        (
            report.idempotency.len(),
            report.result_replay.len(),
            report.recoverability.len(),
        )
    };

    assert_eq!(
        counts(&checked),
        counts(&conseqa::analyzer::verification::verify(&authored)),
        "the same operation-level obligations"
    );
}

/// A sketch that names something the skeleton does not declare does not
/// compile, and says what to fix.
#[test]
fn a_broken_sketch_says_what_is_wrong() {
    let model = authored();
    let workspace = WorkspaceState::from_model(&model, RunMetadata::new(RunId("sketch".into())));
    let symbols = Symbols::of(&workspace);

    for (sketch, says) in [
        (
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_number" } }
            ]}),
            "carries no `order_number`",
        ),
        (
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "transition", "record": "order", "transition": "transition.order.refund",
                  "otherwise": "not_shippable" }
            ]}),
            "declares `transition.order.refund`",
        ),
        (
            serde_json::json!({ "steps": [
                { "kind": "update", "record": "order", "set": ["status"], "from": [] }
            ]}),
            "which no find before it names",
        ),
    ] {
        let mut draft: DraftOperation = workspace.operations[&id("operation.ship_order")].clone();

        draft.sketch = Some(serde_json::from_value(sketch).expect("parses"));

        let error = sketch::compile(
            &id("operation.ship_order"),
            &draft,
            &symbols,
            &Settled::default(),
        )
        .expect_err("does not compile");

        assert!(error.0.contains(says), "{error}");
    }
}

/// The gate compiles a sketch when it is written: a broken one is
/// rejected with what to fix, while its author can still fix it; a good
/// one lands on the interface.
#[test]
fn the_gate_compiles_a_sketch_when_it_is_written() {
    let workspace = WorkspaceState::from_model(&authored(), RunMetadata::new(RunId("gate".into())));

    let interface = |sketch: serde_json::Value| {
        let mut value = workspace.operations[&id("operation.ship_order")].interface();

        value.sketch = Some(serde_json::from_value(sketch).expect("parses"));

        conseqa::confluence::SpecPatch {
            mutations: vec![conseqa::confluence::Mutation::PutOperationInterface {
                operation: id("operation.ship_order"),
                value,
            }],
        }
    };

    let mut broken = workspace.clone();

    let diagnostics = conseqa::confluence::commit::apply_patch(
        &mut broken,
        &interface(serde_json::json!({ "steps": [
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_number" } }
        ]})),
    );

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("does not compile")
                && diagnostic.message.contains("order_number")),
        "{diagnostics:?}"
    );

    let mut sound = workspace.clone();

    let sketch = shop_sketches()
        .into_iter()
        .find(|(operation, _)| *operation == "operation.ship_order")
        .expect("sketched")
        .1;

    let diagnostics = conseqa::confluence::commit::apply_patch(&mut sound, &interface(sketch));

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        sound.operations[&id("operation.ship_order")]
            .sketch
            .is_some()
    );
}

/// The model of `fixture` with every program recompiled from its sketch,
/// and the authors' transaction requirements declared on the compiled
/// transactions.
fn recompile_fixture(
    fixture: &str,
    sketches: Vec<(&'static str, serde_json::Value)>,
) -> (conseqa::spec::Model, conseqa::spec::Model) {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    )
    .expect("fixture readable");

    let authored = conseqa::parser::yaml::parse(&source).expect("fixture parses");
    let mut model = authored.clone();

    let workspace = WorkspaceState::from_model(&model, RunMetadata::new(RunId("sketch".into())));
    let symbols = Symbols::of(&workspace);

    assert_eq!(
        sketches.len(),
        model.operations.len(),
        "{fixture}: every operation is sketched"
    );

    for (operation, sketch) in sketches {
        let mut draft: DraftOperation = workspace.operations[&id(operation)].clone();

        draft.sketch = Some(serde_json::from_value::<OperationSketch>(sketch).expect("parses"));

        let mut program = sketch::compile(&id(operation), &draft, &symbols, &Settled::default())
            .unwrap_or_else(|error| panic!("{fixture} {operation}: {error}"));

        // The authors' transaction requirements, on the compiled
        // transactions in the same order — or all on the first when the
        // shapes differ in number.
        let authored_transactions: Vec<conseqa::spec::TransactionRequirements> = authored
            .operations[&id(operation)]
            .program
            .transactions()
            .into_iter()
            .map(|(_, transaction)| transaction.requirements.clone())
            .collect();

        let compiled: Vec<Id> = program
            .transactions()
            .into_iter()
            .map(|(_, transaction)| transaction.id.clone())
            .collect();

        if compiled.len() == authored_transactions.len() {
            for (transaction, requirements) in compiled.iter().zip(authored_transactions) {
                program
                    .transaction_mut(transaction)
                    .expect("compiled")
                    .requirements = requirements;
            }
        } else {
            let all = authored_transactions.into_iter().fold(
                conseqa::spec::TransactionRequirements::default(),
                |mut all, one| {
                    all.serializability.extend(one.serializability);
                    all.ordering.extend(one.ordering);
                    all
                },
            );

            match compiled.first() {
                Some(first) => {
                    program
                        .transaction_mut(first)
                        .expect("compiled")
                        .requirements = all;
                }
                None => assert!(
                    all.serializability.is_empty() && all.ordering.is_empty(),
                    "{fixture} {operation}: its transaction requirements need a transaction"
                ),
            }
        }

        model
            .operations
            .get_mut(&id(operation))
            .expect("declared")
            .program = program;
    }

    (authored, model)
}

/// The (operation, family) pairs with an unproven obligation.
fn unproven(model: &conseqa::spec::Model) -> std::collections::BTreeSet<(String, String)> {
    let report =
        serde_json::to_value(conseqa::analyzer::verification::verify(model)).expect("serializes");

    let mut open = std::collections::BTreeSet::new();

    for family in [
        "transaction_serializability",
        "transaction_ordering",
        "idempotency",
        "result_replay",
        "recoverability",
    ] {
        for check in report[family].as_array().expect("checks") {
            if check["verdict"]["kind"] != "proven" {
                open.insert((
                    check["operation"]
                        .as_str()
                        .expect("an operation")
                        .to_string(),
                    family.to_string(),
                ));
            }
        }
    }

    open
}

/// Proven obligations, by operation and family.
fn proven(model: &conseqa::spec::Model) -> std::collections::BTreeMap<(String, String), usize> {
    let report =
        serde_json::to_value(conseqa::analyzer::verification::verify(model)).expect("serializes");

    let mut proven = std::collections::BTreeMap::new();

    for family in [
        "transaction_serializability",
        "transaction_ordering",
        "idempotency",
        "result_replay",
        "recoverability",
    ] {
        for check in report[family].as_array().expect("checks") {
            if check["verdict"]["kind"] == "proven" {
                *proven
                    .entry((
                        check["operation"]
                            .as_str()
                            .expect("an operation")
                            .to_string(),
                        family.to_string(),
                    ))
                    .or_insert(0) += 1;
            }
        }
    }

    proven
}

/// A decider that never answers: the pipeline below must not need one.
struct Unavailable;

#[async_trait::async_trait]
impl conseqa::system_one::Decider for Unavailable {
    fn identity(&self) -> conseqa::system_one::DeciderIdentity {
        conseqa::system_one::DeciderIdentity {
            backend: "unavailable".to_string(),
            model: "none".to_string(),
            endpoint: None,
            calibrated: None,
        }
    }

    async fn decide(
        &self,
        _request: &conseqa::system_one::DecisionRequest,
    ) -> Result<conseqa::system_one::Decision, conseqa::system_one::DeciderError> {
        Err(conseqa::system_one::DeciderError::Unavailable {
            attempts: 1,
            last: "no decider in this test".to_string(),
        })
    }
}

/// An agent backend that records every session it is asked for and
/// writes nothing.
#[derive(Default, Clone)]
struct NoSession {
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl conseqa::harness::backend::AgentBackend for NoSession {
    fn name(&self) -> &str {
        "no-session"
    }

    async fn run(
        &self,
        invocation: conseqa::harness::backend::AgentInvocation,
        _handle: conseqa::harness::backend::AgentHandle,
        _events: conseqa::harness::backend::AgentEventSink,
    ) -> Result<conseqa::harness::backend::AgentExit, conseqa::harness::backend::AgentBackendError>
    {
        self.asked.lock().expect("not poisoned").push(format!(
            "{}: {}",
            invocation.kind,
            invocation.prompt.lines().last().unwrap_or("")
        ));

        Ok(conseqa::harness::backend::AgentExit {
            status: conseqa::harness::backend::AgentExitStatus::Completed,
            session: None,
            final_message: None,
            usage: Default::default(),
            backend: conseqa::confluence::AgentBackendMetadata {
                name: "no-session".to_string(),
                version: None,
                session: None,
            },
            escalation: None,
        })
    }
}

/// The model the workflow ends with, from `compiled` — its programs
/// compiled from sketches and the authors' requirements declared on
/// them — with System One builders and no session: what repair settles
/// in process is settled.
async fn through_the_pipeline(
    compiled: &conseqa::spec::Model,
) -> (conseqa::spec::Model, Vec<String>) {
    use std::sync::Arc;

    let mut run_meta = RunMetadata::new(RunId("sketch-pipeline".into()));

    run_meta.prompt = Some("A system.".to_string());
    run_meta.policy = conseqa::confluence::RunPolicy {
        strict_requirements: false,
        adopt_recommended: false,
    };

    let engine = conseqa::confluence::ConfluenceEngine::in_memory(WorkspaceState::from_model(
        compiled, run_meta,
    ))
    .expect("engine starts");

    let sessions = NoSession::default();

    let backend = Arc::new(conseqa::harness::executors::SystemOneBackend::new(
        engine.clone(),
        Arc::new(Unavailable),
        conseqa::harness::executors::BUILDABLE,
        Arc::new(sessions.clone()),
    ));

    let out_dir = std::env::temp_dir().join(format!("conseqa-sketch-{}", uuid::Uuid::new_v4()));

    let supervisor = conseqa::harness::Supervisor::new(
        engine.clone(),
        backend,
        "http://127.0.0.1:0/mcp",
        None,
        out_dir.join("work"),
    );

    let scheduler = conseqa::harness::Scheduler::new(
        engine.clone(),
        supervisor,
        conseqa::harness::SchedulerPolicy::default(),
    );

    let workflow = conseqa::harness::Workflow::new(
        scheduler,
        conseqa::harness::WorkflowConfig {
            out_dir: out_dir.clone(),
            analysis_timeout: std::time::Duration::from_secs(20),
            max_iterations: 8,
            objective: None,
        },
    );

    workflow.run().await.expect("the workflow runs");

    std::fs::remove_dir_all(&out_dir).ok();

    let model = engine
        .head_snapshot()
        .workspace
        .assemble_model()
        .expect("assembles");

    let asked = sessions.asked.lock().expect("not poisoned").clone();

    (model, asked)
}

/// The vocabulary's coverage: every program of `tenant_ledger` and
/// `flash_checkout` — outbox admissions, relays, publications executed
/// after commit, cursors, transition side effects, external calls with
/// result matching, composite identities — compiles from a sketch,
/// validates, and proves at least every obligation its authors' program
/// proved.
#[tokio::test]
async fn every_fixture_compiles_from_sketches_and_proves_what_its_authors_proved() {
    for (fixture, sketches) in [
        ("tenant_ledger.yaml", tenant_ledger_sketches()),
        ("flash_checkout.yaml", flash_checkout_sketches()),
        ("shop.yaml", shop_sketches()),
        ("transactional_outbox.yaml", transactional_outbox_sketches()),
        ("payment_capture.yaml", payment_capture_sketches()),
        ("video_streaming.yaml", video_streaming_sketches()),
        ("hedged_read.yaml", hedged_read_sketches()),
    ] {
        let (authored, compiled) = recompile_fixture(fixture, sketches);

        let errors = conseqa::analyzer::validate(&compiled);

        assert!(errors.is_empty(), "{fixture}: {errors:#?}");

        let (settled, sessions) = through_the_pipeline(&compiled).await;

        // A session is only ever a repair of an obligation the authors
        // left unproven too: what no program change can prove (an
        // external call that is distinguishable on retry, say) is the
        // session's to judge, sketch or not.
        let open = unproven(&authored);

        for session in &sessions {
            assert!(
                session.starts_with("requirement_repair"),
                "{fixture}: only repair reaches a session: {session}"
            );

            let listed = session
                .split("unproven: ")
                .nth(1)
                .unwrap_or_else(|| panic!("{fixture}: names what is unproven: {session}"));

            for reference in listed.split(", ") {
                let family = reference.split(' ').next().expect("a family");
                let operation = reference
                    .rsplit(' ')
                    .next()
                    .expect("an operation")
                    .to_string();

                assert!(
                    open.contains(&(operation.clone(), family.to_string())),
                    "{fixture}: a session for {reference}, which the authors proved"
                );
            }
        }

        let before = proven(&authored);
        let after = proven(&settled);

        for (key, count) in &before {
            assert!(
                after.get(key).copied().unwrap_or(0) >= *count,
                "{fixture}: {key:?} proved {count} as authored, {} compiled",
                after.get(key).copied().unwrap_or(0)
            );
        }
    }
}

/// Compiles `sketch` for `operation` of `fixture`, swaps it in, and
/// returns the validation errors of the whole model.
fn compile_into(
    fixture: &str,
    operation: &str,
    sketch: serde_json::Value,
) -> (conseqa::spec::OperationBlock, Vec<String>) {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    )
    .expect("fixture readable");

    let mut model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    let workspace = WorkspaceState::from_model(&model, RunMetadata::new(RunId("general".into())));
    let symbols = Symbols::of(&workspace);

    let mut draft: DraftOperation = workspace.operations[&id(operation)].clone();

    draft.sketch = Some(serde_json::from_value::<OperationSketch>(sketch).expect("parses"));

    let program = sketch::compile(&id(operation), &draft, &symbols, &Settled::default())
        .unwrap_or_else(|error| panic!("{operation}: {error}"));

    model
        .operations
        .get_mut(&id(operation))
        .expect("declared")
        .program = program.clone();

    let errors = conseqa::analyzer::validate(&model)
        .iter()
        .map(|error| format!("{error:?}"))
        .collect();

    (program, errors)
}

fn kinds(block: &conseqa::spec::OperationBlock) -> Vec<String> {
    block
        .steps
        .iter()
        .map(|step| {
            serde_json::to_value(step).expect("serializes")["kind"]
                .as_str()
                .expect("a kind")
                .to_string()
        })
        .collect()
}

/// The general path: what the fixtures do not happen to exercise —
/// deletes, several transitions, decisions on found records with a
/// declared refusal, requests into other operations, parallel effects,
/// and one operation over two data models — compiles into programs the
/// validator admits.
#[test]
fn the_general_vocabulary_compiles_into_valid_programs() {
    // A decision on a record found in an earlier transaction: it is
    // exported, branched on, and refused with a declared error.
    let (program, errors) = compile_into(
        "shop.yaml",
        "operation.cancel_order",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "when", "if": { "equals": ["order.status", "state.order.paid"] },
              "then": [ { "kind": "reject", "error": "not_cancellable" } ] },
            { "kind": "find", "as": "pending", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "transition", "record": "pending", "transition": "transition.order.cancel",
              "otherwise": "not_cancellable" },
            { "kind": "find", "as": "product", "record": "object.product",
              "by": { "product_id": "pending.product_id" } },
            { "kind": "update", "record": "product", "set": ["stock"],
              "from": ["product.stock", "pending.quantity"] }
        ]}),
    );

    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(
        kinds(&program),
        ["transaction", "branch", "transaction", "return"]
    );

    // Two transitions in one transaction, and a delete.
    let (_, errors) = compile_into(
        "shop.yaml",
        "operation.ship_order",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "transition", "record": "order", "transition": "transition.order.pay",
              "otherwise": "not_shippable" },
            { "kind": "transition", "record": "order", "transition": "transition.order.ship",
              "otherwise": "not_shippable" },
            { "kind": "find", "as": "payment", "record": "object.payment",
              "by": { "order_id": "input.order_id" } },
            { "kind": "delete", "record": "payment" }
        ]}),
    );

    assert!(errors.is_empty(), "{errors:#?}");

    // A request into another operation, acting on its refusal.
    let (program, errors) = compile_into(
        "shop.yaml",
        "operation.ship_order",
        serde_json::json!({ "steps": [
            { "kind": "request", "operation": "operation.pay_order", "as": "paid",
              "retry": "may_repeat",
              "from": ["input.request_id", "input.order_id"],
              "on_error": {
                "not_payable": [ { "kind": "reject", "error": "not_shippable" } ],
                "order_not_found": [ { "kind": "reject", "error": "order_not_found" } ] } },
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "transition", "record": "order", "transition": "transition.order.ship",
              "otherwise": "not_shippable" }
        ]}),
    );

    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(kinds(&program)[..2], ["execute_effect", "match_result"]);

    // Effects started together and awaited together.
    let (program, errors) = compile_into(
        "flash_checkout.yaml",
        "operation.charge_payment",
        serde_json::json!({ "steps": [
            { "kind": "parallel", "steps": [
                { "kind": "publish", "topic": "topic.order_events",
                  "schema": "schema.PaymentCaptured",
                  "from": ["input.event_id", "input.order_id", "input.amount"] },
                { "kind": "call", "name": "fraud-check", "from": ["input.order_id"] } ] }
        ]}),
    );

    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(
        kinds(&program),
        [
            "execute_effect_async",
            "execute_effect_async",
            "join_all",
            "complete"
        ]
    );

    // One operation over two data models: a transaction each, in order.
    let (program, errors) = compile_into(
        "flash_checkout.yaml",
        "operation.create_order",
        serde_json::json!({ "steps": [
            { "kind": "create", "record": "object.order",
              "from": ["input.order_id", "input.amount"] },
            { "kind": "find", "as": "stock", "record": "object.stock",
              "by": { "warehouse_id": "input.warehouse_id", "sku": "input.sku" } },
            { "kind": "update", "record": "stock", "set": ["reserved"],
              "from": ["stock.reserved", "input.quantity"] }
        ]}),
    );

    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(program.transactions().len(), 2);
}

/// [`compile_into`], with the fixture's model edited first.
fn compile_edited(
    fixture: &str,
    edit: impl FnOnce(&mut conseqa::spec::Model),
    operation: &str,
    sketch: serde_json::Value,
) -> Result<(conseqa::spec::OperationBlock, Vec<String>), String> {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    )
    .expect("fixture readable");

    let mut model = conseqa::parser::yaml::parse(&source).expect("fixture parses");

    edit(&mut model);

    let workspace = WorkspaceState::from_model(&model, RunMetadata::new(RunId("edited".into())));
    let symbols = Symbols::of(&workspace);

    let mut draft: DraftOperation = workspace.operations[&id(operation)].clone();

    draft.sketch = Some(serde_json::from_value::<OperationSketch>(sketch).expect("parses"));

    let program = sketch::compile(&id(operation), &draft, &symbols, &Settled::default())
        .map_err(|error| error.0)?;

    model
        .operations
        .get_mut(&id(operation))
        .expect("declared")
        .program = program.clone();

    let errors = conseqa::analyzer::validate(&model)
        .iter()
        .map(|error| format!("{error:?}"))
        .collect();

    Ok((program, errors))
}

thread_local! {
    /// Every `kind` the programs compiled on this thread contain.
    static PRODUCED: std::cell::RefCell<std::collections::BTreeSet<String>> =
        std::cell::RefCell::default();
}

fn compiled(
    fixture: &str,
    edit: impl FnOnce(&mut conseqa::spec::Model),
    operation: &str,
    sketch: serde_json::Value,
) -> conseqa::spec::OperationBlock {
    let (program, errors) = compile_edited(fixture, edit, operation, sketch)
        .unwrap_or_else(|error| panic!("{operation}: {error}"));

    assert!(errors.is_empty(), "{operation}: {errors:#?}");

    PRODUCED.with(|produced| produced.borrow_mut().extend(every_kind(&program)));

    program
}

fn every_kind(block: &conseqa::spec::OperationBlock) -> Vec<String> {
    let mut kinds = Vec::new();

    fn walk(value: &serde_json::Value, kinds: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(kind)) = map.get("kind") {
                    kinds.push(kind.clone());
                }
                for nested in map.values() {
                    walk(nested, kinds);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, kinds);
                }
            }
            _ => {}
        }
    }

    walk(
        &serde_json::to_value(block).expect("serializes"),
        &mut kinds,
    );

    kinds
}

fn none(_: &mut conseqa::spec::Model) {}

/// Every DSL construct is reachable from a sketch: what the general
/// vocabulary test does not cover — transition outbox writes, durable
/// and detached effects, answers bound by a parallel, whole-scope and
/// locked selections with literals, declared isolation and commit keys,
/// several message schemas, several inputs, and field-mapped values —
/// compiles into programs the validator admits.
#[test]
fn every_dsl_construct_is_reachable_from_a_sketch() {
    // A transition that admits an outbox message: its derivation comes
    // from the transition step.
    let program = compiled(
        "flash_checkout.yaml",
        |model| {
            let checkout = model
                .data_models
                .get_mut(&id("data.checkout"))
                .expect("declared");

            checkout.outboxes.insert(
                id("outbox.checkout_events"),
                serde_json::from_value(serde_json::json!({
                    "messages": ["schema.OrderCancelled"],
                    "message_identity": { "kind": "keyed",
                        "mapping": { "schema.OrderCancelled": [["event_id"]] } }
                }))
                .expect("an outbox"),
            );

            // Its exclusive consumer, which relays nothing further.
            let mut relay = model.operations[&id("operation.charge_payment")].clone();

            relay.inputs = [(
                id("input.relay_checkout.outbox"),
                serde_json::from_value(serde_json::json!({
                    "kind": "outbox", "outbox": "outbox.checkout_events"
                }))
                .expect("an outbox input"),
            )]
            .into();

            relay.program = serde_json::from_value(serde_json::json!({
                "steps": [ { "kind": "complete" } ]
            }))
            .expect("a program");

            relay.requirements = Default::default();

            model
                .operations
                .insert(id("operation.relay_checkout"), relay);

            model
                .state_machines
                .get_mut(&id("machine.order_lifecycle"))
                .expect("declared")
                .transitions
                .get_mut(&id("transition.order.cancel"))
                .expect("declared")
                .effects
                .insert(
                    id("effect.order.cancelled_event"),
                    serde_json::from_value(serde_json::json!({
                        "kind": "outbox_write", "outbox": "outbox.checkout_events",
                        "schema": "schema.OrderCancelled", "idempotency_key_propagation": []
                    }))
                    .expect("an outbox write"),
                );
        },
        "operation.cancel_order",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "transition", "record": "order", "transition": "transition.order.cancel",
              "otherwise": "not_pending", "effects_from": ["input.order_id"] }
        ]}),
    );

    assert!(
        serde_json::to_string(&program)
            .expect("serializes")
            .contains("effect.order.cancelled_event")
    );

    // A durable call: arranged in the transaction, made after it, and
    // acted on.
    let program = compiled(
        "shop.yaml",
        none,
        "operation.place_order",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "product", "record": "object.product",
              "by": { "product_id": "input.product_id" } },
            { "kind": "update", "record": "product", "set": ["stock"],
              "from": ["product.stock", "input.quantity"] },
            { "kind": "call", "name": "payment-gateway.authorize", "as": "auth",
              "durable": true, "duplicates": "identical_per_identity",
              "identity": ["input.request_id"],
              "result": { "ok": "schema.PlaceOrderResponse",
                          "errors": { "declined": { "schema": "schema.ProductNotFound",
                                                    "disposition": "terminal" } } },
              "from": ["input.request_id", "product.stock"],
              "on_error": { "declined": [ { "kind": "reject", "error": "insufficient_stock" } ] } }
        ]}),
    );

    let seen = every_kind(&program);

    assert!(
        seen.contains(&"establish_effect_intent".to_string()),
        "{seen:?}"
    );
    assert!(
        seen.contains(&"execute_effect_intent".to_string()),
        "{seen:?}"
    );
    assert!(seen.contains(&"match_result".to_string()), "{seen:?}");

    // Detached effects: a publication after the commit, not waited for,
    // and a direct call left in flight.
    let program = compiled(
        "flash_checkout.yaml",
        none,
        "operation.cancel_order",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "transition", "record": "order", "transition": "transition.order.cancel",
              "otherwise": "not_pending" },
            { "kind": "publish", "topic": "topic.order_events", "schema": "schema.OrderCancelled",
              "from": ["input.order_id"], "detached": true },
            { "kind": "call", "name": "audit-log", "from": ["input.order_id"], "detached": true }
        ]}),
    );

    let seen = every_kind(&program);

    assert!(
        seen.contains(&"execute_effect_intent_async".to_string()),
        "{seen:?}"
    );
    assert!(
        seen.contains(&"execute_effect_async".to_string()),
        "{seen:?}"
    );

    // Answers bound by a parallel, acted on after the join.
    let program = compiled(
        "flash_checkout.yaml",
        none,
        "operation.charge_payment",
        serde_json::json!({ "steps": [
            { "kind": "parallel", "steps": [
                { "kind": "call", "name": "fraud-check", "as": "fraud",
                  "result": { "ok": "schema.ChargeAccepted",
                              "errors": { "declined": "schema.ChargeDeclined" } },
                  "from": ["input.order_id"] },
                { "kind": "call", "name": "risk-score", "from": ["input.order_id"] } ] },
            { "kind": "answer", "of": "fraud",
              "on_error": { "declined": [
                { "kind": "publish", "topic": "topic.order_events",
                  "schema": "schema.PaymentFailed",
                  "from": ["input.event_id", "input.order_id", "fraud.reason"] } ] } }
        ]}),
    );

    assert_eq!(
        kinds(&program),
        [
            "execute_effect_async",
            "execute_effect_async",
            "join_all",
            "match_result",
            "complete"
        ]
    );

    // Whole-scope and locked selections, with literal values.
    let program = compiled(
        "shop.yaml",
        none,
        "operation.restock",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "every", "record": "object.product", "all": true,
              "lock": "shared",
              "lock_order": [ { "field": "product_id", "direction": "ascending" } ] },
            { "kind": "find", "as": "product", "record": "object.product",
              "by": { "product_id": "input.product_id", "stock": 0 } },
            { "kind": "update", "record": "product", "from": { "stock": "input.quantity" } }
        ]}),
    );

    let text = serde_json::to_string(&program).expect("serializes");

    assert!(
        text.contains("\"shared\"") && text.contains("\"ascending\""),
        "{text}"
    );
    assert!(text.contains("\"kind\":\"all\""), "{text}");
    assert!(text.contains("\"literal\""), "{text}");

    // Declared isolation and an explicit (absent) commit key.
    let program = compiled(
        "shop.yaml",
        none,
        "operation.restock",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "product", "record": "object.product",
              "by": { "product_id": "input.product_id" } },
            { "kind": "update", "record": "product", "set": ["stock"],
              "from": ["product.stock", "input.quantity"] }
        ], "isolation": "serializable", "commit_key": [] }),
    );

    let transaction = program.transactions()[0].1.clone();

    assert_eq!(
        transaction.isolation,
        conseqa::spec::TransactionIsolation::Serializable
    );
    assert_eq!(
        transaction.idempotency,
        conseqa::spec::IdempotencyGuarantee::NotDeduplicated
    );

    // A subscription to several message schemas: the fields they share,
    // and the identity they share.
    let program = compiled(
        "flash_checkout.yaml",
        |model| {
            let input = model
                .operations
                .get_mut(&id("operation.charge_payment"))
                .expect("declared")
                .inputs
                .get_mut(&id("input.charge_payment.reserved"))
                .expect("declared");

            if let conseqa::spec::Input::Subscription(subscription) = input {
                subscription.messages = conseqa::spec::MessageSelector::Only(
                    [id("schema.PaymentCaptured"), id("schema.PaymentFailed")].into(),
                );
            }
        },
        "operation.charge_payment",
        serde_json::json!({ "steps": [
            { "kind": "publish", "topic": "topic.order_events", "schema": "schema.OrderPaid",
              "from": ["input.event_id", "input.order_id"] }
        ]}),
    );

    assert!(
        serde_json::to_string(&program)
            .expect("serializes")
            .contains("idempotency_key_propagation\":[{"),
        "the shared identity propagates"
    );

    // Several inputs: values name their input, and the result names the
    // request it is for.
    let program = compiled(
        "shop.yaml",
        |model| {
            let operation = model
                .operations
                .get_mut(&id("operation.restock"))
                .expect("declared");

            let first = operation.inputs[&id("input.restock.request")].clone();

            operation.inputs.insert(id("input.restock.bulk"), first);
        },
        "operation.restock",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "product", "record": "object.product",
              "by": { "product_id": "input.restock.request.product_id" } },
            { "kind": "update", "record": "product", "set": ["stock"],
              "from": ["product.stock", "input.restock.request.quantity"] }
        ], "returns_for": "input.restock.request",
           "returns": ["input.restock.request.product_id", "input.restock.request.request_id"] }),
    );

    assert_eq!(program.transactions().len(), 1);

    // A durable call and a durable publication with no record before
    // them: arranged in an artifact-only transaction of no data model.
    let program = compiled(
        "flash_checkout.yaml",
        none,
        "operation.charge_payment",
        serde_json::json!({ "steps": [
            { "kind": "call", "name": "payment-provider.charge", "as": "charge", "durable": true,
              "duplicates": "distinguishable",
              "result": { "ok": "schema.ChargeAccepted",
                          "errors": { "declined": "schema.ChargeDeclined" } },
              "on_error": { "declined": [
                { "kind": "publish", "topic": "topic.order_events",
                  "schema": "schema.PaymentFailed", "durable": true,
                  "from": ["input.event_id", "input.order_id", "charge.reason"] } ] } }
        ]}),
    );

    let transactions = program.transactions();

    assert!(
        transactions
            .iter()
            .all(|(_, transaction)| transaction.data_model.is_none())
    );
    assert_eq!(transactions.len(), 2);

    // A decision the DSL states no fact about.
    let program = compiled(
        "flash_checkout.yaml",
        none,
        "operation.charge_payment",
        serde_json::json!({ "steps": [
            { "kind": "when", "if": { "unspecified": "the order looks risky" },
              "then": [ { "kind": "call", "name": "manual-review", "from": ["input.order_id"] } ] }
        ]}),
    );

    assert!(
        serde_json::to_string(&program)
            .expect("serializes")
            .contains("\"unspecified\"")
    );

    // A stale position refused with a declared error, and what a
    // version conflict does instead of completing.
    let program = compiled(
        "tenant_ledger.yaml",
        none,
        "operation.post_entry",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "tenant", "record": "object.tenant",
              "by": { "tenant_id": "input.tenant_id" } },
            { "kind": "advance", "record": "tenant", "field": "last_sequence",
              "to": "tenant.last_sequence", "rule": "monotonic_after", "otherwise": "rejected" }
        ], "returns": ["input.entry_id", "input.entry_id"] }),
    );

    assert!(
        serde_json::to_string(&program)
            .expect("serializes")
            .contains("\"err\"")
    );

    let program = compiled(
        "shop.yaml",
        none,
        "operation.restock",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "product", "record": "object.product",
              "by": { "product_id": "input.product_id" } },
            { "kind": "update", "record": "product", "set": ["stock"],
              "from": ["product.stock", "input.quantity"] }
        ], "on_rejected": [ { "kind": "reject", "error": "product_not_found" } ] }),
    );

    let conseqa::spec::OperationStep::Transaction(execute) = &program.steps[0] else {
        panic!("a transaction first: {program:?}");
    };

    assert!(matches!(
        execute.rejected.as_ref().map(|block| &block.steps[..]),
        Some([conseqa::spec::OperationStep::Return(_)])
    ));

    // Each transaction's own isolation, and a message keyed by
    // something other than the trigger.
    let program = compiled(
        "flash_checkout.yaml",
        none,
        "operation.create_order",
        serde_json::json!({ "steps": [
            { "kind": "create", "record": "object.order", "isolation": "serializable",
              "from": ["input.order_id", "input.amount"] },
            { "kind": "find", "as": "stock", "record": "object.stock", "isolation": "snapshot",
              "by": { "warehouse_id": "input.warehouse_id", "sku": "input.sku" } },
            { "kind": "update", "record": "stock", "set": ["reserved"],
              "from": ["stock.reserved", "input.quantity"] },
            { "kind": "publish", "topic": "topic.order_events", "schema": "schema.OrderCreated",
              "key": ["input.order_id"], "from": ["input.order_id"] }
        ]}),
    );

    let isolations: Vec<conseqa::spec::TransactionIsolation> = program
        .transactions()
        .iter()
        .map(|(_, transaction)| transaction.isolation)
        .collect();

    assert_eq!(
        isolations,
        [
            conseqa::spec::TransactionIsolation::Serializable,
            conseqa::spec::TransactionIsolation::Snapshot
        ]
    );

    let text = serde_json::to_string(&program).expect("serializes");

    assert!(
        text.contains("\"source\":{\"components\":[{\"source\":{\"kind\":\"input\",\"id\":\"input.create_order.request\"},\"path\":[\"order_id\"]}]}"),
        "{text}"
    );

    // The rest of the vocabulary, for the coverage check below.
    compiled(
        "tenant_ledger.yaml",
        none,
        "operation.post_entry",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "tenant", "record": "object.tenant",
              "by": { "tenant_id": "input.tenant_id" } },
            { "kind": "fence", "record": "tenant", "field": "last_sequence",
              "token": "tenant.last_sequence", "otherwise": "rejected" },
            { "kind": "create", "record": "object.entry",
              "from": ["input.tenant_id", "input.entry_id", "tenant.last_sequence", "input.amount"] },
            { "kind": "enqueue", "outbox": "outbox.tenant_events", "schema": "schema.EntryPosted",
              "from": ["input.request_id", "input.tenant_id", "input.entry_id", "input.amount"] }
        ], "returns": ["input.entry_id", "tenant.last_sequence"] }),
    );

    compiled(
        "tenant_ledger.yaml",
        none,
        "operation.apply_entry",
        serde_json::json!({ "steps": [
            { "kind": "find", "as": "ledger", "record": "object.tenant_ledger",
              "by": { "tenant_id": "input.tenant_id" } },
            { "kind": "advance", "record": "ledger", "field": "last_applied_sequence",
              "to": "input.sequence", "rule": "successor" }
        ]}),
    );

    compiled(
        "shop.yaml",
        none,
        "operation.ship_order",
        serde_json::json!({ "steps": [
            { "kind": "request", "operation": "operation.pay_order", "as": "paid",
              "from": ["input.request_id", "input.order_id"],
              "on_error": {
                "not_payable": [ { "kind": "reject", "error": "not_shippable" } ],
                "order_not_found": [ { "kind": "reject", "error": "order_not_found" } ] } },
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id" } },
            { "kind": "when", "if": { "any": [
                { "present": "order.status" },
                { "all": [ { "not": { "equals": ["order.status", "state.order.paid"] } } ] } ] },
              "then": [ { "kind": "reject", "error": "not_shippable" } ] },
            { "kind": "find", "as": "payment", "record": "object.payment",
              "by": { "order_id": "input.order_id" } },
            { "kind": "delete", "record": "payment" }
        ]}),
    );

    compiled(
        "hedged_read.yaml",
        none,
        "operation.hedged_read",
        serde_json::json!({ "steps": [
            { "kind": "race", "as": "read",
              "steps": [
                { "kind": "call", "name": "store-a",
                  "result": { "ok": "schema.Row", "errors": { "miss": "schema.Miss" } },
                  "from": ["input.id"] },
                { "kind": "call", "name": "store-b",
                  "result": { "ok": "schema.Row", "errors": { "miss": "schema.Miss" } },
                  "from": ["input.id"] } ],
              "on_error": { "miss": [ { "kind": "reject", "error": "miss" } ] } }
        ], "returns": ["read.id"] }),
    );

    // Every kind of operation step, transaction step, effect, selector
    // predicate and condition the DSL has was produced from a sketch.
    let produced = PRODUCED.with(|produced| produced.borrow().clone());

    for kind in [
        // operation steps
        "transaction",
        "execute_effect",
        "execute_effect_async",
        "execute_effect_intent",
        "execute_effect_intent_async",
        "join_all",
        "race",
        "match_result",
        "branch",
        "return",
        "complete",
        // transaction steps
        "read",
        "write",
        "insert",
        "delete",
        "lock",
        "transition",
        "establish_effect_intent",
        "establish_transaction_output",
        "write_outbox",
        "validate_version",
        "bump_version",
        "advance_cursor",
        "fence",
        // effects
        "publication",
        "request",
        "external",
        // (an outbox write is `write_outbox` in a program; a transition's
        // own is asserted by its effect id above)
        // selector predicates and conditions
        "all",
        "eq",
        "and",
        "not",
        "present",
        "unspecified",
        // values and outcomes
        "literal",
        "value",
        "deterministic",
        "ok",
        "err",
    ] {
        assert!(
            produced.contains(kind),
            "no sketch produced `{kind}`: {produced:?}"
        );
    }

    // Field-mapped values are checked against the target's schema.
    let error = compile_edited(
        "shop.yaml",
        none,
        "operation.restock",
        serde_json::json!({ "steps": [
            { "kind": "create", "record": "object.restock_receipt",
              "from": { "request_id": "input.request_id", "units": "input.quantity" } }
        ]}),
    )
    .expect_err("an unknown target field");

    assert!(error.contains("no field `units`"), "{error}");
}
