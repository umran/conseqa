//! The System One layer: typed, closed-set decisions for the harness
//! (§9–§13 of the System One orchestration revision).
//!
//! Code owns control flow. A System One model answers narrow questions
//! whose answer space code has already enumerated — pick one of these
//! options, place this on that rubric, is this statement true — and
//! returns typed answers with probabilities. It never generates a
//! value, never decides whether a specification is correct, and never
//! widens a scope: the analyzer remains the only authority on
//! correctness (§24).
//!
//! [`Decider`] is the seam. The hosted service and any local server
//! that speaks the same wire format are one backend under different
//! settings ([`SystemOneHttpDecider`]), so a new model is a
//! configuration change and never a code change. Recorded answers
//! replay deterministically for tests ([`ReplayDecider`]); two backends
//! answer every question side by side, so neither is trusted on the
//! other's behalf ([`ShadowDecider`]); and every ask is logged with its
//! full distribution ([`LoggingDecider`]).
//!
//! Typed output guarantees the interface, not the truth. What a
//! backend returns is checked against what was asked
//! ([`Answer::conforms_to`]) precisely because a backend may be
//! swapped for one that reproduces the shape and invents the numbers
//! (§11.2).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod config;
pub mod conformance;
pub mod http;
pub mod log;
pub mod preview;
pub mod questions;
pub mod replay;
pub mod shadow;

pub use config::{BackendSettings, ConfiguredDecider, DeciderKind, DeciderSettings, SettingsError};
pub use http::{
    CircuitPolicy, DeciderConfigError, HOSTED_BASE_URL, HOSTED_KEY_ENV, RequestBudget, RetryPolicy,
    SystemOneHttpDecider,
};
pub use log::{AskRecord, DecisionLog, LoggingDecider, QuestionRecord};
pub use preview::PreviewDecider;
pub use questions::QuestionSpec;
pub use replay::{ReplayDecider, ReplayError};
pub use shadow::{Agreement, ShadowDecider, ShadowOutcome};

/// Instructions, criteria and state are JSON: a string for a simple
/// judgment, an object or array where definitions, contrasts or
/// examples need names.
pub type Json = serde_json::Value;

/// The most options one `Choice` accepts on the wire. A wider
/// enumeration is ranked in chunks and re-ranked (§20.3).
pub const MAX_CHOICE_OPTIONS: usize = 255;

/// The most levels one `Score` accepts on the wire.
pub const MAX_SCORE_LEVELS: usize = 10;

/// How far a `Choice` or `Score` distribution may sum from one before
/// the answer is refused. Backends round; they do not drop mass.
const PROBABILITY_SUM_TOLERANCE: f64 = 0.05;

/// How far outside `[0, 1]` a probability may fall and still be one.
/// A statistic computed in floating point lands a rounding error past
/// the edge of its range — one minus the normalized entropy of an
/// exactly uniform distribution is `-2e-16` — and that is arithmetic,
/// not a defect.
const UNIT_TOLERANCE: f64 = 1e-9;

/// The id a question is asked under, and its answer returned under.
///
/// It is for code only. The model never sees it, so the instructions
/// and criteria must carry the complete meaning of the question (§12).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuestionId(pub String);

impl QuestionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for QuestionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for QuestionId {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

impl From<String> for QuestionId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// One typed question. The shape is the System One wire format, so a
/// question serializes as it is sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Pick one option from a set code enumerated. Each key is an
    /// option and each value describes it; `null` where the name says
    /// enough. The model cannot choose an option it was not offered,
    /// so a set that may not cover the input carries an explicit
    /// no-match option (§12).
    Choice {
        instructions: Json,
        criteria: BTreeMap<String, Json>,
    },

    /// Place the state on an ordered rubric. The level is the position
    /// in the list, from zero.
    Score {
        instructions: Json,
        criteria: Vec<Json>,
    },

    /// Is this statement true? The answer is the probability of yes.
    Noul {
        instructions: Json,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

/// What a yes and a no mean, for a `Noul` whose boundary needs saying.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulCriteria {
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub yes: Option<Json>,

    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub no: Option<Json>,
}

impl Question {
    /// A `Choice` over `(option, description)` pairs. Pass
    /// [`Json::Null`] for an option whose name needs no description.
    pub fn choice<O, D>(
        instructions: impl Into<Json>,
        options: impl IntoIterator<Item = (O, D)>,
    ) -> Self
    where
        O: Into<String>,
        D: Into<Json>,
    {
        Self::Choice {
            instructions: instructions.into(),
            criteria: options
                .into_iter()
                .map(|(option, description)| (option.into(), description.into()))
                .collect(),
        }
    }

    /// A `Score` over ordered level descriptions, lowest first.
    pub fn score<L: Into<Json>>(
        instructions: impl Into<Json>,
        levels: impl IntoIterator<Item = L>,
    ) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria: levels.into_iter().map(Into::into).collect(),
        }
    }

    /// A `Noul` with no stated criteria.
    pub fn noul(instructions: impl Into<Json>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    /// A `Noul` that says what a yes and a no mean.
    pub fn noul_with_criteria(
        instructions: impl Into<Json>,
        yes: impl Into<Json>,
        no: impl Into<Json>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                yes: Some(yes.into()),
                no: Some(no.into()),
            }),
        }
    }

    pub fn kind(&self) -> QuestionKind {
        match self {
            Self::Choice { .. } => QuestionKind::Choice,
            Self::Score { .. } => QuestionKind::Score,
            Self::Noul { .. } => QuestionKind::Noul,
        }
    }

    fn instructions(&self) -> &Json {
        match self {
            Self::Choice { instructions, .. }
            | Self::Score { instructions, .. }
            | Self::Noul { instructions, .. } => instructions,
        }
    }

    /// Whether the question can be asked at all. Checked before any
    /// request leaves the process: a malformed question is a code
    /// error, and is never left for a server to interpret.
    pub fn validate(&self) -> Result<(), QuestionDefect> {
        if is_blank(self.instructions()) {
            return Err(QuestionDefect::EmptyInstructions);
        }

        match self {
            Self::Choice { criteria, .. } => {
                if criteria.len() < 2 {
                    return Err(QuestionDefect::TooFewOptions {
                        found: criteria.len(),
                    });
                }

                if criteria.len() > MAX_CHOICE_OPTIONS {
                    return Err(QuestionDefect::TooManyOptions {
                        found: criteria.len(),
                    });
                }

                if criteria.keys().any(|option| option.trim().is_empty()) {
                    return Err(QuestionDefect::EmptyOptionName);
                }
            }

            Self::Score { criteria, .. } => {
                if criteria.len() < 2 {
                    return Err(QuestionDefect::TooFewLevels {
                        found: criteria.len(),
                    });
                }

                if criteria.len() > MAX_SCORE_LEVELS {
                    return Err(QuestionDefect::TooManyLevels {
                        found: criteria.len(),
                    });
                }
            }

            Self::Noul { .. } => {}
        }

        Ok(())
    }
}

fn is_blank(value: &Json) -> bool {
    match value {
        Json::Null => true,
        Json::String(text) => text.trim().is_empty(),
        Json::Array(items) => items.is_empty(),
        Json::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    Choice,
    Score,
    Noul,
}

impl fmt::Display for QuestionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Choice => "choice",
            Self::Score => "score",
            Self::Noul => "noul",
        })
    }
}

/// Why a question cannot be asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuestionDefect {
    #[error("the instructions are empty")]
    EmptyInstructions,

    #[error("a choice needs at least two options, found {found}")]
    TooFewOptions { found: usize },

    #[error("a choice accepts at most {MAX_CHOICE_OPTIONS} options, found {found}")]
    TooManyOptions { found: usize },

    #[error("an option name is empty")]
    EmptyOptionName,

    #[error("a score needs at least two levels, found {found}")]
    TooFewLevels { found: usize },

    #[error("a score accepts at most {MAX_SCORE_LEVELS} levels, found {found}")]
    TooManyLevels { found: usize },
}

/// One typed answer, in the System One wire format.
///
/// Unknown fields are tolerated: a backend may report more than this
/// layer reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        /// The highest-probability option.
        choice: String,

        /// Every option mapped to its probability.
        probabilities: BTreeMap<String, f64>,

        /// The backend's own statistic over `probabilities`. Its
        /// computation is the backend's and is not comparable across
        /// backends; [`Answer::concentration`] is.
        confidence: f64,
    },

    Score {
        /// The probability-weighted level; it may fall between levels.
        score: f64,

        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        legend: BTreeMap<String, Json>,

        /// Each level index, as a string key, mapped to its probability.
        probabilities: BTreeMap<String, f64>,

        confidence: f64,
    },

    /// The probability that the answer is yes. A value near one half
    /// means yes and no are similarly probable — not that the condition
    /// holds to a medium degree (§10). A `Noul` carries no confidence.
    Noul { noul: f64 },
}

impl Answer {
    pub fn kind(&self) -> QuestionKind {
        match self {
            Self::Choice { .. } => QuestionKind::Choice,
            Self::Score { .. } => QuestionKind::Score,
            Self::Noul { .. } => QuestionKind::Noul,
        }
    }

    /// The chosen option and its distribution, when this is a `Choice`.
    pub fn as_choice(&self) -> Option<(&str, &BTreeMap<String, f64>)> {
        match self {
            Self::Choice {
                choice,
                probabilities,
                ..
            } => Some((choice, probabilities)),
            _ => None,
        }
    }

    /// The probability of yes, when this is a `Noul`.
    pub fn as_noul(&self) -> Option<f64> {
        match self {
            Self::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    /// The weighted level, when this is a `Score`.
    pub fn as_score(&self) -> Option<f64> {
        match self {
            Self::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// The backend's own confidence; `None` for a `Noul`.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Choice { confidence, .. } | Self::Score { confidence, .. } => Some(*confidence),
            Self::Noul { .. } => None,
        }
    }

    /// How concentrated the distribution is: one minus its normalized
    /// Shannon entropy — one when all mass is on one outcome, zero when
    /// it is uniform. `None` for a `Noul`.
    ///
    /// Computed here, identically for every backend, so that two
    /// backends can be compared on one statistic (§11.5). It summarizes
    /// the shape of a distribution. It is neither correctness nor
    /// permission to act: several acceptable options spread
    /// probability as readily as ignorance does (§10).
    pub fn concentration(&self) -> Option<f64> {
        let probabilities = match self {
            Self::Choice { probabilities, .. } | Self::Score { probabilities, .. } => probabilities,
            Self::Noul { .. } => return None,
        };

        let outcomes = probabilities.len();

        if outcomes <= 1 {
            return Some(1.0);
        }

        let entropy: f64 = probabilities
            .values()
            .filter(|probability| **probability > 0.0)
            .map(|probability| -probability * probability.ln())
            .sum();

        Some((1.0 - entropy / (outcomes as f64).ln()).clamp(0.0, 1.0))
    }

    /// Whether this answers the question that was asked: the same
    /// type, every option or level reported and none invented, and
    /// every number a probability.
    pub fn conforms_to(&self, question: &Question) -> Result<(), AnswerDefect> {
        match (question, self) {
            (
                Question::Choice { criteria, .. },
                Self::Choice {
                    choice,
                    probabilities,
                    confidence,
                },
            ) => {
                if !criteria.contains_key(choice) {
                    return Err(AnswerDefect::UnofferedChoice {
                        choice: choice.clone(),
                    });
                }

                if let Some(option) = probabilities
                    .keys()
                    .find(|key| !criteria.contains_key(*key))
                {
                    return Err(AnswerDefect::UnofferedOption {
                        option: option.clone(),
                    });
                }

                if let Some(option) = criteria
                    .keys()
                    .find(|key| !probabilities.contains_key(*key))
                {
                    return Err(AnswerDefect::UnreportedOption {
                        option: option.clone(),
                    });
                }

                check_distribution(probabilities)?;
                check_unit("confidence", *confidence)?;

                let highest = probabilities.values().copied().fold(0.0, f64::max);

                if probabilities[choice] + 1e-9 < highest {
                    return Err(AnswerDefect::ChoiceIsNotMostProbable {
                        choice: choice.clone(),
                    });
                }

                Ok(())
            }

            (
                Question::Score { criteria, .. },
                Self::Score {
                    score,
                    probabilities,
                    confidence,
                    ..
                },
            ) => {
                let expected: Vec<String> =
                    (0..criteria.len()).map(|level| level.to_string()).collect();

                if probabilities.len() != expected.len()
                    || expected
                        .iter()
                        .any(|level| !probabilities.contains_key(level))
                {
                    return Err(AnswerDefect::LevelsMismatch {
                        expected: criteria.len(),
                        found: probabilities.len(),
                    });
                }

                check_distribution(probabilities)?;
                check_unit("confidence", *confidence)?;

                let top = (criteria.len() - 1) as f64;

                if !score.is_finite() || *score < 0.0 || *score > top {
                    return Err(AnswerDefect::OutOfRange {
                        field: "score",
                        value: *score,
                    });
                }

                Ok(())
            }

            (Question::Noul { .. }, Self::Noul { noul }) => check_unit("noul", *noul),

            (asked, answered) => Err(AnswerDefect::WrongType {
                asked: asked.kind(),
                answered: answered.kind(),
            }),
        }
    }
}

fn check_unit(field: &'static str, value: f64) -> Result<(), AnswerDefect> {
    if value.is_finite() && (-UNIT_TOLERANCE..=1.0 + UNIT_TOLERANCE).contains(&value) {
        Ok(())
    } else {
        Err(AnswerDefect::OutOfRange { field, value })
    }
}

fn check_distribution(probabilities: &BTreeMap<String, f64>) -> Result<(), AnswerDefect> {
    for probability in probabilities.values() {
        check_unit("probability", *probability)?;
    }

    let sum: f64 = probabilities.values().sum();

    if (sum - 1.0).abs() > PROBABILITY_SUM_TOLERANCE {
        return Err(AnswerDefect::NotADistribution { sum });
    }

    Ok(())
}

/// Why an answer does not answer the question that was asked.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AnswerDefect {
    #[error("asked a {asked}, answered a {answered}")]
    WrongType {
        asked: QuestionKind,
        answered: QuestionKind,
    },

    #[error("the chosen option `{choice}` was never offered")]
    UnofferedChoice { choice: String },

    #[error("a probability is reported for `{option}`, which was never offered")]
    UnofferedOption { option: String },

    #[error("no probability is reported for the offered option `{option}`")]
    UnreportedOption { option: String },

    #[error("the chosen option `{choice}` is not the most probable one")]
    ChoiceIsNotMostProbable { choice: String },

    #[error("expected probabilities over {expected} levels, found {found}")]
    LevelsMismatch { expected: usize, found: usize },

    #[error("`{field}` is {value}, outside its range")]
    OutOfRange { field: &'static str, value: f64 },

    #[error("the probabilities sum to {sum}, not one")]
    NotADistribution { sum: f64 },
}

/// One state and the questions asked of it.
///
/// Every question sees the same state and none sees another's answer,
/// so independent and speculative questions belong in one request; a
/// second request is warranted only when an earlier answer is needed
/// to build new state or new options (§12).
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRequest {
    /// Only what the questions need (§24): unrelated detail costs
    /// accuracy as well as tokens.
    pub state: Json,

    pub questions: BTreeMap<QuestionId, Question>,

    /// Who is asking and why — the task, the builder, the question
    /// specs and their versions. Logged with the ask; never sent to a
    /// backend, and no part of a replay key.
    pub tags: BTreeMap<String, String>,
}

impl DecisionRequest {
    pub fn new(state: impl Into<Json>) -> Self {
        Self {
            state: state.into(),
            questions: BTreeMap::new(),
            tags: BTreeMap::new(),
        }
    }

    pub fn ask(mut self, id: impl Into<QuestionId>, question: Question) -> Self {
        self.questions.insert(id.into(), question);
        self
    }

    pub fn tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    /// Whether the request can be sent: at least one question, and
    /// every question well formed.
    pub fn validate(&self) -> Result<(), DeciderError> {
        if self.questions.is_empty() {
            return Err(DeciderError::NoQuestions);
        }

        for (id, question) in &self.questions {
            question
                .validate()
                .map_err(|defect| DeciderError::InvalidQuestion {
                    question: id.clone(),
                    defect,
                })?;
        }

        Ok(())
    }
}

/// Which backend answered, and with what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeciderIdentity {
    /// The backend kind: `system_one_http`, `replay`, `shadow`.
    pub backend: String,

    /// The pinned model id the backend asks for. Thresholds are tuned
    /// per backend and per id, never carried from one to another (§10).
    pub model: String,

    /// Where the backend sends its requests, when it sends any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Whether the probabilities are known to be calibrated. `None`
    /// when the backend does not say (§11.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibrated: Option<bool>,
}

/// Token usage a backend reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,

    #[serde(default)]
    pub output_tokens: u64,
}

/// The answers to one request.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub identity: DeciderIdentity,

    /// The model id the backend says answered. It differs from
    /// `identity.model` when an alias was resolved.
    pub answered_by: String,

    /// One answer per question asked, each already checked against its
    /// question.
    pub answers: BTreeMap<QuestionId, Answer>,

    pub usage: Option<Usage>,

    /// End to end, retries included.
    pub latency: Duration,

    /// What the shadow backend answered to the same request, when one
    /// is configured. It never drives behaviour (§11.5).
    pub shadow: Option<ShadowOutcome>,
}

impl Decision {
    pub fn answer(&self, id: &str) -> Option<&Answer> {
        self.answers.get(&QuestionId(id.to_string()))
    }

    /// The probability of yes for a `Noul` asked under `id`.
    pub fn noul(&self, id: &str) -> Option<f64> {
        self.answer(id).and_then(Answer::as_noul)
    }

    /// The chosen option and its distribution for a `Choice` asked
    /// under `id`.
    pub fn choice(&self, id: &str) -> Option<(&str, &BTreeMap<String, f64>)> {
        self.answer(id).and_then(Answer::as_choice)
    }

    /// The weighted level for a `Score` asked under `id`.
    pub fn score(&self, id: &str) -> Option<f64> {
        self.answer(id).and_then(Answer::as_score)
    }
}

/// Why a decision could not be obtained. It is never silently
/// absorbed: a builder that cannot obtain an answer abstains (§16).
#[derive(Debug, thiserror::Error)]
pub enum DeciderError {
    #[error("the request asks no question")]
    NoQuestions,

    #[error("question `{question}` cannot be asked: {defect}")]
    InvalidQuestion {
        question: QuestionId,
        defect: QuestionDefect,
    },

    /// Refused here rather than truncated there: the builder must
    /// slice its state (§11.1).
    #[error("{what} is an estimated {estimated_tokens} tokens, over the budget of {budget}")]
    Oversize {
        what: &'static str,
        estimated_tokens: u64,
        budget: u64,
    },

    #[error("the backend refused the credential (HTTP {status})")]
    Unauthorized { status: u16 },

    /// A request the backend will never accept. Retrying cannot help.
    #[error("the backend rejected the request (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },

    #[error("the backend is unavailable after {attempts} attempts: {last}")]
    Unavailable { attempts: u32, last: String },

    #[error("the backend's circuit is open for another {remaining:?} after repeated failures")]
    CircuitOpen { remaining: Duration },

    #[error("the backend's response is not a System One response: {0}")]
    Malformed(String),

    #[error("no answer was returned for question `{question}`")]
    Unanswered { question: QuestionId },

    /// The backend answered a different question from the one asked.
    #[error("the answer to `{question}` does not answer it: {defect}")]
    NonConformant {
        question: QuestionId,
        defect: AnswerDefect,
    },

    /// Replay mode never makes a live call (§11.4).
    #[error("no recording answers: {}", display_ids(missing))]
    ReplayMiss { missing: Vec<QuestionId> },

    /// The preview decider writes what would be sent, and sends nothing.
    #[error("preview only: the request was written down and not sent")]
    PreviewOnly,

    #[error(transparent)]
    Replay(#[from] ReplayError),
}

fn display_ids(ids: &[QuestionId]) -> String {
    ids.iter()
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks every question has an answer that answers it. Shared by the
/// backends so none returns an unchecked answer.
pub(crate) fn check_answers(
    request: &DecisionRequest,
    answers: &BTreeMap<QuestionId, Answer>,
) -> Result<(), DeciderError> {
    for (id, question) in &request.questions {
        let answer = answers.get(id).ok_or_else(|| DeciderError::Unanswered {
            question: id.clone(),
        })?;

        answer
            .conforms_to(question)
            .map_err(|defect| DeciderError::NonConformant {
                question: id.clone(),
                defect,
            })?;
    }

    Ok(())
}

/// A source of typed decisions.
///
/// Implementations differ in where answers come from — a hosted
/// service, a local server, a recording — and in nothing a caller can
/// observe except [`DeciderIdentity`]. Every implementation returns
/// only answers that conform to their questions.
#[async_trait]
pub trait Decider: Send + Sync {
    fn identity(&self) -> DeciderIdentity;

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError>;
}
