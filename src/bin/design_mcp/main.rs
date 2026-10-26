//! conseqa-design-mcp: the standalone design daemon (§6.2, §88 of
//! the design spec).
//!
//! `serve` exposes the shared MCP endpoint for interactive Claude
//! Code / Codex sessions, IDE integration, and external orchestrators,
//! plus a minimal localhost admin surface for creating tasks. Project
//! lifecycle commands live in the project-local `conseqa` CLI.
//!
//! A project is the `.conseqa/` directory of the repository it
//! describes (see `conseqa::design::store`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Json;
use clap::{Parser, Subcommand, ValueEnum};
use parking_lot::{Mutex, RwLock};
use serde::Deserialize;

use conseqa::design::mcp::{self, DesignLauncher};
use conseqa::design::{
    AnalysisState, DesignEngine, CreateTask, PromptEvidence, RunId, RunMetadata,
    TaskBudget, TaskId, TaskKind, WorkspaceState, WriteScope,
};
use conseqa::harness::backend::InvocationBudget;
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::HandOffBackend;
use conseqa::harness::{
    AgentBackend, RunReport, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};
use conseqa::spec::Revision;

#[derive(Parser)]
#[command(
    name = "conseqa-design-mcp",
    about = "Standalone Conseqa design daemon: shared architecture state over MCP"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the MCP endpoint and the localhost admin surface.
    Serve {
        /// The project: its directory or any directory inside it. Created
        /// there when none exists.
        #[arg(long, default_value = ".")]
        project: PathBuf,

        /// Bind address for MCP (`/mcp`) and admin (`/admin/...`).
        #[arg(long, default_value = "127.0.0.1:43127")]
        bind: SocketAddr,

        /// Adopt an existing Conseqa model when creating the project.
        #[arg(long)]
        model: Option<PathBuf>,

        /// The natural-language application prompt when creating the
        /// project.
        #[arg(long)]
        prompt: Option<String>,

        /// On startup, create an interactive authoring session and
        /// print a ready-to-paste `.mcp.json` for the Claude Code UI.
        /// The session can commit repeatedly under one token.
        #[arg(long)]
        session: bool,

        /// Backend for the `request_design` concurrent workflow.
        #[arg(long, value_enum, default_value_t = Backend::Claude)]
        backend: Backend,

        /// Path to the backend executable (non-default install).
        #[arg(long)]
        backend_program: Option<String>,

        /// Maximum worker agents reasoning concurrently in a fanout.
        #[arg(long, default_value_t = 4)]
        max_agents: usize,

        /// Where a triggered workflow writes its finalized artifacts;
        /// the project's uncommitted `.conseqa/local/design` when omitted.
        #[arg(long)]
        design_out: Option<PathBuf>,

        #[command(flatten)]
        system_one: SystemOne,
    },

    /// Run one shared server hosting many projects, for global use from
    /// the Claude Code UI. Projects are opened and created by path
    /// through the MCP project tools.
    Global {
        /// Bind address for MCP (`/mcp`) and admin (`/admin/...`).
        #[arg(long, default_value = "127.0.0.1:43127")]
        bind: SocketAddr,

        /// Stable API key clients authenticate with. Defaults to
        /// `CONSEQA_API_KEY`, else a key persisted at
        /// `~/.conseqa/api-key` (generated on first run).
        #[arg(long)]
        api_key: Option<String>,

        /// Backend for the `request_design` concurrent workflow.
        #[arg(long, value_enum, default_value_t = Backend::Claude)]
        backend: Backend,

        /// Path to the backend executable (non-default install).
        #[arg(long)]
        backend_program: Option<String>,

        /// Maximum worker agents reasoning concurrently in a fanout.
        #[arg(long, default_value_t = 4)]
        max_agents: usize,

        #[command(flatten)]
        system_one: SystemOne,
    },

    /// Serve one shared multi-project instance over stdio, for the
    /// Claude Desktop / Code UI. The app spawns this command and speaks
    /// MCP over stdin/stdout — no URL, no TLS, no certificate. The
    /// project the app starts it in is the default one; others are
    /// opened and created by path through the MCP project tools.
    Stdio {
        /// Backend for the `request_design` concurrent workflow.
        #[arg(long, value_enum, default_value_t = Backend::Claude)]
        backend: Backend,

        /// Path to the backend executable (non-default install).
        #[arg(long)]
        backend_program: Option<String>,

        /// Maximum worker agents reasoning concurrently in a fanout.
        #[arg(long, default_value_t = 4)]
        max_agents: usize,

        #[command(flatten)]
        system_one: SystemOne,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // Logs go to stderr: the `stdio` command uses stdout as the MCP
    // channel, and stderr is harmless for every other command too.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse_from(with_stdio_args_file(std::env::args().collect()));

    let outcome = match cli.command {
        Command::Serve {
            project,
            bind,
            model,
            prompt,
            session,
            backend,
            backend_program,
            max_agents,
            design_out,
            system_one,
        } => {
            serve(ServeOptions {
                project,
                bind,
                model,
                prompt,
                session,
                backend,
                backend_program,
                max_agents,
                design_out,
                system_one,
            })
            .await
        }
        Command::Global {
            bind,
            api_key,
            backend,
            backend_program,
            max_agents,
            system_one,
        } => {
            serve_global(GlobalOptions {
                bind,
                api_key,
                backend,
                backend_program,
                max_agents,
                system_one,
            })
            .await
        }
        Command::Stdio {
            backend,
            backend_program,
            max_agents,
            system_one,
        } => {
            serve_stdio_cmd(StdioOptions {
                backend,
                backend_program,
                max_agents,
                system_one,
            })
            .await
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");

            ExitCode::FAILURE
        }
    }
}

/// Where `stdio` reads extra flags from: one flag or value per line,
/// `#` comments allowed. A desktop client owns its own config file and
/// may rewrite it at any time, so settings that must survive that live
/// here instead, in a file only a person writes. `CONSEQA_STDIO_ARGS`
/// names another file (`/dev/null` for none).
fn stdio_args_file() -> PathBuf {
    if let Some(path) = std::env::var_os("CONSEQA_STDIO_ARGS") {
        return PathBuf::from(path);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    PathBuf::from(home).join(".conseqa/stdio.args")
}

/// Appends the flags in [`stdio_args_file`] to a `stdio` invocation.
/// Flags given on the command line come first; clap rejects a flag
/// given twice, so a conflict is loud rather than silently resolved.
fn with_stdio_args_file(mut argv: Vec<String>) -> Vec<String> {
    if argv.get(1).map(String::as_str) != Some("stdio") {
        return argv;
    }

    let path = stdio_args_file();

    let Ok(text) = std::fs::read_to_string(&path) else {
        return argv;
    };

    let extra: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();

    if !extra.is_empty() {
        eprintln!("note: stdio flags from {}: {}", path.display(), extra.join(" "));
        argv.extend(extra);
    }

    argv
}

/// The `claude` executable when none was named. A desktop app starts
/// its servers with a minimal PATH that rarely includes the user's
/// install, so the usual install locations are tried after PATH.
fn default_claude_program() -> Option<String> {
    let on_path = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join("claude").is_file())
    });

    if on_path {
        return None;
    }

    let home = std::env::var("HOME").unwrap_or_default();

    [
        format!("{home}/.local/bin/claude"),
        format!("{home}/.claude/local/claude"),
        "/opt/homebrew/bin/claude".to_string(),
        "/usr/local/bin/claude".to_string(),
    ]
    .into_iter()
    .find(|candidate| std::path::Path::new(candidate).is_file())
}

/// A new project's workspace: `model` adopted when given, else empty.
/// It is named `name`, or after the directory it is created in.
fn initial_workspace(
    dir: &std::path::Path,
    name: Option<String>,
    model: Option<PathBuf>,
    prompt: Option<String>,
) -> Result<WorkspaceState, String> {
    let name = name.unwrap_or_else(|| {
        dir.canonicalize()
            .ok()
            .and_then(|dir| {
                dir.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "project".to_string())
    });

    let mut run_meta = RunMetadata::new(RunId(name));

    run_meta.prompt = prompt;

    match model {
        None => Ok(WorkspaceState::empty(run_meta)),

        Some(path) => {
            let source = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

            let model = conseqa::parser::yaml::parse(&source)
                .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;

            Ok(WorkspaceState::from_model(&model, run_meta))
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Claude,
    Codex,
}

/// The System One layer of a triggered design run (§26 of the System
/// One orchestration revision). Nothing is enabled unless explicitly
/// configured.
#[cfg(feature = "system-one")]
type SystemOne = conseqa::harness::executors::cli::SystemOneArgs;

/// Without the feature the flags do not exist, and the layer is off.
#[cfg(not(feature = "system-one"))]
#[derive(clap::Args, Debug, Clone)]
struct SystemOne {}

#[cfg(feature = "system-one")]
type DesignExecutor = Option<conseqa::harness::executors::cli::Executor>;

#[cfg(not(feature = "system-one"))]
type DesignExecutor = ();

/// Validates the settings, and says what a design run will send where.
/// Called before anything is opened or served: a daemon must not come
/// up on settings that are wrong.
#[cfg(feature = "system-one")]
fn design_executor(system_one: &SystemOne) -> Result<DesignExecutor, String> {
    system_one.configure()
}

#[cfg(not(feature = "system-one"))]
fn design_executor(_: &SystemOne) -> Result<DesignExecutor, String> {
    Ok(())
}

struct ServeOptions {
    project: PathBuf,
    bind: SocketAddr,
    model: Option<PathBuf>,
    prompt: Option<String>,
    session: bool,
    backend: Backend,
    backend_program: Option<String>,
    max_agents: usize,
    design_out: Option<PathBuf>,
    system_one: SystemOne,
}

async fn serve(options: ServeOptions) -> Result<(), String> {
    let executor = design_executor(&options.system_one)?;

    let initial = initial_workspace(&options.project, None, options.model, options.prompt)?;

    let engine = DesignEngine::open(&options.project, initial)
        .map_err(|error| format!("cannot open {}: {error}", options.project.display()))?;

    // The workers connect back to this daemon's own MCP endpoint. The
    // bound URL is known only after binding, so the launcher reads it
    // from a shared cell filled in below.
    let mcp_url = Arc::new(RwLock::new(String::new()));

    let backend = build_backend(options.backend, options.backend_program);

    let launcher: Arc<dyn DesignLauncher> = Arc::new(DaemonDesignLauncher {
        backend,
        executor,
        mcp_url: Arc::clone(&mcp_url),
        design_out: options.design_out,
        max_agents: options.max_agents.max(1),
        state: Arc::new(DesignState::default()),
    });

    let admin = axum::Router::new()
        .route("/admin/status", get(admin_status))
        .route("/admin/tasks", post(admin_create_task))
        .route("/admin/session", post(admin_create_session))
        .route("/admin/design", post(admin_create_design))
        .route("/admin/tasks/{task}/cancel", post(admin_cancel_task))
        .route("/admin/analysis/{revision}", get(admin_analysis))
        .with_state(AdminState {
            engine: engine.clone(),
            launcher: Arc::clone(&launcher),
        });

    let router = mcp::router_with_launcher(engine.clone(), Arc::clone(&launcher)).merge(admin);

    let server = mcp::serve_router(router, options.bind)
        .await
        .map_err(|error| format!("cannot bind {}: {error}", options.bind))?;

    let url = format!("http://{}/mcp", server.local_addr);
    *mcp_url.write() = url.clone();

    println!(
        "conseqa-design-mcp serving\n  project: {}\n  mcp:     {url}\n  admin:   http://{}/admin/status\n  head:    revision {}",
        engine.project_dir().unwrap_or_default().display(),
        server.local_addr,
        engine.head_revision().0,
    );

    if options.session {
        let handle = engine
            .create_session(interactive_scope(), "interactive authoring session")
            .map_err(|error| format!("cannot create session: {error}"))?;

        print_session_config(&url, &handle.token.0);
    }

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("cannot wait for ctrl-c: {error}"))?;

    println!("shutting down");

    server.shutdown().await;

    Ok(())
}

struct GlobalOptions {
    bind: SocketAddr,
    api_key: Option<String>,
    backend: Backend,
    backend_program: Option<String>,
    max_agents: usize,
    system_one: SystemOne,
}

/// Loads the stable API key: an explicit value, else `CONSEQA_API_KEY`,
/// else a key persisted at `~/.conseqa/api-key` (generated on first
/// run). Persisting it is what makes the UI config set-once. The key
/// configures this machine's server, not any project, so it stays out
/// of every project.
fn load_or_create_api_key(explicit: Option<String>) -> Result<String, String> {
    if let Some(key) = explicit {
        return Ok(key);
    }

    if let Ok(key) = std::env::var("CONSEQA_API_KEY")
        && !key.is_empty()
    {
        return Ok(key);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let path = PathBuf::from(home).join(".conseqa/api-key");

    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();

        if !existing.is_empty() {
            return Ok(existing);
        }
    }

    let key = conseqa::design::TaskToken::generate().0;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }

    std::fs::write(&path, format!("{key}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;

    Ok(key)
}

async fn serve_global(options: GlobalOptions) -> Result<(), String> {
    let executor = design_executor(&options.system_one)?;

    let api_key = load_or_create_api_key(options.api_key)?;

    let manager = Arc::new(conseqa::design::WorkspaceManager::new());

    let mcp_url = Arc::new(RwLock::new(String::new()));

    let backend = build_backend(options.backend, options.backend_program);

    let launcher: Arc<dyn DesignLauncher> = Arc::new(DaemonDesignLauncher {
        backend,
        executor,
        mcp_url: Arc::clone(&mcp_url),
        design_out: None,
        max_agents: options.max_agents.max(1),
        state: Arc::new(DesignState::default()),
    });

    let manager_for_admin = Arc::clone(&manager);

    let admin = axum::Router::new()
        .route(
            "/admin/projects",
            get(move || {
                let manager = Arc::clone(&manager_for_admin);

                async move {
                    let projects: Vec<serde_json::Value> = manager
                        .list()
                        .into_iter()
                        .map(|project| {
                            serde_json::json!({
                                "name": project.name,
                                "path": project.path,
                                "revision": project.revision.0,
                            })
                        })
                        .collect();

                    Json(serde_json::json!({ "projects": projects }))
                }
            }),
        )
        .with_state(());

    let router = mcp::router_multi(Arc::clone(&manager), api_key.clone(), Some(launcher)).merge(admin);

    let server = mcp::serve_router(router, options.bind)
        .await
        .map_err(|error| format!("cannot bind {}: {error}", options.bind))?;

    let url = format!("http://{}/mcp", server.local_addr);
    *mcp_url.write() = url.clone();

    print_global_config(&url, &api_key);

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("cannot wait for ctrl-c: {error}"))?;

    println!("shutting down");

    server.shutdown().await;

    Ok(())
}

/// Prints the set-once global MCP config for the Claude Code UI.
fn print_global_config(mcp_url: &str, api_key: &str) {
    let config = serde_json::json!({
        "mcpServers": {
            "conseqa": {
                "type": "http",
                "url": mcp_url,
                "headers": { "Authorization": format!("Bearer {api_key}") },
            }
        }
    });

    let pretty = serde_json::to_string_pretty(&config).unwrap_or_default();

    println!(
        "conseqa-design-mcp global server\n  mcp: {mcp_url}\n\n\
         Register this ONCE in Claude Code (user scope, so it is global):\n\n\
         Option A — the GUI: Settings → Connectors → Add → HTTP, URL `{mcp_url}`,\n\
         header `Authorization: Bearer {api_key}`.\n\n\
         Option B — the config file `~/.claude.json` (top-level `mcpServers`):\n\n{pretty}\n\n\
         Then, in any repository, ask Claude to `open_project` or `create_project` with\n\
         the repository's path — each project lives in its repository's `.conseqa/`\n\
         directory, committed with the code. Keep this server running; the API key is\n\
         stable across restarts.\n",
    );
}

struct StdioOptions {
    backend: Backend,
    backend_program: Option<String>,
    max_agents: usize,
    system_one: SystemOne,
}

async fn serve_stdio_cmd(options: StdioOptions) -> Result<(), String> {
    let executor = design_executor(&options.system_one)?;

    let manager = Arc::new(conseqa::design::WorkspaceManager::new());

    // The app starts the server in the user's working directory; the
    // project there is the one meant when a client names none.
    let home = std::env::current_dir().ok();

    // Worker agents from request_design are `claude -p` subprocesses,
    // which accept an http MCP URL. They connect back to a private
    // loopback endpoint sharing this manager; the UI itself speaks over
    // stdio and never touches http.
    let mcp_url = Arc::new(RwLock::new(String::new()));
    let backend = build_backend(options.backend, options.backend_program);

    let launcher: Arc<dyn DesignLauncher> = Arc::new(DaemonDesignLauncher {
        backend,
        executor,
        mcp_url: Arc::clone(&mcp_url),
        design_out: None,
        max_agents: options.max_agents.max(1),
        state: Arc::new(DesignState::default()),
    });

    let worker_key = conseqa::design::TaskToken::generate().0;

    let worker_server = mcp::serve_router(
        mcp::router_multi(Arc::clone(&manager), worker_key, Some(Arc::clone(&launcher))),
        "127.0.0.1:0".parse().expect("valid bind address"),
    )
    .await
    .map_err(|error| format!("cannot start the worker endpoint: {error}"))?;

    *mcp_url.write() = format!("http://{}/mcp", worker_server.local_addr);

    tracing::info!(
        home = %home.as_deref().unwrap_or(std::path::Path::new("?")).display(),
        worker_endpoint = %worker_server.local_addr,
        "conseqa stdio server ready"
    );

    // Blocks until the UI client disconnects. Keep the worker endpoint
    // alive for the duration.
    let result = mcp::serve_stdio(manager, Some(launcher), home)
        .await
        .map_err(|error| format!("stdio server error: {error}"));

    worker_server.shutdown().await;

    result
}

fn build_backend(backend: Backend, program: Option<String>) -> Arc<dyn AgentBackend> {
    match backend {
        Backend::Claude => {
            let mut claude = ClaudeCliBackend::new();

            if let Some(program) = program.or_else(default_claude_program) {
                claude = claude.with_program(program);
            }

            Arc::new(claude)
        }

        Backend::Codex => {
            let mut codex = CodexCliBackend::new();

            if let Some(program) = program {
                codex = codex.with_program(program);
            }

            Arc::new(codex)
        }
    }
}

/// Tracks whether a triggered workflow is running and the last report.
#[derive(Default)]
struct DesignState {
    running: AtomicBool,
    started_at_revision: Mutex<Option<u64>>,
    last_report: Mutex<Option<RunReport>>,

    /// Why the last run produced no report, when it errored out of the
    /// workflow instead of finishing. Without this a failed run is
    /// invisible at the MCP surface: `spec_status` would show the run
    /// gone with the previous report still standing.
    last_error: Mutex<Option<String>>,
}

/// The daemon's implementation of the MCP `request_design` trigger:
/// runs the harness workflow against the caller's project engine, with
/// workers connecting back to this daemon's MCP endpoint.
struct DaemonDesignLauncher {
    backend: Arc<dyn AgentBackend>,

    /// The in-process builders put in front of `backend`, per run.
    /// Without the feature there are none, and nothing reads this.
    #[cfg_attr(not(feature = "system-one"), allow(dead_code))]
    executor: DesignExecutor,
    mcp_url: Arc<RwLock<String>>,

    /// Where runs write their finalized artifacts, overriding each
    /// project's own `.conseqa/local/design`.
    design_out: Option<PathBuf>,
    max_agents: usize,
    state: Arc<DesignState>,
}

impl DaemonDesignLauncher {
    /// Where a run on `engine`'s project writes its finalized model,
    /// report, and manifest: derived artifacts, so the project's
    /// uncommitted local directory.
    fn design_dir(&self, engine: &DesignEngine) -> PathBuf {
        self.design_out
            .clone()
            .or_else(|| engine.local_dir().map(|local| local.join("design")))
            .unwrap_or_else(|| std::env::temp_dir().join("conseqa-design-mcp"))
    }

    /// The worker agents' scratch directories, which hold only their
    /// MCP configuration. Outside every repository, so no worker picks
    /// up a project's own agent settings.
    fn work_dir() -> PathBuf {
        std::env::temp_dir().join("conseqa-work")
    }

    /// The backend of one design run: the agent backend, behind the
    /// System One builders when they are configured. A builder acts on
    /// one engine, and each run has its own project's.
    fn backend_for(&self, engine: &DesignEngine) -> Arc<dyn AgentBackend> {
        self.builders_over(engine, Arc::clone(&self.backend))
    }

    /// `fallback`, behind the System One builders when they are
    /// configured.
    fn builders_over(
        &self,
        engine: &DesignEngine,
        fallback: Arc<dyn AgentBackend>,
    ) -> Arc<dyn AgentBackend> {
        #[cfg(feature = "system-one")]
        if let Some(executor) = &self.executor {
            return executor.wrap(engine, fallback);
        }

        #[cfg(not(feature = "system-one"))]
        let _ = engine;

        fallback
    }
}

impl DesignLauncher for DaemonDesignLauncher {
    fn launch(
        &self,
        engine: DesignEngine,
        objective: Option<String>,
    ) -> Result<serde_json::Value, String> {
        // One workflow at a time across the server: overlapping runs
        // would just contend for the same operations.
        if self
            .state
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("a design workflow is already running on this server".to_string());
        }

        let mcp_url = self.mcp_url.read().clone();

        if mcp_url.is_empty() {
            self.state.running.store(false, Ordering::SeqCst);

            return Err("the MCP endpoint is not ready yet".to_string());
        }

        let supervisor = Supervisor::new(
            engine.clone(),
            self.backend_for(&engine),
            mcp_url,
            None,
            Self::work_dir(),
        );

        let scheduler = Scheduler::new(
            engine.clone(),
            supervisor,
            SchedulerPolicy {
                max_concurrent_agents: self.max_agents,
                invocation_budget: InvocationBudget {
                    max_wall_time_secs: Some(600),
                    max_turns: Some(60),
                },
                ..Default::default()
            },
        );

        let workflow = Workflow::new(
            scheduler,
            WorkflowConfig {
                out_dir: self.design_dir(&engine),
                analysis_timeout: Duration::from_secs(180),
                max_iterations: 8,
                objective,
            },
        );

        let started_revision = engine.head_revision().0;
        let state = Arc::clone(&self.state);

        *state.started_at_revision.lock() = Some(started_revision);
        *state.last_error.lock() = None;

        tokio::spawn(async move {
            tracing::info!("concurrent design workflow started");

            match workflow.run().await {
                Ok(report) => {
                    tracing::info!(?report.status, "concurrent design workflow finished");
                    *state.last_report.lock() = Some(report);
                }
                Err(error) => {
                    tracing::error!("concurrent design workflow failed: {error}");
                    *state.last_error.lock() = Some(error.to_string());
                }
            }

            state.running.store(false, Ordering::SeqCst);
        });

        Ok(serde_json::json!({
            "launched": true,
            "backend": self.backend.name(),
            "max_agents": self.max_agents,
            "started_at_revision": started_revision,
            "note": "One worker per unfinished operation is now running in the background \
                     against this shared model. Do not submit patches while the run is \
                     active — call await_design, which blocks until the run finishes (call \
                     it again if it returns finished false) and returns the run's report. \
                     Then call open_project again to refresh your session to the new head \
                     before reading or patching.",
        }))
    }

    fn build(
        &self,
        engine: DesignEngine,
        objective: Option<String>,
    ) -> Result<futures::future::BoxFuture<'static, serde_json::Value>, String> {
        if self
            .state
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("a design workflow is already running on this server".to_string());
        }

        // No session is ever started, so no worker needs the endpoint.
        let hand_off = HandOffBackend::new(engine.clone());

        let supervisor = Supervisor::new(
            engine.clone(),
            self.builders_over(&engine, Arc::new(hand_off.clone())),
            "http://127.0.0.1:0/mcp",
            None,
            Self::work_dir(),
        );

        // One task at a time: the builders take milliseconds to
        // seconds, and running them in order keeps every one against
        // the head its predecessor committed.
        let scheduler = Scheduler::new(
            engine.clone(),
            supervisor,
            SchedulerPolicy {
                max_concurrent_agents: 1,
                // A handed-off task commits nothing and would be
                // retried; one retry still covers a builder that lost an
                // OCC race.
                max_attempts: 2,
                ..Default::default()
            },
        );

        let workflow = Workflow::new(
            scheduler,
            WorkflowConfig {
                out_dir: self.design_dir(&engine),
                analysis_timeout: Duration::from_secs(180),
                max_iterations: 8,
                objective,
            },
        )
        .with_halt(hand_off.halt());

        let started_revision = engine.head_revision().0;
        let state = Arc::clone(&self.state);

        *state.started_at_revision.lock() = Some(started_revision);
        *state.last_error.lock() = None;

        // Spawned, so a client that gives up on the call does not drop
        // the run halfway and leave the server marked busy.
        let run = tokio::spawn(async move {
            let started = std::time::Instant::now();
            let result = workflow.run().await;

            let body = match &result {
                Ok(report) => {
                    *state.last_report.lock() = Some(report.clone());
                    serde_json::json!({ "built": true, "report": report })
                }
                Err(error) => {
                    *state.last_error.lock() = Some(error.to_string());
                    serde_json::json!({ "built": false, "error": error.to_string() })
                }
            };

            state.running.store(false, Ordering::SeqCst);

            (body, started.elapsed())
        });

        Ok(Box::pin(async move {
            let (mut body, elapsed) = match run.await {
                Ok(done) => done,
                Err(error) => (
                    serde_json::json!({ "built": false, "error": error.to_string() }),
                    Duration::ZERO,
                ),
            };

            let handed = hand_off.handed();

            if let Some(object) = body.as_object_mut() {
                object.insert(
                    "started_at_revision".to_string(),
                    serde_json::json!(started_revision),
                );
                object.insert(
                    "elapsed_secs".to_string(),
                    serde_json::json!(elapsed.as_secs_f64().round()),
                );
                object.insert(
                    "guidance".to_string(),
                    serde_json::json!(if handed.is_empty() {
                        "Nothing was handed back. If the report's status is not success, \
                         spec_status names what is unproven or invalid: fix it with \
                         submit_patch and call build_design again."
                    } else {
                        "Each hand-off is work code could not settle, with what the builder \
                         established. Resolve them with submit_patch — fix a sketch that does \
                         not compile, write a program the vocabulary cannot express, declare \
                         or decline an unsure requirement, repair an unproven transaction — \
                         then call build_design again. The build stopped at the first phase \
                         that handed work back, so later phases have not run yet."
                    }),
                );
                object.insert("hand_offs".to_string(), serde_json::json!(handed));
            }

            body
        }))
    }

    fn status(&self) -> Option<serde_json::Value> {
        let last_run = self
            .state
            .last_report
            .lock()
            .as_ref()
            .and_then(|report| serde_json::to_value(report).ok());

        let mut body = serde_json::json!({
            "running": self.state.running.load(Ordering::SeqCst),
            "started_at_revision": *self.state.started_at_revision.lock(),
            "last_run": last_run,
        });

        // A run that errored out produced no report; say so rather than
        // letting it vanish behind the previous run's report.
        if let Some(error) = self.state.last_error.lock().clone() {
            body["last_run_failed"] = serde_json::json!({
                "error": error,
                "guidance": "The last design run aborted before finalizing. The model keeps \
                             every commit the workers made; check spec_status's analysis \
                             block for where it stands, and call request_design again to \
                             resume from the current head.",
            });
        }

        Some(body)
    }
}

/// The full authoring scope for an interactive UI session: it may
/// create and edit anything, including operations it creates during the
/// session. The human is the coordinator; read-before-reference, draft
/// validation, and OCC still apply.
fn interactive_scope() -> conseqa::design::WriteScope {
    conseqa::design::WriteScope::of([conseqa::design::WriteGrant::All])
}

/// Prints a ready-to-paste `.mcp.json` block for the Claude Code UI,
/// with the session token filled in.
fn print_session_config(mcp_url: &str, token: &str) {
    let config = serde_json::json!({
        "mcpServers": {
            "conseqa": {
                "type": "http",
                "url": mcp_url,
                "headers": { "Authorization": format!("Bearer {token}") },
            }
        }
    });

    let pretty = serde_json::to_string_pretty(&config).unwrap_or_default();

    println!(
        "\ninteractive session ready. Register it with Claude Code:\n\n\
         1. Write this to `.mcp.json` at your project root:\n\n{pretty}\n\n\
         2. Restart Claude Code (or run `/mcp` and connect `conseqa`), approve the\n\
         \x20  server, and ask it to use the conseqa tools — start with `task_context`\n\
         \x20  and `dsl_reference`.\n\n\
         The session commits repeatedly under this one token. Keep this daemon\n\
         running; the token is valid only for this run.\n"
    );
}

/// Shared state for the admin surface: the engine and the design
/// launcher.
#[derive(Clone)]
struct AdminState {
    engine: DesignEngine,
    launcher: Arc<dyn DesignLauncher>,
}

#[derive(Deserialize)]
struct AdminCreateTask {
    kind: TaskKind,
    objective: String,
    write_scope: WriteScope,

    #[serde(default)]
    prompt_evidence: Vec<PromptEvidence>,
}

async fn admin_create_task(
    State(state): State<AdminState>,
    Json(request): Json<AdminCreateTask>,
) -> Json<serde_json::Value> {
    match state.engine.create_task(CreateTask {
        kind: request.kind,
        objective: request.objective,
        write_scope: request.write_scope,
        prompt_evidence: request.prompt_evidence,
        budget: TaskBudget::default(),
    }) {
        Ok(handle) => Json(serde_json::json!({
            "task": handle.id.to_string(),
            "token": handle.token.0,
            "snapshot_revision": handle.snapshot_revision.0,
        })),

        Err(error) => Json(serde_json::json!({ "error": error.to_string() })),
    }
}

/// Creates an interactive authoring session over the current head and
/// returns its token plus a ready-to-paste authorization header.
async fn admin_create_session(State(state): State<AdminState>) -> Json<serde_json::Value> {
    match state
        .engine
        .create_session(interactive_scope(), "interactive authoring session")
    {
        Ok(handle) => Json(serde_json::json!({
            "task": handle.id.to_string(),
            "token": handle.token.0,
            "snapshot_revision": handle.snapshot_revision.0,
            "authorization_header": format!("Bearer {}", handle.token.0),
        })),

        Err(error) => Json(serde_json::json!({ "error": error.to_string() })),
    }
}

/// Launches the concurrent design workflow from a terminal trigger, the
/// same one `request_design` uses from the UI.
async fn admin_create_design(State(state): State<AdminState>) -> Json<serde_json::Value> {
    match state.launcher.launch(state.engine.clone(), None) {
        Ok(value) => Json(value),
        Err(error) => Json(serde_json::json!({ "launched": false, "error": error })),
    }
}

async fn admin_cancel_task(
    State(state): State<AdminState>,
    Path(task): Path<String>,
) -> Json<serde_json::Value> {
    let Ok(task) = task
        .strip_prefix("task-")
        .unwrap_or(&task)
        .parse::<uuid::Uuid>()
    else {
        return Json(serde_json::json!({ "error": "not a task id" }));
    };

    match state.engine.cancel_task(TaskId(task)) {
        Ok(()) => Json(serde_json::json!({ "cancelled": true })),
        Err(error) => Json(serde_json::json!({ "error": error.to_string() })),
    }
}

async fn admin_status(State(state): State<AdminState>) -> Json<serde_json::Value> {
    let engine = &state.engine;
    let head = engine.head_revision();

    let tasks: Vec<serde_json::Value> = engine
        .list_tasks()
        .into_iter()
        .map(|task| {
            serde_json::json!({
                "task": task.id.to_string(),
                "kind": task.kind.to_string(),
                "state": task.state.to_string(),
                "snapshot_revision": task.snapshot_revision.0,
                "objective": task.objective,
            })
        })
        .collect();

    Json(serde_json::json!({
        "head": head.0,
        "analysis": engine.analysis_state(head).label(),
        "tasks": tasks,
    }))
}

async fn admin_analysis(
    State(state): State<AdminState>,
    Path(revision): Path<u64>,
) -> Json<serde_json::Value> {
    let state = state.engine.analysis_state(Revision(revision));

    let value = match &state {
        AnalysisState::Ready(analysis) => serde_json::json!({
            "revision": revision,
            "state": state.label(),
            "all_proven": analysis.verification.all_proven(),
        }),

        other => serde_json::json!({
            "revision": revision,
            "state": other.label(),
        }),
    };

    Json(value)
}
