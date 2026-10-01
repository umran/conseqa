//! The decision log (§13.2 of the System One orchestration revision).
//!
//! One JSON line per ask: who was asked, what of, and the full
//! distribution that came back — with the shadow's beside it, when
//! there is one. The state itself is referenced by hash; the replay
//! store holds the state and question text, so any logged decision can
//! be reproduced and inspected as it was asked.
//!
//! The log is what thresholds are chosen from. Recorded probabilities
//! are replayed against a candidate threshold offline, and nothing is
//! asked again while the evidence and the question meanings are
//! unchanged (§13.3). It is also where a wrong outcome is first
//! classified — missing evidence, a model error, a code error, or a
//! service failure — before anything is tuned.
//!
//! It is a run artifact, not workspace state, and it never holds a
//! credential.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::confluence::SemanticHash;

use super::{
    Agreement, Answer, Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest,
    QuestionId, QuestionKind, Usage,
};

/// What one question was asked and answered, as logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionRecord {
    pub kind: QuestionKind,

    /// The hash of the question as asked. With the state hash it
    /// addresses the exact request in the replay store.
    pub question_hash: String,

    pub answer: Answer,

    /// [`Answer::concentration`], the one statistic computed alike for
    /// every backend. Absent for a `Noul`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concentration: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_answer: Option<Answer>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreement: Option<Agreement>,
}

/// One ask, as logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskRecord {
    /// Unique to this ask, so a builder's later record of what it did
    /// with the answers can refer back to it.
    pub ask: String,

    pub at_unix_ms: u64,

    /// The caller's tags: the task, the builder, the question specs
    /// and their versions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,

    pub backend: DeciderIdentity,

    /// The model id the backend says answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,

    pub state_hash: String,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub questions: BTreeMap<QuestionId, QuestionRecord>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_backend: Option<DeciderIdentity>,

    /// Why the shadow gave no answers, when it gave none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_error: Option<String>,

    /// Why the ask failed, when it did. A failed ask is logged too: a
    /// service failure that made a builder abstain is an outcome worth
    /// counting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An append-only JSON-lines file.
#[derive(Debug)]
pub struct DecisionLog {
    path: PathBuf,
    writer: Mutex<BufWriter<File>>,
}

impl DecisionLog {
    /// Opens `path` for appending, creating it and its directory.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            path,
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one record and flushes it: a run that is killed keeps
    /// every decision it made.
    pub fn append(&self, record: &AskRecord) -> std::io::Result<()> {
        let line = serde_json::to_string(record)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

        let mut writer = self.writer.lock();

        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()
    }
}

/// A decider that logs every ask it passes on.
pub struct LoggingDecider {
    inner: Arc<dyn Decider>,
    log: Arc<DecisionLog>,
}

impl std::fmt::Debug for LoggingDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingDecider")
            .field("inner", &self.inner.identity())
            .field("log", &self.log.path())
            .finish()
    }
}

impl LoggingDecider {
    pub fn new(inner: Arc<dyn Decider>, log: Arc<DecisionLog>) -> Self {
        Self { inner, log }
    }

    fn record(
        &self,
        request: &DecisionRequest,
        outcome: &Result<Decision, DeciderError>,
    ) -> AskRecord {
        let mut record = AskRecord {
            ask: uuid::Uuid::new_v4().to_string(),
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0),
            tags: request.tags.clone(),
            backend: self.inner.identity(),
            answered_by: None,
            state_hash: SemanticHash::of(&request.state).to_hex(),
            questions: BTreeMap::new(),
            usage: None,
            latency_ms: None,
            shadow_backend: None,
            shadow_error: None,
            error: None,
        };

        let decision = match outcome {
            Ok(decision) => decision,

            Err(error) => {
                record.error = Some(error.to_string());

                return record;
            }
        };

        record.backend = decision.identity.clone();
        record.answered_by = Some(decision.answered_by.clone());
        record.usage = decision.usage;
        record.latency_ms = Some(u64::try_from(decision.latency.as_millis()).unwrap_or(u64::MAX));

        let shadow_answers = decision.shadow.as_ref().and_then(|shadow| {
            record.shadow_backend = Some(shadow.identity.clone());

            match &shadow.answers {
                Ok(answers) => Some(answers),

                Err(error) => {
                    record.shadow_error = Some(error.clone());

                    None
                }
            }
        });

        for (id, question) in &request.questions {
            let Some(answer) = decision.answers.get(id) else {
                continue;
            };

            record.questions.insert(
                id.clone(),
                QuestionRecord {
                    kind: question.kind(),
                    question_hash: SemanticHash::of(question).to_hex(),
                    concentration: answer.concentration(),
                    answer: answer.clone(),
                    shadow_answer: shadow_answers.and_then(|answers| answers.get(id).cloned()),
                    agreement: decision
                        .shadow
                        .as_ref()
                        .and_then(|shadow| shadow.agreement.get(id).cloned()),
                },
            );
        }

        record
    }
}

#[async_trait]
impl Decider for LoggingDecider {
    fn identity(&self) -> DeciderIdentity {
        self.inner.identity()
    }

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
        let outcome = self.inner.decide(request).await;

        // A log that cannot be written must not turn a good decision
        // into a failed one; it is reported, and the run continues.
        if let Err(error) = self.log.append(&self.record(request, &outcome)) {
            tracing::warn!(
                log = %self.log.path().display(),
                %error,
                "cannot append to the decision log"
            );
        }

        outcome
    }
}
