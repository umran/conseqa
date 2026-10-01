//! Operation sketches compile into programs the checker admits: the
//! five programs of the `shop` fixture, recompiled from sketches as a
//! coordinator would write them, validate and prove what the authored
//! programs proved.

use std::path::PathBuf;

use conseqa::confluence::sketch::{self, OperationSketch, Settled, Symbols};
use conseqa::confluence::{DraftOperation, RunId, RunMetadata, WorkspaceState};
use conseqa::spec::Id;

mod common;

use common::sketches::shop_sketches;

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
