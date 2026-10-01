//! The command-line surface of the System One layer (§26 of the System
//! One orchestration revision), shared by every entry point that can
//! start a design run: `conseqa-harness design`, and the daemon whose
//! `request_design` runs the same workflow.
//!
//! Nothing here is ever enabled implicitly. A run uses a decider only
//! when one is named, sends state off the machine only to a URL that
//! was given, and attempts in process only the task kinds it was told
//! to.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, ValueEnum};

use crate::confluence::{ConfluenceEngine, TaskKind};
use crate::harness::AgentBackend;
use crate::system_one::{BackendSettings, ConfiguredDecider, DeciderKind, DeciderSettings};

use super::{BUILDABLE, SystemOneBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    /// The System One layer is off.
    None,

    /// A wire-format server: the hosted service or a local one.
    SystemOne,

    /// Recordings only; nothing is asked live.
    Replay,

    /// Nothing is asked: every request a builder would have made is
    /// written to --decider-log, exactly as it would have been sent,
    /// and the run proceeds on its agent backend. For reading what
    /// would leave the machine before anything does.
    Preview,
}

/// One wire-format backend.
#[derive(Args, Debug, Clone)]
pub struct BackendArgs {
    /// The server's base URL. There is no default: a URL that is not
    /// loopback sends state off this machine. The hosted service is
    /// https://api.typesafe.ai.
    #[arg(long)]
    pub decider_url: Option<String>,

    /// The pinned model id, such as jev-1.13.0. An alias is refused
    /// unless --decider-allow-alias is given.
    #[arg(long)]
    pub decider_model: Option<String>,

    /// The environment variable holding the bearer credential. The
    /// hosted service's is conventionally TYPESAFE_API_KEY.
    #[arg(long)]
    pub decider_key_env: Option<String>,

    /// A file holding only the bearer credential, for clients whose
    /// config cannot carry an environment. Ignored when
    /// --decider-key-env is given.
    #[arg(long)]
    pub decider_key_file: Option<PathBuf>,

    /// Accept a model alias such as jev-latest. For probing only:
    /// thresholds are tuned per versioned id.
    #[arg(long)]
    pub decider_allow_alias: bool,
}

impl BackendArgs {
    pub fn settings(&self) -> BackendSettings {
        BackendSettings {
            url: self.decider_url.clone(),
            model: self.decider_model.clone(),
            key_env: self.decider_key_env.clone(),
            key_file: self.decider_key_file.clone(),
            allow_alias: self.decider_allow_alias,
        }
    }
}

/// Everything that decides which decider is used.
#[derive(Args, Debug, Clone)]
pub struct DeciderArgs {
    /// Where answers come from.
    #[arg(long, value_enum, default_value_t = Kind::None)]
    pub decider: Kind,

    #[command(flatten)]
    pub primary: BackendArgs,

    /// A second server asked the same questions. Its answers are logged
    /// and never acted on.
    #[arg(long, requires = "decider_shadow_model")]
    pub decider_shadow_url: Option<String>,

    #[arg(long, requires = "decider_shadow_url")]
    pub decider_shadow_model: Option<String>,

    #[arg(long, requires = "decider_shadow_url")]
    pub decider_shadow_key_env: Option<String>,

    /// The replay store: the only source of answers with
    /// `--decider replay`, and what `--decider-record` fills.
    #[arg(long)]
    pub decider_replay: Option<PathBuf>,

    /// Record the primary's answers into the replay store.
    #[arg(long, requires = "decider_replay")]
    pub decider_record: bool,

    /// Append every ask, with its full distribution, to this file.
    #[arg(long)]
    pub decider_log: Option<PathBuf>,
}

impl DeciderArgs {
    pub fn settings(&self) -> DeciderSettings {
        DeciderSettings {
            kind: match self.decider {
                Kind::None => DeciderKind::None,
                Kind::SystemOne => DeciderKind::SystemOne,
                Kind::Replay => DeciderKind::Replay,
                Kind::Preview => DeciderKind::Preview,
            },
            primary: self.primary.settings(),
            shadow: self.decider_shadow_url.as_ref().map(|url| BackendSettings {
                url: Some(url.clone()),
                model: self.decider_shadow_model.clone(),
                key_env: self.decider_shadow_key_env.clone(),
                key_file: None,
                allow_alias: false,
            }),
            replay: self.decider_replay.clone(),
            record: self.decider_record,
            log: self.decider_log.clone(),
        }
    }
}

/// Everything a design run needs to know about the System One layer.
#[derive(Args, Debug, Clone)]
pub struct SystemOneArgs {
    #[command(flatten)]
    pub decider: DeciderArgs,

    /// Task kinds a System One builder attempts in process before the
    /// agent backend, comma separated. Needs a decider. A builder that
    /// is unsure hands the task to the agent backend.
    #[arg(long, value_delimiter = ',', value_parser = buildable_kind)]
    pub system_one_kinds: Vec<TaskKind>,
}

impl SystemOneArgs {
    /// Validates the settings and says what will leave the machine.
    /// See [`configure`].
    pub fn configure(&self) -> Result<Option<Executor>, String> {
        configure(&self.decider, &self.system_one_kinds)
    }
}

/// Says, before anything is sent, what is going where.
pub fn announce_egress(what: &str, endpoints: &[String]) {
    for endpoint in endpoints {
        eprintln!("note: {what} is sent off this machine, to {endpoint}");
    }
}

/// A task kind named on the command line. Only a kind with a builder is
/// accepted: naming another would silently do nothing.
pub fn buildable_kind(text: &str) -> Result<TaskKind, String> {
    let wanted = text.trim().replace('-', "_");

    BUILDABLE
        .into_iter()
        .find(|kind| kind.to_string() == wanted)
        .ok_or_else(|| {
            format!(
                "`{text}` has no System One builder; the kinds that do: {}",
                BUILDABLE.map(|kind| kind.to_string()).join(", ")
            )
        })
}

/// A validated System One configuration for a run, awaiting the engine
/// it will act on.
pub struct Executor {
    configured: ConfiguredDecider,
    kinds: Vec<TaskKind>,
}

/// Validates the settings before anything is opened or served, and
/// says what will leave the machine. `None` means the layer is off: the
/// executor is never enabled implicitly, and needs both a decider and
/// the kinds it may attempt.
pub fn configure(args: &DeciderArgs, kinds: &[TaskKind]) -> Result<Option<Executor>, String> {
    let Some(configured) = args.settings().build().map_err(|error| error.to_string())? else {
        if kinds.is_empty() {
            return Ok(None);
        }

        return Err(
            "--system-one-kinds needs a decider: pass --decider system-one or --decider replay"
                .to_string(),
        );
    };

    if kinds.is_empty() {
        eprintln!("note: a decider is configured but --system-one-kinds is empty: nothing asks it");

        return Ok(None);
    }

    announce_egress(
        "the run's prompt, and a summary of each operation a builder decides,",
        &configured.egress,
    );

    Ok(Some(Executor {
        configured,
        kinds: kinds.to_vec(),
    }))
}

impl Executor {
    /// Puts the in-process builders in front of `fallback`, acting on
    /// `engine`. Reusable: a daemon wraps once per design run, each
    /// against its own project's engine.
    pub fn wrap(
        &self,
        engine: &ConfluenceEngine,
        fallback: Arc<dyn AgentBackend>,
    ) -> Arc<dyn AgentBackend> {
        let backend = SystemOneBackend::new(
            engine.clone(),
            Arc::clone(&self.configured.decider),
            self.kinds.iter().copied(),
            fallback,
        );

        eprintln!(
            "system one attempts, before the agent backend: {}",
            backend
                .kinds()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );

        Arc::new(backend)
    }
}
