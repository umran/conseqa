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

    // 2 transaction serializability + 5 idempotency + 1 result replay
    // + 4 recoverability.
    assert_eq!(report.obligations.len(), 12);

    // Same-video work is serializable by the serializable-isolation
    // route: every transaction in each closure declares serializable
    // isolation, and no runtime fact is consumed.
    for (transaction, key) in [
        ("tx.transcode_video.complete", "video_id"),
        ("tx.publish_video.ready", "video_id"),
    ] {
        let check = verification
            .transaction_serializability
            .iter()
            .find(|check| check.transaction == Id(transaction.into()))
            .expect("the transaction declares serializability");

        assert!(
            matches!(
                &check.verdict,
                verification::TransactionSerializabilityVerdict::Proven {
                    proof: verification::TransactionSerializabilityProof::SerializableIsolationClosure { .. },
                    ..
                }
            ),
            "expected {transaction} proven by serializable isolation over {key}:\n{:#?}",
            check.verdict
        );
    }

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
/// and admits an outbox message, the outbox's one consuming relay, and
/// a topic subscriber — with idempotency traced through the outbox and
/// completion driven by the outbox's intrinsic durable re-drive. The
/// relay declares no ordering: it commits no transaction, and the
/// outbox runtime's partitioning is a placement fact, not a proof.
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
        paths
            .iter()
            .flat_map(|path| path.effects.iter())
            .any(|effect| {
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

    // No transaction property is declared anywhere in the model, so
    // the transaction families are empty rather than vacuously proven.
    assert!(verification.transaction_serializability.is_empty());
    assert!(verification.transaction_ordering.is_empty());
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
/// ledger completion rests on the declared runtime's at-least-once
/// delivery. Everything proves, and nothing is left warned about.
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
        paths
            .iter()
            .flat_map(|path| path.effects.iter())
            .any(|effect| {
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
            && consumers
                .iter()
                .any(|(operation, _)| **operation == receipts),
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
        panic!(
            "receipts idempotency should prove: {:#?}",
            receipt_check.verdict
        );
    };

    assert!(
        paths
            .iter()
            .flat_map(|path| path.effects.iter())
            .any(|effect| {
                matches!(
                    &effect.safety,
                    verification::EffectSafety::ExternallyIdempotent { .. }
                )
            }),
        "the email send should be externally idempotent:\n{paths:#?}"
    );

    // The ledger's guaranteed completion is driven by at-least-once
    // topic delivery; the relay's by the outbox's intrinsic durable
    // re-drive.
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
        verification::RetryDriver::IntrinsicOutboxRedrive { .. }
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

#[test]
fn tenant_ledger_example_is_valid() {
    let model = load("tenant_ledger.yaml");

    let errors = validation::validate(&model);

    assert!(
        errors.is_empty(),
        "tenant ledger example should validate:\n{errors:#?}"
    );
}

/// A partitioned accounts store written through a request boundary, an
/// outbox partitioned by tenant, a relay pool routed by tenant, a
/// per-tenant-ordered topic, and two subscribers — one applying the
/// events to a second partitioned store in sequence order. Every
/// transport fact is placement; every proof is a transaction's own.
#[test]
fn tenant_ledger_example_proves_everything() {
    let model = load("tenant_ledger.yaml");

    let verification = verification::verify(&model);

    let report = report::obligations(&model, &verification);

    // 2 serializability + 1 ordering + 4 idempotency + 1 result replay
    // + 4 recoverability.
    assert_eq!(report.obligations.len(), 12);

    let unproven: Vec<&str> = report
        .obligations
        .iter()
        .filter(|obligation| obligation.status != Status::Proven)
        .map(|obligation| obligation.id.as_str())
        .collect();

    assert_eq!(unproven, [""; 0], "every obligation should prove");
    assert_eq!(verification.notes, vec![]);

    // The producer assigns the sequence under the tenant row's
    // exclusive lock: its read-then-write of last_sequence is
    // commit-ordered by strict locking.
    let post = verification
        .transaction_serializability
        .iter()
        .find(|check| check.transaction == Id("tx.post_entry".into()))
        .expect("post_entry declares serializability");

    let verification::TransactionSerializabilityVerdict::Proven {
        proof: verification::TransactionSerializabilityProof::ConflictGraph { dependencies, .. },
        scope: verification::ProofScope::L0Only,
    } = &post.verdict
    else {
        panic!("post_entry should prove by the graph route:\n{:#?}", post.verdict);
    };

    assert!(
        dependencies.iter().any(|dependency| matches!(
            dependency.evidence,
            verification::CommitOrderEvidence::StrictLock { .. }
        )),
        "{dependencies:#?}"
    );

    // The ledger writer: serializable per tenant by the version
    // protocol, and ordered by sequence through the successor cursor.
    let apply = verification
        .transaction_serializability
        .iter()
        .find(|check| check.transaction == Id("tx.apply_entry".into()))
        .expect("apply_entry declares serializability");

    let verification::TransactionSerializabilityVerdict::Proven {
        proof: verification::TransactionSerializabilityProof::ConflictGraph { dependencies, .. },
        ..
    } = &apply.verdict
    else {
        panic!("apply_entry should prove by the graph route:\n{:#?}", apply.verdict);
    };

    assert!(
        dependencies.iter().any(|dependency| matches!(
            dependency.evidence,
            verification::CommitOrderEvidence::VersionValidation { .. }
        )),
        "{dependencies:#?}"
    );

    let ordering = verification
        .transaction_ordering
        .iter()
        .find(|check| check.transaction == Id("tx.apply_entry".into()))
        .expect("apply_entry declares ordering");

    assert!(
        matches!(
            &ordering.verdict,
            verification::TransactionOrderingVerdict::Proven {
                proof: verification::TransactionOrderingProof::Cursor {
                    rule: conseqa::spec::CursorAdvanceRule::Successor,
                    ..
                },
                scope: verification::ProofScope::L0Only,
            }
        ),
        "{:#?}",
        ordering.verdict
    );

    // The relay's duplicate publication is the same logical message,
    // and both subscribers collapse it: the ledger by its keyed
    // commit, the notifier at the gateway's own deduplication.
    let relay = verification
        .idempotency
        .iter()
        .find(|check| check.operation == Id("operation.relay_tenant_event".into()))
        .expect("the relay declares idempotency");

    let IdempotencyVerdict::Proven {
        proof: verification::IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &relay.verdict
    else {
        panic!("relay idempotency should prove: {:#?}", relay.verdict);
    };

    let consumers: Vec<&Id> = paths
        .iter()
        .flat_map(|path| path.effects.iter())
        .filter_map(|effect| match &effect.safety {
            verification::EffectSafety::SameLogicalMessage { consumers, .. } => Some(consumers),
            _ => None,
        })
        .flatten()
        .map(|consumer| match consumer {
            verification::ConsumerCollapse::ProvenRequirement { operation, .. }
            | verification::ConsumerCollapse::SingleDelivery { operation, .. } => operation,
        })
        .collect();

    assert!(consumers.contains(&&Id("operation.apply_entry".into())), "{consumers:?}");
    assert!(consumers.contains(&&Id("operation.notify_entry".into())), "{consumers:?}");

    // Completion drivers: the relay's intrinsic re-drive, the
    // subscribers' at-least-once delivery.
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
        driver_of("operation.relay_tenant_event"),
        verification::RetryDriver::IntrinsicOutboxRedrive { .. }
    ));

    for operation in ["operation.apply_entry", "operation.notify_entry"] {
        assert!(
            matches!(driver_of(operation), verification::RetryDriver::AtLeastOnceDelivery { .. }),
            "{operation}"
        );
    }

    // The posting's result — entry id and assigned sequence — is
    // recovered from its keyed commit, so retries answer alike.
    let result = verification
        .result_replay
        .iter()
        .find(|check| check.operation == Id("operation.post_entry".into()))
        .expect("post_entry declares a replay-consistent result");

    assert!(
        matches!(result.verdict, verification::ResultReplayVerdict::Proven { .. }),
        "{:#?}",
        result.verdict
    );
}
