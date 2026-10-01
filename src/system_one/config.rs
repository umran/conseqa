//! Decider settings (§26 of the System One orchestration revision).
//!
//! Which backend answers is configuration, never code: the hosted
//! service, a local server and a recording are the same [`Decider`] to
//! everything that asks one.
//!
//! No backend is a default. With no decider configured the System One
//! layer is off and the harness behaves exactly as it did without it;
//! and because a base URL that is not loopback sends state off the
//! machine, there is no default URL either — egress is always something
//! a person wrote down (§11.6).

use std::path::PathBuf;
use std::sync::Arc;

use super::{
    Decider, DeciderConfigError, DecisionLog, LoggingDecider, PreviewDecider, ReplayDecider,
    ReplayError, ShadowDecider, SystemOneHttpDecider,
};

/// One wire-format backend: where it is, what it is asked for, and
/// where its credential is read from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackendSettings {
    /// The base URL. The hosted service is
    /// [`HOSTED_BASE_URL`](super::HOSTED_BASE_URL); a local server is a
    /// loopback address.
    pub url: Option<String>,

    /// The pinned model id.
    pub model: Option<String>,

    /// The environment variable holding the bearer credential. Absent
    /// for a server that needs none.
    pub key_env: Option<String>,

    /// A file holding only the bearer credential. Used when `key_env`
    /// is absent.
    pub key_file: Option<PathBuf>,

    /// Accept an alias such as `jev-latest`. For a connectivity probe
    /// only: thresholds are tuned per versioned id (§11.1).
    pub allow_alias: bool,
}

impl BackendSettings {
    /// The wire-format decider these settings describe. `role` names
    /// it in an error: `primary` or `shadow`.
    pub fn build(&self, role: &'static str) -> Result<SystemOneHttpDecider, SettingsError> {
        let url = self.url.as_deref().ok_or(SettingsError::Missing {
            role,
            setting: "url",
        })?;

        let model = self.model.as_deref().ok_or(SettingsError::Missing {
            role,
            setting: "model",
        })?;

        let decider = if self.allow_alias {
            SystemOneHttpDecider::with_alias_allowed(url, model)?
        } else {
            SystemOneHttpDecider::new(url, model)?
        };

        match (&self.key_env, &self.key_file) {
            (Some(var), _) => Ok(decider.with_credential_from_env(var)?),
            (None, Some(path)) => Ok(decider.with_credential_from_file(path)?),
            (None, None) => Ok(decider),
        }
    }
}

/// Where answers come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeciderKind {
    /// The System One layer is off.
    #[default]
    None,

    /// A wire-format server: the hosted service or a local one.
    SystemOne,

    /// Recordings only; nothing is ever asked live.
    Replay,

    /// Nothing is asked at all: what would have been is written down.
    Preview,
}

/// Everything that decides which [`Decider`] a run uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeciderSettings {
    pub kind: DeciderKind,

    pub primary: BackendSettings,

    /// A second backend asked the same questions, whose answers are
    /// logged and never acted on (§11.5).
    pub shadow: Option<BackendSettings>,

    /// The replay store. With [`DeciderKind::Replay`] it is the only
    /// source of answers; with [`DeciderKind::SystemOne`] and `record`
    /// it is filled from the primary.
    pub replay: Option<PathBuf>,

    pub record: bool,

    /// The decision log (§13.2).
    pub log: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("the {role} decider needs a {setting}")]
    Missing {
        role: &'static str,
        setting: &'static str,
    },

    #[error("recording needs a replay store to record into")]
    RecordWithoutStore,

    #[error("a shadow decider needs a primary to shadow")]
    ShadowWithoutPrimary,

    #[error(transparent)]
    Decider(#[from] DeciderConfigError),

    #[error(transparent)]
    Replay(#[from] ReplayError),

    #[error("cannot open the decision log {path}: {source}")]
    Log {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// A configured decider, and whether asking it sends state off the
/// machine.
pub struct ConfiguredDecider {
    pub decider: Arc<dyn Decider>,

    /// The endpoints that are not loopback. Empty when nothing leaves.
    pub egress: Vec<String>,
}

impl DeciderSettings {
    /// Builds the decider these settings describe, or `None` when the
    /// layer is off.
    ///
    /// The layers compose outside in: every ask is logged, recordings
    /// answer what they can, and what remains goes to the primary with
    /// the shadow beside it.
    pub fn build(&self) -> Result<Option<ConfiguredDecider>, SettingsError> {
        let mut egress = Vec::new();

        let mut note_egress = |decider: &SystemOneHttpDecider| {
            if decider.is_egress()
                && let Some(endpoint) = decider.identity().endpoint
            {
                egress.push(endpoint);
            }
        };

        let decider: Arc<dyn Decider> = match self.kind {
            DeciderKind::None => return Ok(None),

            DeciderKind::Replay => {
                if self.shadow.is_some() {
                    return Err(SettingsError::ShadowWithoutPrimary);
                }

                let store = self.replay.clone().ok_or(SettingsError::Missing {
                    role: "replay",
                    setting: "store",
                })?;

                let model = self.primary.model.clone().ok_or(SettingsError::Missing {
                    role: "replay",
                    setting: "model",
                })?;

                Arc::new(ReplayDecider::replay(store, model)?)
            }

            // Writes to the log path, which is then not a decision log:
            // there are no decisions. No URL, no credential, no egress.
            DeciderKind::Preview => {
                let path = self.log.clone().ok_or(SettingsError::Missing {
                    role: "preview",
                    setting: "log",
                })?;

                let model = self
                    .primary
                    .model
                    .clone()
                    .unwrap_or_else(|| "preview".to_string());

                let preview = PreviewDecider::new(&path, model)
                    .map_err(|source| SettingsError::Log { path, source })?;

                return Ok(Some(ConfiguredDecider {
                    decider: Arc::new(preview),
                    egress: Vec::new(),
                }));
            }

            DeciderKind::SystemOne => {
                let primary = self.primary.build("primary")?;

                note_egress(&primary);

                let asked: Arc<dyn Decider> = match &self.shadow {
                    None => Arc::new(primary),

                    Some(shadow) => {
                        let shadow = shadow.build("shadow")?;

                        note_egress(&shadow);

                        Arc::new(ShadowDecider::new(Arc::new(primary), Arc::new(shadow)))
                    }
                };

                match (&self.replay, self.record) {
                    (Some(store), true) => Arc::new(ReplayDecider::record(store.clone(), asked)?),
                    (None, true) => return Err(SettingsError::RecordWithoutStore),
                    (_, false) => asked,
                }
            }
        };

        let decider = match &self.log {
            None => decider,

            Some(path) => {
                let log = DecisionLog::open(path).map_err(|source| SettingsError::Log {
                    path: path.clone(),
                    source,
                })?;

                Arc::new(LoggingDecider::new(decider, Arc::new(log)))
            }
        };

        Ok(Some(ConfiguredDecider { decider, egress }))
    }
}
