//! The replay backend (§11.4 of the System One orchestration
//! revision).
//!
//! A content-addressed store of answers. Each answer is keyed by the
//! model id, the state and the question it answers — the question's
//! own text, so a reworded question misses and a stale recording can
//! never answer it.
//!
//! Questions are keyed one by one, not request by request. That is
//! sound because a System One backend answers each question in
//! isolation: what else was asked beside it cannot change its answer
//! (§11.2). It is also what keeps a recording useful when a builder
//! later asks one more question of the same state.
//!
//! In *replay* mode a miss is an error and never a live call, which is
//! what makes tests and CI deterministic (§24). In *record* mode the
//! source is asked only for what the store lacks.
//!
//! The store keeps each state and question as it was asked, so any
//! recorded decision can be reproduced and inspected later (§13.2). It
//! never holds a credential.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::confluence::SemanticHash;

use super::{
    Answer, Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest, Json, Question,
    QuestionId, check_answers,
};

const STORE_FORMAT: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("cannot read the replay store {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot write the replay store {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("the replay store {path} is not a replay store: {reason}")]
    Corrupt { path: PathBuf, reason: String },

    #[error("the replay store {path} is format {found}; this build reads format {STORE_FORMAT}")]
    Format { path: PathBuf, found: u32 },

    /// A recording that no longer answers its own question was edited
    /// by hand, or written by a build that checked less.
    #[error("the recording `{key}` in {path} does not answer its question: {reason}")]
    Inconsistent {
        path: PathBuf,
        key: String,
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreFile {
    format: u32,

    /// States by hash, held once however many questions were asked of
    /// each.
    states: BTreeMap<String, Json>,

    entries: BTreeMap<String, Entry>,
}

impl Default for StoreFile {
    fn default() -> Self {
        Self {
            format: STORE_FORMAT,
            states: BTreeMap::new(),
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    model: String,

    /// The hash of the state this was asked of, into `states`.
    state: String,

    question: Question,
    answer: Answer,
}

/// A decider that answers from recordings.
pub struct ReplayDecider {
    path: PathBuf,
    model: String,

    /// The backend asked for what the store lacks. `None` in replay
    /// mode, where nothing is ever asked.
    source: Option<Arc<dyn Decider>>,

    store: Mutex<StoreFile>,
}

impl std::fmt::Debug for ReplayDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayDecider")
            .field("path", &self.path)
            .field("model", &self.model)
            .field("recording", &self.source.is_some())
            .finish_non_exhaustive()
    }
}

impl ReplayDecider {
    /// Answers only from the recordings at `path`, which were made
    /// against `model`. The store must exist: an absent store is a
    /// mistake, not an empty one.
    pub fn replay(path: impl Into<PathBuf>, model: impl Into<String>) -> Result<Self, ReplayError> {
        let path = path.into();

        let store = load(&path)?.ok_or_else(|| ReplayError::Read {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such store"),
        })?;

        Ok(Self {
            path,
            model: model.into(),
            source: None,
            store: Mutex::new(store),
        })
    }

    /// Answers from the recordings at `path`, asking `source` for
    /// whatever they lack and recording its answers.
    pub fn record(path: impl Into<PathBuf>, source: Arc<dyn Decider>) -> Result<Self, ReplayError> {
        let path = path.into();

        let store = load(&path)?.unwrap_or_default();

        Ok(Self {
            path,
            model: source.identity().model,
            source: Some(source),
            store: Mutex::new(store),
        })
    }

    /// How many answers are recorded.
    pub fn len(&self) -> usize {
        self.store.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn key(&self, state_hash: &str, question: &Question) -> String {
        SemanticHash::of(&(&self.model, state_hash, question)).to_hex()
    }

    fn persist(&self, store: &StoreFile) -> Result<(), ReplayError> {
        let write = |source| ReplayError::Write {
            path: self.path.clone(),
            source,
        };

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(write)?;
        }

        let mut json = serde_json::to_string_pretty(store)
            .map_err(|error| write(std::io::Error::new(std::io::ErrorKind::InvalidData, error)))?;

        json.push('\n');

        // Written beside the store and renamed over it, so a crash
        // leaves the old recordings rather than half of the new ones.
        let staged = self.path.with_extension("tmp");

        std::fs::write(&staged, json).map_err(write)?;
        std::fs::rename(&staged, &self.path).map_err(write)?;

        Ok(())
    }
}

fn load(path: &Path) -> Result<Option<StoreFile>, ReplayError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,

        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),

        Err(source) => {
            return Err(ReplayError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let store: StoreFile = serde_json::from_str(&text).map_err(|error| ReplayError::Corrupt {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    if store.format != STORE_FORMAT {
        return Err(ReplayError::Format {
            path: path.to_path_buf(),
            found: store.format,
        });
    }

    for (key, entry) in &store.entries {
        let inconsistent = |reason: String| ReplayError::Inconsistent {
            path: path.to_path_buf(),
            key: key.clone(),
            reason,
        };

        if !store.states.contains_key(&entry.state) {
            return Err(inconsistent("its state is not in the store".to_string()));
        }

        entry
            .answer
            .conforms_to(&entry.question)
            .map_err(|defect| inconsistent(defect.to_string()))?;
    }

    Ok(Some(store))
}

#[async_trait]
impl Decider for ReplayDecider {
    fn identity(&self) -> DeciderIdentity {
        DeciderIdentity {
            backend: "replay".to_string(),
            model: self.model.clone(),
            endpoint: None,
            calibrated: self
                .source
                .as_ref()
                .and_then(|source| source.identity().calibrated),
        }
    }

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
        request.validate()?;

        let started = Instant::now();

        let state_hash = SemanticHash::of(&request.state).to_hex();

        let mut answers: BTreeMap<QuestionId, Answer> = BTreeMap::new();
        let mut missing: BTreeMap<QuestionId, Question> = BTreeMap::new();

        {
            let store = self.store.lock();

            for (id, question) in &request.questions {
                match store.entries.get(&self.key(&state_hash, question)) {
                    Some(entry) => {
                        answers.insert(id.clone(), entry.answer.clone());
                    }

                    None => {
                        missing.insert(id.clone(), question.clone());
                    }
                }
            }
        }

        let mut usage = None;
        let mut answered_by = self.model.clone();

        if !missing.is_empty() {
            let Some(source) = &self.source else {
                return Err(DeciderError::ReplayMiss {
                    missing: missing.into_keys().collect(),
                });
            };

            // Only what the store lacks is asked, so a recording grows
            // by exactly the questions that are new.
            let asked = DecisionRequest {
                state: request.state.clone(),
                questions: missing.clone(),
                tags: request.tags.clone(),
            };

            let live = source.decide(&asked).await?;

            // The store vouches for what it holds, whatever the source
            // is: an answer that does not answer its question is never
            // recorded.
            check_answers(&asked, &live.answers)?;

            usage = live.usage;
            answered_by = live.answered_by;

            let mut store = self.store.lock();

            store
                .states
                .entry(state_hash.clone())
                .or_insert_with(|| request.state.clone());

            for (id, question) in missing {
                let answer =
                    live.answers
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| DeciderError::Unanswered {
                            question: id.clone(),
                        })?;

                store.entries.insert(
                    self.key(&state_hash, &question),
                    Entry {
                        model: self.model.clone(),
                        state: state_hash.clone(),
                        question,
                        answer: answer.clone(),
                    },
                );

                answers.insert(id, answer);
            }

            self.persist(&store)?;
        }

        Ok(Decision {
            identity: self.identity(),
            answered_by,
            answers,
            usage,
            latency: started.elapsed(),
            shadow: None,
        })
    }
}
