//! The wire-format backend (§11.1 of the System One orchestration
//! revision).
//!
//! ```text
//! POST {base_url}/v1/systemone
//! { "state": ..., "model": ..., "questions": { <id>: <question> } }
//!     -> { "model": ..., "answers": { <id>: <answer> }, "usage": ... }
//! ```
//!
//! The wire format is the contract, not the vendor. This one client
//! addresses the hosted service and any local server that implements
//! the format; base URL, model id and credential are settings, so the
//! two differ in nothing else. A base URL that is not loopback is data
//! egress — state slices leave the machine — which is why there is no
//! default URL and the caller must name one (§11.6).
//!
//! Retry defaults mirror the vendor's own client: two retries of `408`,
//! `429` and every `5xx`, connection failures and timeouts, backing
//! off from half a second and honouring `retry-after`.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::{
    Answer, Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest, Json, Question,
    QuestionId, Usage, check_answers,
};

/// The hosted service. Named by callers explicitly; never a default.
pub const HOSTED_BASE_URL: &str = "https://api.typesafe.ai";

/// The environment variable the hosted service's credential is
/// conventionally read from.
pub const HOSTED_KEY_ENV: &str = "TYPESAFE_API_KEY";

const ENDPOINT_PATH: &str = "/v1/systemone";

/// How much of a rejection body is kept for the error message.
const REJECTION_BODY_LIMIT: usize = 2048;

/// A bearer credential. It is read from the environment and written
/// nowhere: not to a prompt, the decision log, the manifest or the
/// replay store (§11.1).
#[derive(Clone)]
struct Credential(String);

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

/// When a failed attempt is tried again.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Retries after the first attempt; zero disables them.
    pub max_retries: u32,

    /// The first delay, doubled each attempt up to `backoff_max`.
    pub backoff_initial: Duration,

    pub backoff_max: Duration,

    /// The fraction of each delay that is randomized, so concurrent
    /// builders do not retry in step.
    pub backoff_jitter: f64,

    /// Whether a `retry-after` header lengthens the delay.
    pub respect_retry_after: bool,

    /// The longest a `retry-after` header is honoured for. A server
    /// cannot stall a run indefinitely.
    pub retry_after_cap: Duration,

    /// The timeout of one attempt.
    pub timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            respect_retry_after: true,
            retry_after_cap: Duration::from_secs(30),
            timeout: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// No retries: one attempt decides the outcome.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    fn retries_status(status: u16) -> bool {
        status == 408 || status == 429 || (500..600).contains(&status)
    }

    /// The delay before retry number `retry`, counted from one.
    fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        let doubled = self
            .backoff_initial
            .saturating_mul(2_u32.saturating_pow(retry.saturating_sub(1)));

        let base = doubled.min(self.backoff_max);

        let spread = self.backoff_jitter.clamp(0.0, 1.0) * (2.0 * unit_random() - 1.0);

        let jittered = base.mul_f64((1.0 + spread).max(0.0));

        match retry_after {
            Some(requested) if self.respect_retry_after => {
                jittered.max(requested.min(self.retry_after_cap))
            }
            _ => jittered,
        }
    }
}

/// A number in `[0, 1)` from operating-system randomness.
fn unit_random() -> f64 {
    let mut bytes = [0_u8; 4];

    // Jitter only spreads retries apart; without randomness the
    // midpoint is a correct, merely unspread, delay.
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }

    f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
}

/// How large a request may be. The backend never truncates: a request
/// over budget is refused here, and the builder must slice its state
/// (§11.1).
///
/// Tokens are estimated from bytes, conservatively, because the
/// backend's tokenizer is not available to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestBudget {
    /// The state and every question together.
    pub max_request_tokens: u64,

    /// The state and the single longest question.
    pub max_state_tokens: u64,

    /// Bytes assumed per token. Three over-estimates English and JSON
    /// alike, so a request inside the estimate is inside the limit.
    pub bytes_per_token: u64,
}

impl RequestBudget {
    /// The hosted service's limits.
    pub fn hosted() -> Self {
        Self {
            max_request_tokens: 64_000,
            max_state_tokens: 32_000,
            bytes_per_token: 3,
        }
    }

    /// No client-side limit, for a local server whose limits are its
    /// own to report.
    pub fn unlimited() -> Self {
        Self {
            max_request_tokens: u64::MAX,
            max_state_tokens: u64::MAX,
            bytes_per_token: 3,
        }
    }

    fn tokens(&self, bytes: usize) -> u64 {
        (bytes as u64).div_ceil(self.bytes_per_token.max(1))
    }

    fn check(&self, request: &DecisionRequest) -> Result<(), DeciderError> {
        let state = self.tokens(serialized_len(&request.state));

        let questions: Vec<u64> = request
            .questions
            .values()
            .map(|question| self.tokens(serialized_len(question)))
            .collect();

        let longest = questions.iter().copied().max().unwrap_or(0);

        let whole = state.saturating_add(questions.iter().sum());

        if state.saturating_add(longest) > self.max_state_tokens {
            return Err(DeciderError::Oversize {
                what: "the state with its longest question",
                estimated_tokens: state.saturating_add(longest),
                budget: self.max_state_tokens,
            });
        }

        if whole > self.max_request_tokens {
            return Err(DeciderError::Oversize {
                what: "the state with all of its questions",
                estimated_tokens: whole,
                budget: self.max_request_tokens,
            });
        }

        Ok(())
    }
}

fn serialized_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

/// When the backend stops being asked.
///
/// After `failure_threshold` consecutive failures the circuit opens
/// and every ask fails at once for `cool_down`, so builders abstain to
/// the agent backend instead of each waiting out its own retries
/// (§16). One success closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircuitPolicy {
    pub failure_threshold: u32,
    pub cool_down: Duration,
}

impl Default for CircuitPolicy {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            cool_down: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Default)]
struct CircuitState {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
}

#[derive(Debug)]
struct Circuit {
    policy: CircuitPolicy,
    state: Mutex<CircuitState>,
}

impl Circuit {
    fn new(policy: CircuitPolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(CircuitState::default()),
        }
    }

    /// Refuses while open. Once the cool-down has passed the next ask
    /// goes through as a trial; its outcome closes or re-opens the
    /// circuit.
    fn admit(&self) -> Result<(), DeciderError> {
        let state = self.state.lock();

        let Some(opened_at) = state.opened_at else {
            return Ok(());
        };

        let elapsed = opened_at.elapsed();

        if elapsed < self.policy.cool_down {
            return Err(DeciderError::CircuitOpen {
                remaining: self.policy.cool_down - elapsed,
            });
        }

        Ok(())
    }

    fn succeeded(&self) {
        *self.state.lock() = CircuitState::default();
    }

    fn failed(&self) {
        let mut state = self.state.lock();

        state.consecutive_failures = state.consecutive_failures.saturating_add(1);

        if state.consecutive_failures >= self.policy.failure_threshold {
            state.opened_at = Some(Instant::now());
        }
    }
}

/// Why a decider cannot be constructed.
#[derive(Debug, thiserror::Error)]
pub enum DeciderConfigError {
    #[error("`{0}` is not an http or https base URL")]
    InvalidBaseUrl(String),

    #[error("the environment variable `{var}` holds no credential")]
    MissingCredential { var: String },

    #[error("cannot read a credential from {path}: {reason}")]
    UnreadableCredentialFile { path: String, reason: String },

    /// An alias moves when a release ships, so the answers behind it
    /// change with no change here, and thresholds tuned against one
    /// model are silently applied to another (§11.1).
    #[error("`{model}` is an alias; pin a versioned model id")]
    AliasedModel { model: String },

    #[error("cannot build the HTTP client: {0}")]
    Client(String),
}

/// A decider over the System One wire format.
#[derive(Debug)]
pub struct SystemOneHttpDecider {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    model: String,
    credential: Option<Credential>,
    retry: RetryPolicy,
    budget: RequestBudget,
    circuit: Circuit,
    calibrated: Option<bool>,
}

impl SystemOneHttpDecider {
    /// A decider for the server at `base_url`, asking for `model`.
    ///
    /// The model id must be versioned. Use [`Self::with_alias_allowed`]
    /// only where no threshold depends on the answer.
    pub fn new(base_url: &str, model: impl Into<String>) -> Result<Self, DeciderConfigError> {
        Self::build(base_url, model.into(), false)
    }

    /// As [`Self::new`], accepting an alias such as `jev-latest`. For a
    /// connectivity probe; never for a run whose thresholds were tuned.
    pub fn with_alias_allowed(
        base_url: &str,
        model: impl Into<String>,
    ) -> Result<Self, DeciderConfigError> {
        Self::build(base_url, model.into(), true)
    }

    fn build(base_url: &str, model: String, allow_alias: bool) -> Result<Self, DeciderConfigError> {
        if !allow_alias && is_alias(&model) {
            return Err(DeciderConfigError::AliasedModel { model });
        }

        let invalid = || DeciderConfigError::InvalidBaseUrl(base_url.to_string());

        let base = reqwest::Url::parse(base_url).map_err(|_| invalid())?;

        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(invalid());
        }

        let endpoint = base
            .join(&format!(
                "{}{ENDPOINT_PATH}",
                base.path().trim_end_matches('/')
            ))
            .map_err(|_| invalid())?;

        let retry = RetryPolicy::default();

        Ok(Self {
            client: build_client(retry.timeout)?,
            // A loopback server reports its own limits; the hosted
            // limits are the conservative default everywhere else.
            budget: if is_loopback(&endpoint) {
                RequestBudget::unlimited()
            } else {
                RequestBudget::hosted()
            },
            endpoint,
            model,
            credential: None,
            retry,
            circuit: Circuit::new(CircuitPolicy::default()),
            calibrated: None,
        })
    }

    /// Reads the bearer credential from the environment variable `var`.
    pub fn with_credential_from_env(mut self, var: &str) -> Result<Self, DeciderConfigError> {
        let value = std::env::var(var).unwrap_or_default();

        if value.trim().is_empty() {
            return Err(DeciderConfigError::MissingCredential {
                var: var.to_string(),
            });
        }

        self.credential = Some(Credential(value.trim().to_string()));

        Ok(self)
    }

    /// Reads the bearer credential from a file holding only the key.
    /// Keeps the secret out of client configs that are rewritten by
    /// the application that owns them.
    pub fn with_credential_from_file(
        mut self,
        path: &std::path::Path,
    ) -> Result<Self, DeciderConfigError> {
        let unreadable = |reason: String| DeciderConfigError::UnreadableCredentialFile {
            path: path.display().to_string(),
            reason,
        };

        let value = std::fs::read_to_string(path).map_err(|error| unreadable(error.to_string()))?;

        if value.trim().is_empty() {
            return Err(unreadable("the file is empty".to_string()));
        }

        self.credential = Some(Credential(value.trim().to_string()));

        Ok(self)
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Result<Self, DeciderConfigError> {
        self.client = build_client(retry.timeout)?;
        self.retry = retry;

        Ok(self)
    }

    pub fn with_budget(mut self, budget: RequestBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_circuit(mut self, policy: CircuitPolicy) -> Self {
        self.circuit = Circuit::new(policy);
        self
    }

    /// Records whether this server's probabilities are calibrated, for
    /// the decision log. A stock model read through its logits is not
    /// (§11.2).
    pub fn with_calibration(mut self, calibrated: bool) -> Self {
        self.calibrated = Some(calibrated);
        self
    }

    /// Whether asking this decider sends state off the machine.
    pub fn is_egress(&self) -> bool {
        !is_loopback(&self.endpoint)
    }

    /// Sends `body` as it is — unvalidated, once, with no retry — and
    /// returns the status and body. For the conformance suite, which
    /// must see how a server answers a request this client would never
    /// send (§32).
    pub async fn post_raw(&self, body: &Json) -> Result<(u16, String), DeciderError> {
        let response =
            self.request(body)
                .send()
                .await
                .map_err(|error| DeciderError::Unavailable {
                    attempts: 1,
                    last: describe(error),
                })?;

        let status = response.status().as_u16();

        let text = response.text().await.unwrap_or_default();

        Ok((status, text))
    }

    fn request<B: Serialize + ?Sized>(&self, body: &B) -> reqwest::RequestBuilder {
        let builder = self.client.post(self.endpoint.clone()).json(body);

        match &self.credential {
            Some(credential) => builder.bearer_auth(&credential.0),
            None => builder,
        }
    }

    async fn attempt(&self, body: &WireRequest<'_>) -> Result<WireResponse, Attempt> {
        let response = self.request(body).send().await.map_err(|error| {
            // A request that could not be built will never succeed;
            // anything else is the network, and may.
            if error.is_builder() {
                Attempt::Fatal(DeciderError::Malformed(describe(error)))
            } else {
                Attempt::Retryable {
                    reason: describe(error),
                    retry_after: None,
                }
            }
        })?;

        let status = response.status().as_u16();

        if response.status().is_success() {
            let text = response.text().await.map_err(|error| Attempt::Retryable {
                reason: describe(error),
                retry_after: None,
            })?;

            return serde_json::from_str(&text)
                .map_err(|error| Attempt::Fatal(DeciderError::Malformed(error.to_string())));
        }

        if RetryPolicy::retries_status(status) {
            return Err(Attempt::Retryable {
                reason: format!("HTTP {status}"),
                retry_after: retry_after(&response),
            });
        }

        if status == 401 || status == 403 {
            return Err(Attempt::Fatal(DeciderError::Unauthorized { status }));
        }

        let mut body = response.text().await.unwrap_or_default();

        if body.len() > REJECTION_BODY_LIMIT {
            let mut end = REJECTION_BODY_LIMIT;

            while !body.is_char_boundary(end) {
                end -= 1;
            }

            body.truncate(end);
            body.push('…');
        }

        Err(Attempt::Fatal(DeciderError::Rejected { status, body }))
    }

    async fn ask(&self, body: &WireRequest<'_>) -> Result<WireResponse, DeciderError> {
        let mut retries = 0;

        loop {
            match self.attempt(body).await {
                Ok(response) => return Ok(response),

                Err(Attempt::Fatal(error)) => return Err(error),

                Err(Attempt::Retryable {
                    reason,
                    retry_after,
                }) => {
                    if retries >= self.retry.max_retries {
                        return Err(DeciderError::Unavailable {
                            attempts: retries + 1,
                            last: reason,
                        });
                    }

                    retries += 1;

                    tokio::time::sleep(self.retry.delay(retries, retry_after)).await;
                }
            }
        }
    }
}

enum Attempt {
    Fatal(DeciderError),

    Retryable {
        reason: String,
        retry_after: Option<Duration>,
    },
}

/// The request body, as posted. The preview decider writes this same
/// type, so what it shows is what would have been sent.
#[derive(Serialize)]
pub(super) struct WireRequest<'a> {
    pub(super) state: &'a Json,
    pub(super) model: &'a str,
    pub(super) questions: &'a BTreeMap<QuestionId, Question>,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: Option<String>,

    answers: BTreeMap<QuestionId, Answer>,

    #[serde(default)]
    usage: Option<Usage>,
}

#[async_trait]
impl Decider for SystemOneHttpDecider {
    fn identity(&self) -> DeciderIdentity {
        DeciderIdentity {
            backend: "system_one_http".to_string(),
            model: self.model.clone(),
            endpoint: Some(self.endpoint.to_string()),
            calibrated: self.calibrated,
        }
    }

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
        // A malformed or oversize request is the caller's error and
        // says nothing about the backend's health.
        request.validate()?;
        self.budget.check(request)?;

        self.circuit.admit()?;

        let started = Instant::now();

        let body = WireRequest {
            state: &request.state,
            model: &self.model,
            questions: &request.questions,
        };

        let response = match self.ask(&body).await {
            Ok(response) => response,

            Err(error) => {
                // Only the backend's failures open the circuit. A
                // rejected request or a refused credential would fail
                // identically against a healthy backend.
                if matches!(
                    error,
                    DeciderError::Unavailable { .. } | DeciderError::Malformed(_)
                ) {
                    self.circuit.failed();
                }

                return Err(error);
            }
        };

        // A backend that answers a different question from the one
        // asked is a broken backend, however healthy its transport.
        if let Err(error) = check_answers(request, &response.answers) {
            self.circuit.failed();

            return Err(error);
        }

        self.circuit.succeeded();

        let mut answers = response.answers;

        // Answers to questions that were never asked are dropped
        // rather than trusted.
        answers.retain(|id, _| request.questions.contains_key(id));

        Ok(Decision {
            identity: self.identity(),
            answered_by: response.model.unwrap_or_else(|| self.model.clone()),
            answers,
            usage: response.usage,
            latency: started.elapsed(),
            shadow: None,
        })
    }
}

fn build_client(timeout: Duration) -> Result<reqwest::Client, DeciderConfigError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("conseqa/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| DeciderConfigError::Client(error.to_string()))
}

/// An alias resolves to whichever release is current.
fn is_alias(model: &str) -> bool {
    model.ends_with("-latest") || model.ends_with("-preview")
}

fn is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };

    // An IPv6 host is bracketed in a URL.
    let host = host.trim_start_matches('[').trim_end_matches(']');

    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// The delay a `retry-after` header asks for, when it is given in
/// seconds.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

/// A transport error by its root cause, without its URL: the endpoint
/// is already known, and a URL is where a credential would leak from
/// if one were ever put in it.
fn describe(error: reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else if error.is_body() || error.is_decode() {
        "could not read the response"
    } else {
        "transport error"
    };

    let error = error.without_url();

    let mut cause: &dyn std::error::Error = &error;

    while let Some(next) = cause.source() {
        cause = next;
    }

    format!("{kind}: {cause}")
}
