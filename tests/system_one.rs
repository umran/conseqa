//! The System One layer against a scripted wire-format server.
//!
//! The server speaks `POST /v1/systemone` and answers by word overlap
//! — no model, fully deterministic. Beside its honest mode it has one
//! mode per way a server can reproduce the wire shape without the
//! behaviour, so every conformance probe is shown to catch what it
//! claims to (§11.2, §32 of the System One orchestration revision).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::routing::post;
use axum::{Json as Body, Router};
use serde_json::{Value, json};

use conseqa::system_one::conformance::{self, Verdict};
use conseqa::system_one::{
    Agreement, Answer, AnswerDefect, AskRecord, BackendSettings, CircuitPolicy, Decider,
    DeciderConfigError, DeciderError, DeciderIdentity, DeciderKind, DeciderSettings, Decision,
    DecisionLog, DecisionRequest, LoggingDecider, Question, QuestionDefect, QuestionId,
    ReplayDecider, ReplayError, RequestBudget, RetryPolicy, ShadowDecider, SystemOneHttpDecider,
};

const MODEL: &str = "jev-1.13.0";

// ---------------------------------------------------------------------
// The scripted server
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Honest,

    /// Floors the winner at 0.75 and hands the rest out evenly.
    Floors,

    /// Answers a request that cannot be answered, with its first option.
    AnswersTheUnaskable,

    /// Shows its model only the first twenty options of a choice.
    ShowsTwentyOptions,

    /// Tells options apart by their keys, never their criteria.
    IgnoresCriteria,

    /// Lets the presence of other questions move an answer.
    LeaksQuestions,
}

/// A response served before the server starts behaving.
struct Scripted {
    status: u16,
    retry_after: Option<&'static str>,
    body: Value,
}

struct Fake {
    behaviour: Behaviour,
    requests: AtomicU32,
    script: Mutex<VecDeque<Scripted>>,
    bodies: Mutex<Vec<Value>>,
    authorizations: Mutex<Vec<Option<String>>>,
}

struct Server {
    url: String,
    fake: Arc<Fake>,
}

impl Server {
    fn requests(&self) -> u32 {
        self.fake.requests.load(Ordering::SeqCst)
    }

    fn script(&self, status: u16, retry_after: Option<&'static str>, body: Value) {
        self.fake.script.lock().unwrap().push_back(Scripted {
            status,
            retry_after,
            body,
        });
    }

    fn last_body(&self) -> Value {
        self.fake
            .bodies
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a request was received")
    }

    fn decider(&self) -> SystemOneHttpDecider {
        SystemOneHttpDecider::new(&self.url, MODEL)
            .expect("a loopback decider")
            .with_retry(fast_retries(2))
            .expect("a client")
    }
}

async fn serve(behaviour: Behaviour) -> Server {
    let fake = Arc::new(Fake {
        behaviour,
        requests: AtomicU32::new(0),
        script: Mutex::new(VecDeque::new()),
        bodies: Mutex::new(Vec::new()),
        authorizations: Mutex::new(Vec::new()),
    });

    let router = Router::new()
        .route("/v1/systemone", post(handle))
        .with_state(fake.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");

    let url = format!("http://{}", listener.local_addr().expect("a bound address"));

    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("the scripted server runs");
    });

    Server { url, fake }
}

fn fast_retries(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        backoff_initial: Duration::from_millis(1),
        backoff_max: Duration::from_millis(4),
        backoff_jitter: 0.0,
        respect_retry_after: true,
        retry_after_cap: Duration::from_millis(20),
        timeout: Duration::from_secs(5),
    }
}

async fn handle(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Body(body): Body<Value>,
) -> (StatusCode, HeaderMap, String) {
    fake.requests.fetch_add(1, Ordering::SeqCst);

    fake.authorizations.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    );

    fake.bodies.lock().unwrap().push(body.clone());

    if let Some(scripted) = fake.script.lock().unwrap().pop_front() {
        let mut headers = HeaderMap::new();

        if let Some(seconds) = scripted.retry_after {
            headers.insert("retry-after", HeaderValue::from_static(seconds));
        }

        return (
            StatusCode::from_u16(scripted.status).expect("a status"),
            headers,
            scripted.body.to_string(),
        );
    }

    match answer(fake.behaviour, &body) {
        Ok(response) => (StatusCode::OK, HeaderMap::new(), response.to_string()),

        Err(reason) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            HeaderMap::new(),
            json!({ "error": reason }).to_string(),
        ),
    }
}

fn answer(behaviour: Behaviour, body: &Value) -> Result<Value, String> {
    if body.get("state").is_none() && behaviour != Behaviour::AnswersTheUnaskable {
        return Err("`state` is required".to_string());
    }

    let state = words_of(&body["state"]);

    let questions = body["questions"]
        .as_object()
        .ok_or("`questions` is not a map")?;

    let mut answers = serde_json::Map::new();

    for (id, question) in questions {
        let answered = match question["type"].as_str() {
            Some("choice") => choice(behaviour, &state, question, questions.len()),
            Some("score") => score(behaviour, &state, question),
            Some("noul") => Ok(noul(&state, question)),
            other => Err(format!("unknown question type {other:?}")),
        };

        match answered {
            Ok(answered) => {
                answers.insert(id.clone(), answered);
            }

            // The misbehaviour: a question that cannot be answered is
            // answered anyway.
            Err(_) if behaviour == Behaviour::AnswersTheUnaskable => {
                answers.insert(id.clone(), json!({ "type": "noul", "noul": 1.0 }));
            }

            Err(reason) => return Err(reason),
        }
    }

    Ok(json!({
        "model": body["model"],
        "answers": answers,
        "usage": { "input_tokens": body.to_string().len() / 4, "output_tokens": 0 },
    }))
}

fn choice(
    behaviour: Behaviour,
    state: &BTreeSet<String>,
    question: &Value,
    siblings: usize,
) -> Result<Value, String> {
    let criteria = question["criteria"]
        .as_object()
        .ok_or("`criteria` is not a map")?;

    if criteria.len() < 2 {
        return Err("a choice needs at least two options".to_string());
    }

    let mut logits: Vec<f64> = criteria
        .iter()
        .enumerate()
        .map(|(position, (key, description))| {
            if behaviour == Behaviour::ShowsTwentyOptions && position >= 20 {
                return f64::NEG_INFINITY;
            }

            let described = match (behaviour, description) {
                (Behaviour::IgnoresCriteria, _) | (_, Value::Null) => words_of(&json!(key)),
                (_, description) => words_of(description),
            };

            2.0 * overlap(state, &described)
        })
        .collect();

    if behaviour == Behaviour::LeaksQuestions && siblings > 1 {
        *logits.last_mut().expect("at least two options") += 3.0;
    }

    let mut probabilities = softmax(&logits);

    let winner = argmax(&probabilities);

    if behaviour == Behaviour::Floors {
        let floored = probabilities[winner].max(0.75);

        let rest = (1.0 - floored) / (probabilities.len() - 1) as f64;

        probabilities = (0..probabilities.len())
            .map(|index| if index == winner { floored } else { rest })
            .collect();
    }

    let keys: Vec<&String> = criteria.keys().collect();

    Ok(json!({
        "type": "choice",
        "choice": keys[winner],
        "probabilities": keys.iter().zip(&probabilities).collect::<BTreeMap<_, _>>(),
        "confidence": concentration(&probabilities),
    }))
}

fn score(_: Behaviour, state: &BTreeSet<String>, question: &Value) -> Result<Value, String> {
    let levels = question["criteria"]
        .as_array()
        .ok_or("`criteria` is not a list")?;

    if levels.len() < 2 {
        return Err("a score needs at least two levels".to_string());
    }

    let logits: Vec<f64> = levels
        .iter()
        .map(|level| 2.0 * overlap(state, &words_of(level)))
        .collect();

    let probabilities = softmax(&logits);

    let expectation: f64 = probabilities
        .iter()
        .enumerate()
        .map(|(level, probability)| level as f64 * probability)
        .sum();

    Ok(json!({
        "type": "score",
        "score": expectation,
        "legend": levels.iter().enumerate().map(|(level, text)| (level.to_string(), text)).collect::<BTreeMap<_, _>>(),
        "probabilities": probabilities.iter().enumerate().map(|(level, probability)| (level.to_string(), probability)).collect::<BTreeMap<_, _>>(),
        "confidence": concentration(&probabilities),
    }))
}

fn noul(state: &BTreeSet<String>, question: &Value) -> Value {
    let asked = words_of(&question["instructions"]);

    json!({ "type": "noul", "noul": if overlap(state, &asked) >= 2.0 { 0.9 } else { 0.1 } })
}

const STOPWORDS: [&str; 14] = [
    "the", "and", "for", "was", "has", "not", "does", "about", "say", "message", "text", "this",
    "with", "its",
];

/// Every word of every string in `value`, lowercased.
fn words_of(value: &Value) -> BTreeSet<String> {
    let mut text = String::new();

    fn collect(value: &Value, into: &mut String) {
        match value {
            Value::String(string) => {
                into.push_str(string);
                into.push(' ');
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, into)),
            Value::Object(fields) => fields.values().for_each(|field| collect(field, into)),
            _ => {}
        }
    }

    collect(value, &mut text);

    text.to_lowercase()
        .split(|character: char| !character.is_alphabetic())
        .filter(|word| word.len() > 2 && !STOPWORDS.contains(word))
        .map(str::to_string)
        .collect()
}

fn overlap(first: &BTreeSet<String>, second: &BTreeSet<String>) -> f64 {
    first.intersection(second).count() as f64
}

fn softmax(logits: &[f64]) -> Vec<f64> {
    let highest = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let weights: Vec<f64> = logits.iter().map(|logit| (logit - highest).exp()).collect();

    let total: f64 = weights.iter().sum();

    weights.iter().map(|weight| weight / total).collect()
}

fn argmax(probabilities: &[f64]) -> usize {
    probabilities
        .iter()
        .enumerate()
        .fold((0, f64::MIN), |best, (index, probability)| {
            if *probability > best.1 {
                (index, *probability)
            } else {
                best
            }
        })
        .0
}

fn concentration(probabilities: &[f64]) -> f64 {
    let entropy: f64 = probabilities
        .iter()
        .filter(|probability| **probability > 0.0)
        .map(|probability| -probability * probability.ln())
        .sum();

    1.0 - entropy / (probabilities.len() as f64).ln()
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn ticket() -> Value {
    json!({ "ticket": "I was charged twice for one invoice. Please refund the duplicate payment." })
}

fn department() -> Question {
    Question::choice(
        "Which team should handle `ticket`?",
        [
            (
                "billing",
                "Charged amounts, an invoice, a payment or a refund.",
            ),
            ("shipping", "Parcel delivery, delays and lost packages."),
            ("account", "Password reset and login problems."),
        ],
    )
}

fn triage() -> DecisionRequest {
    DecisionRequest::new(ticket())
        .ask("department", department())
        .ask(
            "refund_requested",
            Question::noul_with_criteria(
                "Does `ticket` ask for a refund of a duplicate payment?",
                "It asks for money back.",
                "It does not ask for money back.",
            ),
        )
        .ask(
            "directness",
            Question::score(
                "How directly does `ticket` ask for money back?",
                [
                    "It does not ask for money back at all.",
                    "It explicitly asks for a refund.",
                ],
            ),
        )
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "conseqa-system-one-{name}-{}",
        uuid::Uuid::new_v4()
    ))
}

// ---------------------------------------------------------------------
// The wire format
// ---------------------------------------------------------------------

#[tokio::test]
async fn typed_questions_go_out_and_typed_answers_come_back() {
    let server = serve(Behaviour::Honest).await;

    let decision = server
        .decider()
        .decide(&triage().tag("task", "never sent"))
        .await
        .expect("a decision");

    let (department, probabilities) = decision.choice("department").expect("a choice");

    assert_eq!(department, "billing");
    assert_eq!(probabilities.len(), 3);

    assert!(decision.noul("refund_requested").expect("a noul") > 0.5);
    assert!(decision.score("directness").expect("a score") > 0.5);

    assert_eq!(decision.identity.backend, "system_one_http");
    assert_eq!(decision.identity.model, MODEL);
    assert_eq!(decision.answered_by, MODEL);
    assert!(decision.usage.expect("usage").input_tokens > 0);
    assert!(decision.shadow.is_none());

    // What crossed the wire is the documented shape, and nothing else:
    // tags are for the log and are never sent.
    let sent = server.last_body();

    assert_eq!(
        sent.as_object()
            .expect("an object")
            .keys()
            .collect::<Vec<_>>(),
        ["model", "questions", "state"]
    );
    assert_eq!(sent["model"], MODEL);
    assert_eq!(sent["state"], ticket());
    assert_eq!(sent["questions"]["department"]["type"], "choice");
    assert_eq!(sent["questions"]["directness"]["type"], "score");
    assert_eq!(
        sent["questions"]["refund_requested"],
        json!({
            "type": "noul",
            "instructions": "Does `ticket` ask for a refund of a duplicate payment?",
            "criteria": { "true": "It asks for money back.", "false": "It does not ask for money back." },
        })
    );
}

/// Before a run is allowed to send anything, it can be run once with
/// the preview decider in the wire client's place. What it writes is the
/// body the wire client posts — compared here with what a server
/// actually received — and it sends nothing: no URL, no credential, no
/// egress, and an ask that fails, so that every builder abstains.
#[tokio::test]
async fn a_preview_writes_what_would_be_sent_and_sends_nothing() {
    let path = scratch("preview").with_extension("jsonl");

    let configured = DeciderSettings {
        kind: DeciderKind::Preview,
        primary: BackendSettings {
            model: Some(MODEL.to_string()),
            ..Default::default()
        },
        log: Some(path.clone()),
        ..Default::default()
    }
    .build()
    .expect("a preview needs no url and no credential")
    .expect("the layer is on");

    assert!(configured.egress.is_empty());

    let request = triage().tag("task", "never sent");

    let refused = configured
        .decider
        .decide(&request)
        .await
        .expect_err("a preview answers nothing");

    assert!(matches!(refused, DeciderError::PreviewOnly), "{refused}");

    // The same request through the wire client, to a server that keeps
    // what it received.
    let server = serve(Behaviour::Honest).await;

    server.decider().decide(&request).await.expect("a decision");

    let written = std::fs::read_to_string(&path).expect("the preview was written");

    let previewed: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).expect("one json object per line"))
        .collect();

    assert_eq!(previewed.len(), 1);
    assert_eq!(previewed[0]["body"], server.last_body());
    assert_eq!(previewed[0]["tags"]["task"], "never sent");

    // A request the wire client would have refused is not previewed as
    // though it would have been sent.
    let unaskable = DecisionRequest::new(ticket());

    assert!(matches!(
        configured.decider.decide(&unaskable).await,
        Err(DeciderError::NoQuestions)
    ));

    assert_eq!(
        std::fs::read_to_string(&path)
            .expect("still readable")
            .lines()
            .count(),
        1
    );

    // Without a file to write to, a preview would show nothing.
    assert!(
        DeciderSettings {
            kind: DeciderKind::Preview,
            ..Default::default()
        }
        .build()
        .is_err()
    );
}

#[tokio::test]
async fn the_credential_is_sent_as_a_bearer_and_shown_nowhere() {
    let server = serve(Behaviour::Honest).await;

    let var = format!("CONSEQA_TEST_KEY_{}", uuid::Uuid::new_v4().simple());

    // SAFETY: the variable's name is unique to this test, so no other
    // thread reads or writes it.
    unsafe { std::env::set_var(&var, "s3cr3t-credential") };

    let decider = server
        .decider()
        .with_credential_from_env(&var)
        .expect("the credential is set");

    assert!(!format!("{decider:?}").contains("s3cr3t"), "{decider:?}");

    decider.decide(&triage()).await.expect("a decision");

    assert_eq!(
        server
            .fake
            .authorizations
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some("Bearer s3cr3t-credential")
    );

    // A local server needs no credential, and is sent none.
    server
        .decider()
        .decide(&triage())
        .await
        .expect("a decision");

    assert_eq!(
        server
            .fake
            .authorizations
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten(),
        None
    );

    assert!(matches!(
        server
            .decider()
            .with_credential_from_env("CONSEQA_TEST_KEY_THAT_IS_NOT_SET"),
        Err(DeciderConfigError::MissingCredential { .. })
    ));
}

/// A key file keeps the secret out of client configs the owning
/// application rewrites; it is trimmed and sent like an env credential.
#[tokio::test]
async fn the_credential_can_be_read_from_a_file() {
    let server = serve(Behaviour::Honest).await;

    let path = std::env::temp_dir().join(format!("conseqa-key-{}", uuid::Uuid::new_v4()));

    std::fs::write(&path, "file-credential\n").expect("key written");

    let decider = server
        .decider()
        .with_credential_from_file(&path)
        .expect("the file holds a credential");

    assert!(!format!("{decider:?}").contains("file-credential"), "{decider:?}");

    decider.decide(&triage()).await.expect("a decision");

    assert_eq!(
        server
            .fake
            .authorizations
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some("Bearer file-credential")
    );

    std::fs::write(&path, "  \n").expect("emptied");

    assert!(matches!(
        server.decider().with_credential_from_file(&path),
        Err(DeciderConfigError::UnreadableCredentialFile { .. })
    ));

    std::fs::remove_file(&path).ok();

    assert!(matches!(
        server.decider().with_credential_from_file(&path),
        Err(DeciderConfigError::UnreadableCredentialFile { .. })
    ));
}

#[tokio::test]
async fn a_model_alias_is_refused_unless_it_is_asked_for() {
    assert!(matches!(
        SystemOneHttpDecider::new("http://127.0.0.1:9", "jev-latest"),
        Err(DeciderConfigError::AliasedModel { .. })
    ));

    assert!(SystemOneHttpDecider::with_alias_allowed("http://127.0.0.1:9", "jev-latest").is_ok());

    assert!(matches!(
        SystemOneHttpDecider::new("ftp://example.test", MODEL),
        Err(DeciderConfigError::InvalidBaseUrl(_))
    ));
}

#[tokio::test]
async fn only_a_url_that_leaves_the_machine_is_egress() {
    for (url, egress) in [
        ("http://127.0.0.1:8000", false),
        ("http://localhost:8000", false),
        ("http://[::1]:8000", false),
        ("https://api.typesafe.ai", true),
        ("http://192.168.1.20:8000", true),
    ] {
        let decider = SystemOneHttpDecider::new(url, MODEL).expect("a decider");

        assert_eq!(decider.is_egress(), egress, "{url}");
    }
}

// ---------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------

#[tokio::test]
async fn transient_failures_are_retried() {
    let server = serve(Behaviour::Honest).await;

    server.script(429, Some("0"), json!({ "error": "slow down" }));
    server.script(503, None, json!({ "error": "overloaded" }));

    server
        .decider()
        .decide(&triage())
        .await
        .expect("the third attempt answers");

    assert_eq!(server.requests(), 3);
}

#[tokio::test]
async fn retries_are_bounded() {
    let server = serve(Behaviour::Honest).await;

    for _ in 0..3 {
        server.script(529, None, json!({ "error": "overloaded" }));
    }

    let decider = server
        .decider()
        .with_retry(fast_retries(1))
        .expect("a client");

    let error = decider
        .decide(&triage())
        .await
        .expect_err("both attempts fail");

    assert!(
        matches!(&error, DeciderError::Unavailable { attempts: 2, last } if last == "HTTP 529"),
        "{error}"
    );

    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn a_request_the_backend_will_never_accept_is_not_retried() {
    let server = serve(Behaviour::Honest).await;

    server.script(422, None, json!({ "error": "`state` is required" }));

    let error = server
        .decider()
        .decide(&triage())
        .await
        .expect_err("rejected");

    assert!(
        matches!(&error, DeciderError::Rejected { status: 422, body } if body.contains("`state` is required")),
        "{error}"
    );

    server.script(401, None, json!({ "error": "bad key" }));

    let error = server
        .decider()
        .decide(&triage())
        .await
        .expect_err("unauthorized");

    assert!(
        matches!(error, DeciderError::Unauthorized { status: 401 }),
        "{error}"
    );

    assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn a_malformed_or_oversize_request_never_leaves_the_process() {
    let server = serve(Behaviour::Honest).await;

    let one_option = DecisionRequest::new(ticket()).ask(
        "department",
        Question::choice("Which team?", [("billing", Value::Null)]),
    );

    assert!(matches!(
        server.decider().decide(&one_option).await,
        Err(DeciderError::InvalidQuestion {
            defect: QuestionDefect::TooFewOptions { found: 1 },
            ..
        })
    ));

    assert!(matches!(
        server
            .decider()
            .decide(&DecisionRequest::new(ticket()))
            .await,
        Err(DeciderError::NoQuestions)
    ));

    let cramped = server.decider().with_budget(RequestBudget {
        max_request_tokens: 64_000,
        max_state_tokens: 8,
        bytes_per_token: 3,
    });

    // Refused, not truncated: the builder must slice its state.
    assert!(matches!(
        cramped.decide(&triage()).await,
        Err(DeciderError::Oversize { budget: 8, .. })
    ));

    assert_eq!(server.requests(), 0);
}

#[tokio::test]
async fn an_answer_that_does_not_answer_its_question_is_refused() {
    let server = serve(Behaviour::Honest).await;

    let ask = DecisionRequest::new(ticket()).ask("department", department());

    let reply = |answer: Value| json!({ "model": MODEL, "answers": { "department": answer } });

    // An option that was never offered.
    server.script(
        200,
        None,
        reply(json!({
            "type": "choice", "choice": "sales", "confidence": 1.0,
            "probabilities": { "billing": 0.0, "shipping": 0.0, "account": 0.0, "sales": 1.0 },
        })),
    );

    // An offered option with no probability.
    server.script(
        200,
        None,
        reply(json!({
            "type": "choice", "choice": "billing", "confidence": 1.0,
            "probabilities": { "billing": 1.0, "shipping": 0.0 },
        })),
    );

    // Probabilities that are not a distribution.
    server.script(
        200,
        None,
        reply(json!({
            "type": "choice", "choice": "billing", "confidence": 1.0,
            "probabilities": { "billing": 0.9, "shipping": 0.9, "account": 0.9 },
        })),
    );

    // The wrong type altogether.
    server.script(200, None, reply(json!({ "type": "noul", "noul": 0.9 })));

    // No answer at all.
    server.script(200, None, json!({ "model": MODEL, "answers": {} }));

    let expected: [fn(&DeciderError) -> bool; 5] = [
        |error| {
            matches!(
                error,
                DeciderError::NonConformant {
                    defect: AnswerDefect::UnofferedChoice { .. },
                    ..
                }
            )
        },
        |error| {
            matches!(
                error,
                DeciderError::NonConformant {
                    defect: AnswerDefect::UnreportedOption { .. },
                    ..
                }
            )
        },
        |error| {
            matches!(
                error,
                DeciderError::NonConformant {
                    defect: AnswerDefect::NotADistribution { .. },
                    ..
                }
            )
        },
        |error| {
            matches!(
                error,
                DeciderError::NonConformant {
                    defect: AnswerDefect::WrongType { .. },
                    ..
                }
            )
        },
        |error| matches!(error, DeciderError::Unanswered { .. }),
    ];

    for is_expected in expected {
        let error = server.decider().decide(&ask).await.expect_err("refused");

        assert!(is_expected(&error), "{error}");
    }
}

#[tokio::test]
async fn the_circuit_opens_after_repeated_failures_and_closes_on_a_success() {
    let server = serve(Behaviour::Honest).await;

    let decider = server
        .decider()
        .with_retry(fast_retries(0))
        .expect("a client")
        .with_circuit(CircuitPolicy {
            failure_threshold: 2,
            cool_down: Duration::from_millis(60),
        });

    for _ in 0..2 {
        server.script(503, None, json!({}));

        assert!(matches!(
            decider.decide(&triage()).await,
            Err(DeciderError::Unavailable { .. })
        ));
    }

    // Open: the backend is not even asked, so builders abstain at once
    // instead of each waiting out its own retries.
    assert!(matches!(
        decider.decide(&triage()).await,
        Err(DeciderError::CircuitOpen { .. })
    ));

    assert_eq!(server.requests(), 2);

    tokio::time::sleep(Duration::from_millis(80)).await;

    decider
        .decide(&triage())
        .await
        .expect("the trial ask succeeds");
    decider
        .decide(&triage())
        .await
        .expect("and the circuit is closed");

    assert_eq!(server.requests(), 4);
}

// ---------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_recording_replays_without_a_live_call() {
    let server = serve(Behaviour::Honest).await;

    let path = scratch("replay").with_extension("json");

    let recorder = ReplayDecider::record(&path, Arc::new(server.decider())).expect("a recorder");

    let live = recorder.decide(&triage()).await.expect("recorded");

    assert_eq!(server.requests(), 1);
    assert_eq!(recorder.len(), 3);

    let replayed = ReplayDecider::replay(&path, MODEL)
        .expect("the store exists")
        .decide(&triage())
        .await
        .expect("replayed");

    assert_eq!(replayed.answers, live.answers);
    assert_eq!(replayed.identity.backend, "replay");
    assert_eq!(server.requests(), 1, "replay never calls the backend");

    // The store keeps the request as it was asked, and no credential.
    let stored = std::fs::read_to_string(&path).expect("the store");

    assert!(stored.contains("Which team should handle `ticket`?"));
    assert!(stored.contains("charged twice"));
}

#[tokio::test]
async fn a_reworded_question_or_another_model_misses() {
    let server = serve(Behaviour::Honest).await;

    let path = scratch("miss").with_extension("json");

    ReplayDecider::record(&path, Arc::new(server.decider()))
        .expect("a recorder")
        .decide(&triage())
        .await
        .expect("recorded");

    let reworded = DecisionRequest::new(ticket()).ask(
        "department",
        Question::choice(
            "Which team owns `ticket`?",
            [("billing", Value::Null), ("shipping", Value::Null)],
        ),
    );

    let replay = ReplayDecider::replay(&path, MODEL).expect("the store exists");

    // A miss is an error, never a live call.
    assert!(matches!(
        replay.decide(&reworded).await,
        Err(DeciderError::ReplayMiss { missing }) if missing == ["department".into()]
    ));

    assert!(matches!(
        ReplayDecider::replay(&path, "jev-9.9.9").expect("the store exists").decide(&triage()).await,
        Err(DeciderError::ReplayMiss { missing }) if missing.len() == 3
    ));

    assert!(matches!(
        ReplayDecider::replay(scratch("absent"), MODEL),
        Err(ReplayError::Read { .. })
    ));

    assert_eq!(server.requests(), 1);
}

#[tokio::test]
async fn recording_asks_only_for_what_the_store_lacks() {
    let server = serve(Behaviour::Honest).await;

    let path = scratch("grow").with_extension("json");

    let recorder = ReplayDecider::record(&path, Arc::new(server.decider())).expect("a recorder");

    recorder.decide(&triage()).await.expect("recorded");

    let extended = triage().ask(
        "urgent",
        Question::noul("Does `ticket` say the matter is urgent?"),
    );

    let decision = recorder.decide(&extended).await.expect("recorded");

    assert_eq!(decision.answers.len(), 4);
    assert_eq!(server.requests(), 2);

    // Questions are keyed one by one, so only the new one was asked.
    assert_eq!(
        server.last_body()["questions"]
            .as_object()
            .expect("a map")
            .keys()
            .collect::<Vec<_>>(),
        ["urgent"]
    );
}

#[tokio::test]
async fn a_recording_that_no_longer_answers_its_question_is_refused() {
    let server = serve(Behaviour::Honest).await;

    let path = scratch("edited").with_extension("json");

    ReplayDecider::record(&path, Arc::new(server.decider()))
        .expect("a recorder")
        .decide(&DecisionRequest::new(ticket()).ask("department", department()))
        .await
        .expect("recorded");

    let edited = std::fs::read_to_string(&path)
        .expect("the store")
        .replace("\"choice\": \"billing\"", "\"choice\": \"sales\"");

    std::fs::write(&path, edited).expect("the store is writable");

    assert!(matches!(
        ReplayDecider::replay(&path, MODEL),
        Err(ReplayError::Inconsistent { .. })
    ));
}

// ---------------------------------------------------------------------
// Plugging in a backend that is not a wire-format server
// ---------------------------------------------------------------------

/// A backend with no server behind it. A model that does not speak the
/// wire format is plugged in the same way: by implementing the trait.
/// Replay, shadowing and logging compose around it unchanged.
struct Canned {
    department: &'static str,
}

#[async_trait]
impl Decider for Canned {
    fn identity(&self) -> DeciderIdentity {
        DeciderIdentity {
            backend: "canned".to_string(),
            model: "canned-1".to_string(),
            endpoint: None,
            calibrated: Some(false),
        }
    }

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
        let answers = request
            .questions
            .keys()
            .map(|id| {
                (
                    id.clone(),
                    Answer::Choice {
                        choice: self.department.to_string(),
                        probabilities: [
                            ("billing".to_string(), 0.0),
                            ("shipping".to_string(), 0.0),
                            ("account".to_string(), 0.0),
                            (self.department.to_string(), 1.0),
                        ]
                        .into(),
                        confidence: 1.0,
                    },
                )
            })
            .collect();

        Ok(Decision {
            identity: self.identity(),
            answered_by: "canned-1".to_string(),
            answers,
            usage: None,
            latency: Duration::ZERO,
            shadow: None,
        })
    }
}

#[tokio::test]
async fn any_backend_plugs_in_through_the_trait() {
    let primary = serve(Behaviour::Honest).await;

    let store = scratch("canned").with_extension("json");
    let log = scratch("canned").with_extension("jsonl");

    // Recorded, logged, and shadowed by the wire-format server: none of
    // them knows or cares what kind of backend it wraps.
    let decider = LoggingDecider::new(
        Arc::new(
            ReplayDecider::record(
                &store,
                Arc::new(ShadowDecider::new(
                    Arc::new(Canned {
                        department: "shipping",
                    }),
                    Arc::new(primary.decider()),
                )),
            )
            .expect("a recorder"),
        ),
        Arc::new(DecisionLog::open(&log).expect("a log")),
    );

    let ask = DecisionRequest::new(ticket()).ask("department", department());

    let decision = decider.decide(&ask).await.expect("a decision");

    assert_eq!(
        decision.choice("department").expect("a choice").0,
        "shipping"
    );
    assert_eq!(decision.identity.backend, "replay");

    let replayed = ReplayDecider::replay(&store, "canned-1")
        .expect("the store exists")
        .decide(&ask)
        .await
        .expect("replayed");

    assert_eq!(replayed.answers, decision.answers);

    let logged: AskRecord = serde_json::from_str(
        std::fs::read_to_string(&log)
            .expect("the log")
            .lines()
            .next()
            .expect("a record"),
    )
    .expect("a record");

    assert_eq!(
        logged.questions[&QuestionId::new("department")]
            .kind
            .to_string(),
        "choice"
    );
}

#[tokio::test]
async fn a_source_that_answers_badly_is_never_recorded() {
    let store = scratch("unrecorded").with_extension("json");

    // `sales` was never offered. The source did not check; the store
    // does.
    let recorder = ReplayDecider::record(
        &store,
        Arc::new(Canned {
            department: "sales",
        }),
    )
    .expect("a recorder");

    let error = recorder
        .decide(&DecisionRequest::new(ticket()).ask("department", department()))
        .await
        .expect_err("refused");

    assert!(
        matches!(
            &error,
            DeciderError::NonConformant { defect: AnswerDefect::UnofferedChoice { choice }, .. }
                if choice == "sales"
        ),
        "{error}"
    );

    assert!(recorder.is_empty());
    assert!(!store.exists(), "nothing was written");
}

// ---------------------------------------------------------------------
// Shadow and log
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_primary_drives_and_the_shadow_is_only_recorded() {
    let primary = serve(Behaviour::Honest).await;
    let shadow = serve(Behaviour::IgnoresCriteria).await;

    let decider = ShadowDecider::new(Arc::new(primary.decider()), Arc::new(shadow.decider()));

    let decision = decider.decide(&triage()).await.expect("a decision");

    // The shadow reads keys, not criteria, and would have chosen the
    // first one. It is recorded; it changes nothing.
    assert_eq!(
        decision.choice("department").expect("a choice").0,
        "billing"
    );
    assert_eq!(
        decider.identity().endpoint,
        primary.decider().identity().endpoint
    );

    let outcome = decision.shadow.expect("the shadow answered");

    let answers = outcome.answers.expect("answers");

    assert_eq!(
        answers[&"department".into()]
            .as_choice()
            .expect("a choice")
            .0,
        "account"
    );

    assert!(matches!(
        outcome.agreement[&"department".into()],
        Agreement::Choice {
            same_choice: false,
            ..
        }
    ));

    assert!(!outcome.agreement[&"department".into()].agrees());
    assert_eq!(primary.requests(), 1);
    assert_eq!(shadow.requests(), 1);
}

#[tokio::test]
async fn a_failing_shadow_never_fails_the_decision_and_a_failing_primary_always_does() {
    let healthy = serve(Behaviour::Honest).await;

    let dead = || {
        Arc::new(
            SystemOneHttpDecider::new("http://127.0.0.1:9", MODEL)
                .expect("a decider")
                .with_retry(fast_retries(0))
                .expect("a client"),
        )
    };

    let decision = ShadowDecider::new(Arc::new(healthy.decider()), dead())
        .decide(&triage())
        .await
        .expect("the primary answered");

    let outcome = decision.shadow.expect("the shadow was tried");

    assert!(outcome.answers.is_err());
    assert!(outcome.agreement.is_empty());

    assert!(matches!(
        ShadowDecider::new(dead(), Arc::new(healthy.decider()))
            .decide(&triage())
            .await,
        Err(DeciderError::Unavailable { .. })
    ));
}

#[tokio::test]
async fn every_ask_is_logged_with_its_distribution_its_shadow_and_no_credential() {
    let primary = serve(Behaviour::Honest).await;
    let shadow = serve(Behaviour::Floors).await;

    let var = format!("CONSEQA_TEST_KEY_{}", uuid::Uuid::new_v4().simple());

    // SAFETY: the variable's name is unique to this test.
    unsafe { std::env::set_var(&var, "s3cr3t-credential") };

    let path = scratch("log").with_extension("jsonl");

    let log = Arc::new(DecisionLog::open(&path).expect("a log"));

    let decider = LoggingDecider::new(
        Arc::new(ShadowDecider::new(
            Arc::new(
                primary
                    .decider()
                    .with_credential_from_env(&var)
                    .expect("a credential"),
            ),
            Arc::new(shadow.decider()),
        )),
        log,
    );

    decider
        .decide(&triage().tag("task", "task-7"))
        .await
        .expect("a decision");

    // A failed ask is an outcome too.
    primary.script(422, None, json!({ "error": "no" }));

    decider.decide(&triage()).await.expect_err("rejected");

    let text = std::fs::read_to_string(&path).expect("the log");

    assert!(!text.contains("s3cr3t"), "the log holds a credential");

    let records: Vec<AskRecord> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("a record"))
        .collect();

    assert_eq!(records.len(), 2);

    let asked = &records[0];

    assert_eq!(asked.tags["task"], "task-7");
    assert_eq!(asked.backend.model, MODEL);
    assert_eq!(asked.questions.len(), 3);
    assert!(asked.latency_ms.is_some());
    assert!(asked.shadow_backend.is_some());

    let department = &asked.questions[&"department".into()];

    assert!(
        matches!(&department.answer, Answer::Choice { probabilities, .. } if probabilities.len() == 3)
    );
    assert!(department.concentration.is_some());
    assert!(department.shadow_answer.is_some());
    assert!(
        department
            .agreement
            .as_ref()
            .expect("an agreement")
            .agrees()
    );

    // A noul carries no confidence, and so no concentration.
    assert!(
        asked.questions[&"refund_requested".into()]
            .concentration
            .is_none()
    );

    assert!(
        records[1]
            .error
            .as_deref()
            .expect("an error")
            .contains("422")
    );
    assert!(records[1].questions.is_empty());
    assert_eq!(records[0].state_hash, records[1].state_hash);
}

// ---------------------------------------------------------------------
// Conformance
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_honest_server_is_admissible() {
    let server = serve(Behaviour::Honest).await;

    let report = conformance::run(&server.decider()).await;

    assert!(report.admissible(), "{report:#?}");

    assert!(
        report
            .probes
            .iter()
            .all(|probe| probe.verdict == Verdict::Pass),
        "{report:#?}"
    );

    assert_eq!(report.requests, server.requests());
    assert!(report.input_tokens > 0);
}

#[tokio::test]
async fn each_misbehaviour_fails_exactly_the_probes_written_for_it() {
    let cases: [(Behaviour, &[&str]); 5] = [
        (Behaviour::Floors, &["probabilities_are_the_models"]),
        (Behaviour::AnswersTheUnaskable, &["unaskable_is_an_error"]),
        (Behaviour::ShowsTwentyOptions, &["every_option_is_offered"]),
        // A server that cannot tell options apart by their criteria
        // cannot find the right one among thirty either.
        (
            Behaviour::IgnoresCriteria,
            &["criteria_are_read", "every_option_is_offered"],
        ),
        (Behaviour::LeaksQuestions, &["questions_are_isolated"]),
    ];

    for (behaviour, expected) in cases {
        let server = serve(behaviour).await;

        let report = conformance::run(&server.decider()).await;

        assert!(
            !report.admissible(),
            "{behaviour:?} was admitted: {report:#?}"
        );

        let failed: Vec<&str> = report
            .probes
            .iter()
            .filter(|result| result.verdict == Verdict::Fail)
            .map(|result| result.probe.as_str())
            .collect();

        assert_eq!(failed, expected, "{behaviour:?}: {report:#?}");
    }
}

// ---------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------

#[test]
fn concentration_is_one_minus_normalized_entropy() {
    let choice = |probabilities: [f64; 4]| Answer::Choice {
        choice: "a".to_string(),
        probabilities: ["a", "b", "c", "d"]
            .into_iter()
            .map(str::to_string)
            .zip(probabilities)
            .collect(),
        confidence: 0.0,
    };

    let certain = choice([1.0, 0.0, 0.0, 0.0])
        .concentration()
        .expect("a statistic");
    let uniform = choice([0.25; 4]).concentration().expect("a statistic");
    let leaning = choice([0.7, 0.1, 0.1, 0.1])
        .concentration()
        .expect("a statistic");

    assert!((certain - 1.0).abs() < 1e-12);
    assert!(uniform.abs() < 1e-12);
    assert!(uniform < leaning && leaning < certain);

    assert_eq!(Answer::Noul { noul: 0.5 }.concentration(), None);
}

#[test]
fn rounding_noise_at_the_edge_of_a_range_is_not_a_defect() {
    let question = Question::choice("Which?", [("a", Value::Null), ("b", Value::Null)]);

    let answer = |confidence: f64| Answer::Choice {
        choice: "a".to_string(),
        probabilities: [("a".to_string(), 0.5), ("b".to_string(), 0.5)].into(),
        confidence,
    };

    // One minus the normalized entropy of a uniform distribution, as
    // floating point computes it.
    assert!(
        answer(-2.220446049250313e-16)
            .conforms_to(&question)
            .is_ok()
    );
    assert!(answer(1.0000000000000002).conforms_to(&question).is_ok());

    assert!(matches!(
        answer(-0.01).conforms_to(&question),
        Err(AnswerDefect::OutOfRange {
            field: "confidence",
            ..
        })
    ));

    assert!(matches!(
        answer(f64::NAN).conforms_to(&question),
        Err(AnswerDefect::OutOfRange { .. })
    ));
}

#[test]
fn agreement_is_measured_on_what_a_builder_would_act_on() {
    let noul = |noul| Answer::Noul { noul };

    // Far apart, yet both say no: a builder would have acted alike.
    assert!(Agreement::between(&noul(0.05), &noul(0.45)).agrees());

    // Close together, yet on either side of one half.
    assert!(!Agreement::between(&noul(0.49), &noul(0.51)).agrees());

    assert_eq!(
        Agreement::between(
            &noul(0.9),
            &Answer::Score {
                score: 1.0,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 1.0,
            }
        ),
        Agreement::Incomparable
    );
}
