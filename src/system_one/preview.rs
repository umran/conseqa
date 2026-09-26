//! The preview decider: what *would* be sent, and nothing sent.
//!
//! A hosted decider is data egress, and a run sends it the prompt. So
//! before a run is allowed to, it can be run once with this decider in
//! its place. Every request a builder would have made is written to a
//! file, as the body the wire client would have posted, and no request
//! is made at all: every ask fails with [`DeciderError::PreviewOnly`],
//! every builder abstains, and the run proceeds on its agent backend
//! exactly as it would with the layer off.
//!
//! The file is then the whole answer to "what leaves this machine, and
//! what is asked of it" — reviewable before a credential is ever
//! configured, and the material a question's wording is judged against.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Serialize;

use super::http::WireRequest;
use super::{Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest};

/// One request a builder would have made.
#[derive(Serialize)]
struct Previewed<'a> {
    at_unix_ms: u64,

    /// Who was asking and why. Never part of what is sent.
    tags: &'a std::collections::BTreeMap<String, String>,

    /// Exactly the body the wire client posts.
    body: WireRequest<'a>,
}

pub struct PreviewDecider {
    path: PathBuf,
    model: String,
    writer: Mutex<BufWriter<std::fs::File>>,
}

impl PreviewDecider {
    /// Appends to `path`, creating it and its directory. `model` is the
    /// id the requests would have named.
    pub fn new(path: impl Into<PathBuf>, model: impl Into<String>) -> std::io::Result<Self> {
        let path = path.into();

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            path,
            model: model.into(),
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl Decider for PreviewDecider {
    fn identity(&self) -> DeciderIdentity {
        DeciderIdentity {
            backend: "preview".to_string(),
            model: self.model.clone(),
            endpoint: None,
            calibrated: None,
        }
    }

    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DeciderError> {
        // A request the wire client would have refused is not one it
        // would have sent.
        request.validate()?;

        let previewed = Previewed {
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or_default(),
            tags: &request.tags,
            body: WireRequest {
                state: &request.state,
                model: &self.model,
                questions: &request.questions,
            },
        };

        let written = serde_json::to_string(&previewed)
            .map_err(std::io::Error::other)
            .and_then(|line| {
                let mut writer = self
                    .writer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);

                writeln!(writer, "{line}")?;

                writer.flush()
            });

        if let Err(error) = written {
            tracing::warn!(path = %self.path.display(), %error, "a previewed request was not written");
        }

        Err(DeciderError::PreviewOnly)
    }
}
