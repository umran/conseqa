//! The `abandon` terminal (dsl 5): a message consumer passes a
//! retryable error up by ending the attempt without completing, its
//! message left for another attempt — and still proves idempotency and
//! recoverability.

use std::path::PathBuf;

use conseqa::analyzer::validation::{self, ValidationError};
use conseqa::analyzer::verification::{
    self, DecisionRule, IdempotencyObstacle, IdempotencyProof, IdempotencyVerdict, ModelNote, ProofScope,
    RecoverabilityObstacle, RecoverabilityProof, RecoverabilityVerdict, RetryDriver,
};
use conseqa::parser::yaml;
use conseqa::spec::{Id, Model};
use serde_yaml::Value;

fn id(value: &str) -> Id {
    Id(value.to_owned())
}

fn fixture(name: &str) -> Value {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .expect("fixture readable");

    serde_yaml::from_str(&source).expect("fixture is YAML")
}

fn model(document: &Value) -> Model {
    yaml::parse(&serde_yaml::to_string(document).expect("serializes")).expect("parses")
}

fn yaml_value(text: &str) -> Value {
    serde_yaml::from_str(text).expect("literal YAML")
}

fn program_steps<'a>(document: &'a mut Value, operation: &str) -> &'a mut Vec<Value> {
    document["operations"][operation]["program"]["steps"]
        .as_sequence_mut()
        .expect("a program")
}

fn idempotency(model: &Model, operation: &str) -> IdempotencyVerdict {
    verification::verify(model)
        .idempotency
        .into_iter()
        .find(|check| check.operation == id(operation))
        .expect("an idempotency check")
        .verdict
}

fn recoverability(model: &Model, operation: &str) -> RecoverabilityVerdict {
    verification::verify(model)
        .recoverability
        .into_iter()
        .find(|check| check.operation == id(operation))
        .expect("a recoverability check")
        .verdict
}

/// `video_streaming`, its transcoding engine declaring a retryable
/// `busy` beside the terminal `failed`, and the transcoder abandoning
/// the delivery on `busy` — after its keyed claim committed. `busy`
/// is given `before_abandon` first.
fn a_transcoder_abandoning_on_busy(before_abandon: Vec<Value>) -> Value {
    let mut document = fixture("video_streaming.yaml");

    let steps = program_steps(&mut document, "operation.transcode_video");

    let engine = steps
        .iter_mut()
        .find(|step| step["effect_id"] == "effect.transcode_video.engine")
        .expect("the engine call");

    engine["effect"]["result"]["errors"]["busy"] = yaml_value(
        "schema: schema.RenderFailed\n\
         disposition: retryable\n",
    );

    let matched = steps
        .iter_mut()
        .find(|step| step["kind"] == "match_result")
        .expect("the match on the render");

    let mut busy = before_abandon;
    busy.push(yaml_value("kind: abandon"));

    matched["errors"]["busy"] = yaml_value("steps: []");
    matched["errors"]["busy"]["steps"] = Value::Sequence(busy);

    document
}

/// The consumer shape the revision is for: `ok` does the work, the
/// retryable arm abandons. The decision on `busy` does not replay — a
/// retry may render — but nothing follows it on its own path, so it
/// adds no work; and the redelivery a retry rides on re-encounters the
/// keyed claim, which resolves. Completion is still guaranteed by
/// at-least-once delivery, on the other paths.
#[test]
fn a_consumer_abandoning_on_a_retryable_error_stays_idempotent_and_recoverable() {
    let model = model(&a_transcoder_abandoning_on_busy(Vec::new()));

    assert_eq!(validation::validate(&model), Vec::<ValidationError>::new());

    let verdict = idempotency(&model, "operation.transcode_video");

    let IdempotencyVerdict::Proven {
        proof: IdempotencyProof::RetrySafePaths { paths },
        ..
    } = &verdict
    else {
        panic!("expected a proven verdict, found {verdict:#?}");
    };

    assert!(
        paths.iter().flat_map(|path| &path.decisions).any(|replay| matches!(
            &replay.decision,
            verification::DecisionTaken::Match { arm, .. }
                if *arm == conseqa::spec::ResultArm::err(&id("busy"))
        ) && matches!(
            replay.rule,
            DecisionRule::IdempotencyInertContinuation
        )),
        "{paths:#?}"
    );

    let verdict = recoverability(&model, "operation.transcode_video");

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Guaranteed { driver, paths },
        ..
    } = &verdict
    else {
        panic!("expected a guaranteed verdict, found {verdict:#?}");
    };

    assert!(matches!(driver, RetryDriver::AtLeastOnceDelivery { .. }));
    assert_eq!(paths.iter().filter(|path| path.abandons).count(), 1);
    assert!(paths.iter().any(|path| !path.abandons));
}

/// Work before `abandon` is re-done by the attempt the redelivery
/// starts, so it must be retry-safe like any other: a failure mark
/// committed without a key is not.
#[test]
fn work_before_abandon_must_be_retry_safe() {
    let mut document = fixture("video_streaming.yaml");

    let failure = program_steps(&mut document, "operation.transcode_video")
        .iter()
        .find(|step| step["kind"] == "match_result")
        .expect("the match")["errors"]["failed"]["steps"][0]
        .clone();

    let mut unkeyed = failure;
    unkeyed["transaction"]["id"] = "tx.transcode_video.note_busy".into();
    unkeyed["transaction"]["idempotency"] = yaml_value("kind: unspecified");

    let model = model(&a_transcoder_abandoning_on_busy(vec![unkeyed]));

    assert_eq!(validation::validate(&model), Vec::<ValidationError>::new());

    let verdict = idempotency(&model, "operation.transcode_video");

    let IdempotencyVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:#?}");
    };

    assert!(
        obstacles.iter().any(|obstacle| matches!(
            obstacle,
            IdempotencyObstacle::TransactionNotRetrySafe { transaction, .. }
                if transaction == &id("tx.transcode_video.note_busy")
        )),
        "{obstacles:#?}"
    );
}

/// A consumer that abandons every attempt can never complete one.
#[test]
fn a_consumer_abandoning_on_every_path_cannot_make_progress() {
    let mut document = fixture("video_streaming.yaml");

    *program_steps(&mut document, "operation.transcode_video") = vec![yaml_value("kind: abandon")];

    let model = model(&document);

    assert_eq!(validation::validate(&model), Vec::<ValidationError>::new());

    let verdict = recoverability(&model, "operation.transcode_video");

    let RecoverabilityVerdict::Unproven { obstacles } = &verdict else {
        panic!("expected an unproven verdict, found {verdict:#?}");
    };

    assert!(
        obstacles.iter().any(|obstacle| matches!(
            obstacle,
            RecoverabilityObstacle::EveryPathAbandons { input }
                if input == &id("input.transcode_video.uploaded")
        )),
        "{obstacles:#?}"
    );
}

/// An outbox consumer abandoning leaves its message pending, and the
/// outbox's intrinsic re-drive — an L0 fact — drives completion.
#[test]
fn an_outbox_consumer_abandoning_is_re_driven_by_the_outbox() {
    let mut document = fixture("transactional_outbox.yaml");

    let steps = program_steps(&mut document, "operation.publish_order_event");

    let relay = std::mem::take(steps);

    steps.push(yaml_value(
        "kind: execute_effect\n\
         effect_id: effect.publish_order_event.screen\n\
         effect:\n\
         \x20 kind: external\n\
         \x20 name: screening.check\n\
         \x20 identity:\n\
         \x20   kind: keyed\n\
         \x20   key:\n\
         \x20     components:\n\
         \x20     - source: input:input.publish_order_event.outbox\n\
         \x20       path: event_id\n\
         \x20 idempotency: side_effect_free\n\
         \x20 result_replay: replay_stable\n\
         \x20 result:\n\
         \x20   ok: schema.OrderCreated\n\
         \x20   errors:\n\
         \x20     busy:\n\
         \x20       schema: schema.OrderCreated\n\
         \x20       disposition: retryable\n\
         values:\n\
         \x20 kind: deterministic\n\
         \x20 from:\n\
         \x20 - source: input:input.publish_order_event.outbox\n\
         \x20   path: event_id\n\
         bind: result.publish_order_event.screen\n",
    ));

    let mut matched = yaml_value(
        "kind: match_result\n\
         result: result.publish_order_event.screen\n\
         ok:\n\
         \x20 steps: []\n\
         errors:\n\
         \x20 busy:\n\
         \x20   steps:\n\
         \x20   - kind: abandon\n",
    );

    matched["ok"]["steps"] = Value::Sequence(relay);
    steps.push(matched);

    let model = model(&document);

    assert_eq!(validation::validate(&model), Vec::<ValidationError>::new());

    let verdict = recoverability(&model, "operation.publish_order_event");

    let RecoverabilityVerdict::Proven {
        proof: RecoverabilityProof::Guaranteed { driver, paths },
        scope,
    } = &verdict
    else {
        panic!("expected a guaranteed verdict, found {verdict:#?}");
    };

    assert!(matches!(driver, RetryDriver::IntrinsicOutboxRedrive { .. }));
    assert_eq!(*scope, ProofScope::L0Only);
    assert!(paths.iter().any(|path| path.abandons));

    assert!(matches!(
        idempotency(&model, "operation.publish_order_event"),
        IdempotencyVerdict::Proven { .. }
    ));
}

/// A request-only operation cannot abandon: a request passes a
/// retryable error up by returning it.
#[test]
fn abandon_needs_a_message_input() {
    let mut document = fixture("video_streaming.yaml");

    program_steps(&mut document, "operation.complete_upload")
        .insert(0, yaml_value("kind: abandon"));

    let errors = validation::validate(&model(&document));

    assert!(
        errors.iter().any(|error| matches!(
            error,
            ValidationError::AbandonWithoutMessageInput { operation, .. }
                if operation == &id("operation.complete_upload")
        )),
        "{errors:#?}"
    );
}

/// Under at-most-once delivery an abandoned message is never delivered
/// again: `abandon` drops it, and the checker says so.
#[test]
fn abandon_under_at_most_once_delivery_is_flagged() {
    let mut document = a_transcoder_abandoning_on_busy(Vec::new());

    document["runtime"]["subscriptions"]["operation.transcode_video"]
        ["input.transcode_video.uploaded"]["delivery"] = "at_most_once".into();

    let report = verification::verify(&model(&document));

    assert!(
        report.notes.iter().any(|note| matches!(
            note,
            ModelNote::AbandonWithoutRedelivery { operation, .. }
                if operation == &id("operation.transcode_video")
        )),
        "{:#?}",
        report.notes
    );
}

/// `abandon` survives the canonical round trip.
#[test]
fn abandon_round_trips() {
    let model = model(&a_transcoder_abandoning_on_busy(Vec::new()));

    let serialized = yaml::serialize(&model).expect("serializes");

    assert!(serialized.contains("kind: abandon"), "{serialized}");
    assert_eq!(yaml::parse(&serialized).expect("reparses"), model);
}
