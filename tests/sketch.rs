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
