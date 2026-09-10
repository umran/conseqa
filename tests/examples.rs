//! The worked example models must stay valid and keep every obligation
//! proven, except where the model deliberately leaves the checker an
//! honest gap — and then exactly that gap, for exactly that reason.

use std::{
    fs,
    path::{Path, PathBuf},
};

use conseqa::{
    analyzer::{
        report::{self, Status},
        validation,
        verification::{self, IdempotencyVerdict},
    },
    parser::yaml,
    spec::{Id, Model},
};

fn load(name: &str) -> Model {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);

    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read `{}`: {error}", path.display()));

    yaml::parse(&source).unwrap_or_else(|error| panic!("`{name}` should parse: {error}"))
}

#[test]
fn video_streaming_example_is_valid() {
    let model = load("video_streaming.yaml");

    let errors = validation::validate(&model);

    assert!(
        errors.is_empty(),
        "video streaming example should validate:\n{errors:#?}"
    );
}

#[test]
fn video_streaming_example_proves_everything() {
    let model = load("video_streaming.yaml");

    let verification = verification::verify(&model);

    // The transcoder branches on the engine's result. The engine
    // deduplicates renders by video_id — one logical render per video,
    // whose terminal result the guarantee fixes — and a rejected
    // source is a terminal error, so a retried transcode observes the
    // same terminal result and takes the same arm. That closes what
    // was the model's one gap before error dispositions existed: the
    // transcoder's idempotency, and the upload's cascade through it,
    // now prove.
    for operation in ["operation.transcode_video", "operation.complete_upload"] {
        let check = verification
            .idempotency
            .iter()
            .find(|check| check.operation == Id(operation.into()))
            .expect("the operation declares idempotency");

        assert!(
            matches!(check.verdict, IdempotencyVerdict::Proven { .. }),
            "expected {operation} proven:\n{:#?}",
            check.verdict
        );
    }

    let report = report::obligations(&model, &verification);

    assert_eq!(report.obligations.len(), 15);

    let unproven: Vec<&str> = report
        .obligations
        .iter()
        .filter(|obligation| obligation.status != Status::Proven)
        .map(|obligation| obligation.id.as_str())
        .collect();

    assert_eq!(unproven, [""; 0], "every obligation should prove");
}

#[test]
fn transactional_outbox_example_is_valid() {
    let model = load("transactional_outbox.yaml");

    let errors = validation::validate(&model);

    assert!(
        errors.is_empty(),
        "transactional outbox example should validate:\n{errors:#?}"
    );
}

/// The acceptance architecture of the outbox revision (§105): a
/// request-driven producer whose transaction atomically mutates state
/// and admits an outbox message, an outbox-consuming relay, and a
/// topic subscriber — with idempotency traced through the outbox,
/// ordering discharged from the outbox runtime, and completion driven
/// by at-least-once outbox delivery.
#[test]
fn transactional_outbox_example_proves_everything() {
    let model = load("transactional_outbox.yaml");

    let verification = verification::verify(&model);

    let report = report::obligations(&model, &verification);

    let unproven: Vec<&str> = report
        .obligations
        .iter()
        .filter(|obligation| obligation.status != Status::Proven)
        .map(|obligation| obligation.id.as_str())
        .collect();

    assert_eq!(unproven, [""; 0], "every obligation should prove");

    // The producer's duplicate outbox write is discharged by its
    // transaction's keyed commit — route one of §50 — not by the
    // message/consumer route.
    let producer = verification
        .idempotency
        .iter()
        .find(|check| check.operation == Id("operation.create_order".into()))
        .expect("the producer declares idempotency");

    let IdempotencyVerdict::Proven { proof, .. } = &producer.verdict else {
        panic!("producer idempotency should prove: {:#?}", producer.verdict);
    };

    let verification::IdempotencyProof::RetrySafePaths { paths } = proof else {
        panic!("producer proof should walk paths: {proof:#?}");
    };

    assert!(
        paths.iter().flat_map(|path| path.effects.iter()).any(|effect| {
            matches!(
                &effect.safety,
                verification::EffectSafety::TransactionDeduplicated { transaction, .. }
                    if transaction == &Id("tx.create_order".into())
            )
        }),
        "the outbox write should be discharged by the keyed commit:\n{paths:#?}"
    );

    // The relay's lineage traces the outbox message identity back to
    // the producer's propagated request_id.
    let relay = verification
        .idempotency
        .iter()
        .find(|check| check.operation == Id("operation.publish_order_event".into()))
        .expect("the relay declares idempotency");

    assert!(
        relay.lineage.iter().any(|lineage| {
            matches!(
                &lineage.source,
                verification::LineageSource::Outbox { outbox }
                    if outbox == &Id("outbox.order_events".into())
            ) && matches!(&lineage.fact, verification::LineageFact::Propagated { .. })
        }),
        "the relay's key should trace through the outbox identity:\n{:#?}",
        relay.lineage
    );

    // The relay's ordering rests on the outbox runtime.
    let ordering = verification
        .ordering
        .iter()
        .find(|check| check.operation == Id("operation.publish_order_event".into()))
        .expect("the relay declares ordering");

    assert!(
        matches!(
            &ordering.verdict,
            verification::OrderingVerdict::Proven {
                proof: verification::OrderingProof::OutboxRoutedOrder { .. },
                ..
            }
        ),
        "relay ordering should prove from the outbox runtime:\n{:#?}",
        ordering.verdict
    );
}

#[test]
fn payment_capture_example_is_valid() {
    let model = load("payment_capture.yaml");

    let errors = validation::validate(&model);

    assert!(
        errors.is_empty(),
        "payment capture example should validate:\n{errors:#?}"
    );
}

/// The pattern in its textbook habitat: a retried payment capture whose
/// transaction atomically records the payment and admits the event, a
/// relay onto a keyed topic, and two independent subscribers. The
/// producer's duplicate write is suppressed by its keyed commit; the
/// relay's duplicate publication collapses at every modeled consumer;
/// per-account relay order and ledger completion rest on the declared
/// runtime. Everything proves, and nothing is left warned about.
#[test]
fn payment_capture_example_proves_everything() {
    let model = load("payment_capture.yaml");

    let verification = verification::verify(&model);

    let report = report::obligations(&model, &verification);

    let unproven: Vec<&str> = report
        .obligations
        .iter()
        .filter(|obligation| obligation.status != Status::Proven)
        .map(|obligation| obligation.id.as_str())
        .collect();

    assert_eq!(unproven, [""; 0], "every obligation should prove");

    // Every duplicate-admitting input declares the idempotency
    // requirement that answers for its retries, so the checker raises
    // no duplicate-delivery warnings.
    assert_eq!(verification.notes, vec![]);

    // The producer: checkout's retries land in one commit class, and
    // that keyed commit is what discharges the staged outbox write —
    // at most one recorded payment and one admitted event per
    // idempotency key.
    let producer = verification
        .idempotency
        .iter()
        .find(|check| check.operation == Id("operation.capture_payment".into()))
        .expect("the producer declares idempotency");

    let IdempotencyVerdict::Proven { proof, .. } = &producer.verdict else {
        panic!("producer idempotency should prove: {:#?}", producer.verdict);
    };

    let verification::IdempotencyProof::RetrySafePaths { paths } = proof else {
        panic!("producer proof should walk paths: {proof:#?}");
    };

    assert!(
        paths.iter().flat_map(|path| path.effects.iter()).any(|effect| {
            effect.effect == Id("effect.capture_payment.outbox_captured".into())
                && matches!(
                    &effect.safety,
                    verification::EffectSafety::TransactionDeduplicated { transaction, .. }
                        if transaction == &Id("tx.capture_payment".into())
                )
        }),
        "the outbox write should be discharged by the keyed commit:\n{paths:#?}"
    );

    // The relay: its duplicate publication is the same logical message
    // under the topic's event_id identity, and BOTH modeled
    // subscribers collapse the duplicate deliveries it may cause.
    let relay = verification
        .idempotency
        .iter()
        .find(|check| check.operation == Id("operation.publish_payment_event".into()))
        .expect("the relay declares idempotency");

    let IdempotencyVerdict::Proven { proof, .. } = &relay.verdict else {
        panic!("relay idempotency should prove: {:#?}", relay.verdict);
    };

    let verification::IdempotencyProof::RetrySafePaths { paths } = proof else {
        panic!("relay proof should walk paths: {proof:#?}");
    };

    let consumers: Vec<(&Id, &Id)> = paths
        .iter()
        .flat_map(|path| path.effects.iter())
        .filter_map(|effect| match &effect.safety {
            verification::EffectSafety::SameLogicalMessage { consumers, .. } => Some(consumers),
            _ => None,
        })
        .flatten()
        .map(|consumer| match consumer {
            verification::ConsumerCollapse::ProvenRequirement { operation, input } => {
                (operation, input)
            }
            verification::ConsumerCollapse::SingleDelivery { operation, input } => {
                (operation, input)
            }
        })
        .collect();

    let ledger = Id("operation.post_ledger_entry".into());
    let receipts = Id("operation.send_receipt".into());

    assert!(
        consumers.iter().any(|(operation, _)| **operation == ledger)
            && consumers.iter().any(|(operation, _)| **operation == receipts),
        "the cascade should collapse at both subscribers:\n{consumers:#?}"
    );

    // The receipts subscriber collapses at the email provider's own
    // deduplication boundary.
    let receipt_check = verification
        .idempotency
        .iter()
        .find(|check| check.operation == receipts)
        .expect("receipts declares idempotency");

    let IdempotencyVerdict::Proven {
        proof: verification::IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &receipt_check.verdict
    else {
        panic!("receipts idempotency should prove: {:#?}", receipt_check.verdict);
    };

    assert!(
        paths.iter().flat_map(|path| path.effects.iter()).any(|effect| {
            matches!(
                &effect.safety,
                verification::EffectSafety::ExternallyDeduplicated { .. }
            )
        }),
        "the email send should be externally deduplicated:\n{paths:#?}"
    );

    // Per-account relay order rests on the outbox runtime's keyed
    // partitioning and partition ordering.
    let ordering = verification
        .ordering
        .iter()
        .find(|check| check.operation == Id("operation.publish_payment_event".into()))
        .expect("the relay declares ordering");

    assert!(
        matches!(
            &ordering.verdict,
            verification::OrderingVerdict::Proven {
                proof: verification::OrderingProof::OutboxRoutedOrder { batching: None, .. },
                ..
            }
        ),
        "relay ordering should prove from the outbox runtime:\n{:#?}",
        ordering.verdict
    );

    // The ledger's guaranteed completion is driven by at-least-once
    // topic delivery; the relay's by at-least-once outbox delivery.
    let driver_of = |operation: &str| {
        let check = verification
            .recoverability
            .iter()
            .find(|check| check.operation == Id(operation.into()))
            .unwrap_or_else(|| panic!("{operation} declares recoverability"));

        match &check.verdict {
            verification::RecoverabilityVerdict::Proven {
                proof: verification::RecoverabilityProof::Guaranteed { driver, .. },
                ..
            } => driver.clone(),
            other => panic!("{operation} should prove guaranteed completion: {other:#?}"),
        }
    };

    assert!(matches!(
        driver_of("operation.post_ledger_entry"),
        verification::RetryDriver::AtLeastOnceDelivery { .. }
    ));

    assert!(matches!(
        driver_of("operation.publish_payment_event"),
        verification::RetryDriver::AtLeastOnceOutboxDelivery { .. }
    ));
}

#[test]
fn hedged_read_example_is_valid() {
    let model = load("hedged_read.yaml");

    let errors = validation::validate(&model);

    assert!(
        errors.is_empty(),
        "hedged read example should validate:\n{errors:#?}"
    );
}

#[test]
fn hedged_read_example_exposes_async_executions_to_the_graph() {
    use conseqa::viz::graph::{EdgeDetail, extract};

    let model = load("hedged_read.yaml");

    let graph = extract(&model);

    // Every effect of the hedged read is launched asynchronously —
    // both raced stores and the fire-and-forget audit — while the
    // ledger write of record_read is an ordinary synchronous
    // execution. The graph says which is which, so a visualization
    // can distinguish the edges (§84 of the async revision).
    let mut asynchronous = Vec::new();
    let mut synchronous = Vec::new();

    for edge in &graph.edges {
        let (effect, executed, launched) = match &edge.detail {
            EdgeDetail::Publish {
                effect,
                executed_at,
                async_executed_at,
                ..
            }
            | EdgeDetail::Request {
                effect,
                executed_at,
                async_executed_at,
                ..
            }
            | EdgeDetail::External {
                effect,
                executed_at,
                async_executed_at,
                ..
            } => (effect, executed_at, async_executed_at),

            _ => continue,
        };

        assert!(!executed.is_empty(), "every declared effect is executed");

        if launched == executed {
            asynchronous.push(effect.0.as_str());
        } else {
            assert!(launched.is_empty(), "no effect mixes launch modes here");
            synchronous.push(effect.0.as_str());
        }
    }

    asynchronous.sort_unstable();

    assert_eq!(
        asynchronous,
        [
            "effect.hedged_read.audit",
            "effect.hedged_read.primary",
            "effect.hedged_read.replica",
        ]
    );

    assert_eq!(synchronous, ["effect.record_read.ledger"]);
}
