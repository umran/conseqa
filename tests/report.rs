use std::{
    fs,
    path::{Path, PathBuf},
};

use conseqa::{
    analyzer::{
        report::{self, Property, ProverReport, Status},
        verification,
    },
    parser::yaml,
    spec::Model,
};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load_flash_checkout() -> Model {
    let path = fixture_path("flash_checkout.yaml");

    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture `{}`: {error}", path.display()));

    yaml::parse(&source).expect("flash checkout fixture should parse")
}

#[test]
fn scaffold_enumerates_requirement_obligations() {
    let model = load_flash_checkout();
    let report = report::scaffold(&model);

    assert_eq!(report.format, report::FORMAT);
    assert_eq!(report.format, 5);
    assert_eq!(report.dsl, Some(conseqa::spec::DSL_VERSION));
    assert_eq!(report.model_revision, Some(1));

    // 3 serialization + 3 ordering + 4 idempotency + 1 result replay
    // + 3 recoverability.
    assert_eq!(report.obligations.len(), 14);

    assert!(
        report
            .obligations
            .iter()
            .all(|obligation| obligation.status == Status::Unknown)
    );

    assert_eq!(
        report
            .obligations
            .iter()
            .filter(|obligation| matches!(obligation.property, Property::ResultReplay))
            .count(),
        1
    );
}

#[test]
fn scaffold_round_trips_through_json() {
    let report = report::scaffold(&load_flash_checkout());

    let json = serde_json::to_string_pretty(&report).expect("serializes");
    let parsed: ProverReport = serde_json::from_str(&json).expect("deserializes");

    assert_eq!(parsed, report);
}

#[test]
fn obligations_carry_real_verdicts() {
    let model = load_flash_checkout();

    let report = report::obligations(&model, &verification::verify(&model));

    assert_eq!(report.obligations.len(), 14);

    let count = |status: Status| {
        report
            .obligations
            .iter()
            .filter(|obligation| obligation.status == status)
            .count()
    };

    // 3 serialization + 3 ordering + apply_payment idempotency +
    // create_order result replay + create_order/apply_payment
    // recoverability. create_order's idempotency is unknown through its
    // cascade: reserve_inventory consumes OrderCreated and is itself
    // unproven.
    assert_eq!(count(Status::Proven), 10);
    assert_eq!(count(Status::Unknown), 4);
    assert_eq!(count(Status::Disproven), 0);

    // Proven obligations state the facts their proofs rely on;
    // unproven ones carry the checker's evidence.
    for obligation in &report.obligations {
        match obligation.status {
            Status::Proven => assert!(
                !obligation.assumptions.is_empty(),
                "proven obligation `{}` should cite assumptions",
                obligation.id
            ),

            Status::Unknown => assert!(
                !obligation.evidence.is_empty(),
                "unknown obligation `{}` should carry evidence",
                obligation.id
            ),

            Status::Disproven => unreachable!(),
        }
    }

    // The card-charge gap surfaces on charge_payment's idempotency,
    // next to the branch on the provider's result.
    let charge = report
        .obligations
        .iter()
        .find(|obligation| obligation.id == "oblig.operation.charge_payment.idempotency.0")
        .expect("charge_payment idempotency obligation exists");

    assert_eq!(charge.status, Status::Unknown);

    assert!(
        charge
            .evidence
            .iter()
            .any(|evidence| evidence.message.contains("distinguishable"))
    );

    assert!(
        charge
            .evidence
            .iter()
            .any(|evidence| evidence.message.contains("not established to replay"))
    );

    // The result-replay proof names the path facts it rests on.
    let created = report
        .obligations
        .iter()
        .find(|obligation| obligation.id == "oblig.operation.create_order.result_replay.0")
        .expect("create_order result replay obligation exists");

    assert_eq!(created.status, Status::Proven);

    assert!(created.assumptions.iter().any(|assumption| {
        assumption.contains("recovered from tx.create_order.new's keyed commit")
    }));
}

#[test]
fn fixture_report_matches_the_checker() {
    let model = load_flash_checkout();

    let expected = report::obligations(&model, &verification::verify(&model));

    let path = fixture_path("flash_checkout.report.json");

    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read `{}`: {error}", path.display()));

    let stored: ProverReport = serde_json::from_str(&raw).expect("fixture report parses");

    assert_eq!(
        stored, expected,
        "tests/fixtures/flash_checkout.report.json is stale; regenerate it \
         with `cargo run --bin conseqa -- tests/fixtures/flash_checkout.yaml \
         --report tests/fixtures/flash_checkout.report.json`"
    );
}
