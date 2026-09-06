//! conseqa-confluence: the standalone confluence daemon (§6.2, §88 of
//! the confluence spec).
//!
//! `serve` exposes the shared MCP endpoint for interactive Claude
//! Code / Codex sessions, IDE integration, and external orchestrators,
//! plus a minimal localhost admin surface for creating tasks. `status`
//! and `export` operate directly on the database while no daemon holds
//! it.

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

use conseqa::confluence::mcp::{self, DesignLauncher};
use conseqa::confluence::{
    AnalysisState, ConfluenceEngine, CreateTask, Persistence, PromptEvidence, RunId, RunMetadata,
    TaskBudget, TaskId, TaskKind, WorkspaceState, WriteScope,
};
use conseqa::harness::backend::InvocationBudget;
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::{
    AgentBackend, RunReport, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};
use conseqa::spec::Revision;

#[derive(Parser)]
#[command(
    name = "conseqa-confluence",
    about = "Standalone Conseqa confluence daemon: shared architecture state over MCP"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the MCP endpoint and the localhost admin surface.
    Serve {
        /// Authoring database path.
        #[arg(long, default_value = ".conseqa/confluence.redb")]
        database: PathBuf,

        /// Bind address for MCP (`/mcp`) and admin (`/admin/...`).
        #[arg(long, default_value = "127.0.0.1:43127")]
        bind: SocketAddr,

        /// Adopt an existing Conseqa model when the database is empty.
        #[arg(long)]
        model: Option<PathBuf>,

        /// The natural-language application prompt for a fresh run.
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

        /// Where a triggered workflow writes its finalized artifacts.
        #[arg(long, default_value = ".conseqa/design")]
        design_out: PathBuf,
    },

    /// Run one shared server hosting many projects, for global use from
    /// the Claude Code UI. No per-project database is given up front;
    /// projects are created and selected through the MCP project tools.
    Global {
        /// Directory holding one database per project.
        #[arg(long, default_value_t = default_data_dir())]
        data_dir: String,

        /// Bind address for MCP (`/mcp`) and admin (`/admin/...`).
        #[arg(long, default_value = "127.0.0.1:43127")]
        bind: SocketAddr,

        /// Stable API key clients authenticate with. Defaults to
        /// `CONSEQA_API_KEY`, else a key persisted next to the data
        /// directory (generated on first run).
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
    },

    /// Print the persisted head, tasks, and commit count.
    Status {
        #[arg(long, default_value = ".conseqa/confluence.redb")]
        database: PathBuf,
    },

    /// Assemble the head workspace and write the canonical model YAML.
    Export {
        #[arg(long, default_value = ".conseqa/confluence.redb")]
        database: PathBuf,

        /// Where to write the model.
        #[arg(long, default_value = "conseqa.yaml")]
        out: PathBuf,

        /// Also validate, verify, and write the obligation report.
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    let outcome = match cli.command {
        Command::Serve {
            database,
            bind,
            model,
            prompt,
            session,
            backend,
            backend_program,
            max_agents,
            design_out,
        } => {
            serve(ServeOptions {
                database,
                bind,
                model,
                prompt,
                session,
                backend,
                backend_program,
                max_agents,
                design_out,
            })
            .await
        }
        Command::Global {
            data_dir,
            bind,
            api_key,
            backend,
            backend_program,
            max_agents,
        } => {
            serve_global(GlobalOptions {
                data_dir: PathBuf::from(data_dir),
                bind,
                api_key,
                backend,
                backend_program,
                max_agents,
            })
            .await
        }
        Command::Status { database } => status(database),
        Command::Export {
            database,
            out,
            report,
        } => export(database, out, report),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");

            ExitCode::FAILURE
        }
    }
}

fn initial_workspace(
    model: Option<PathBuf>,
    prompt: Option<String>,
) -> Result<WorkspaceState, String> {
    let mut run_meta = RunMetadata::new(RunId(uuid::Uuid::new_v4().to_string()));

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

struct ServeOptions {
    database: PathBuf,
    bind: SocketAddr,
    model: Option<PathBuf>,
    prompt: Option<String>,
    session: bool,
    backend: Backend,
    backend_program: Option<String>,
    max_agents: usize,
    design_out: PathBuf,
}

async fn serve(options: ServeOptions) -> Result<(), String> {
    let initial = initial_workspace(options.model, options.prompt)?;

    let engine = ConfluenceEngine::open(&options.database, initial)
        .map_err(|error| format!("cannot open {}: {error}", options.database.display()))?;

    // The workers connect back to this daemon's own MCP endpoint. The
    // bound URL is known only after binding, so the launcher reads it
    // from a shared cell filled in below.
    let mcp_url = Arc::new(RwLock::new(String::new()));

    let backend = build_backend(options.backend, options.backend_program);

    let launcher: Arc<dyn DesignLauncher> = Arc::new(DaemonDesignLauncher {
        backend,
        mcp_url: Arc::clone(&mcp_url),
        out_dir: options.design_out,
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
        "conseqa-confluence serving\n  mcp:   {url}\n  admin: http://{}/admin/status\n  head:  revision {}",
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
    data_dir: PathBuf,
    bind: SocketAddr,
    api_key: Option<String>,
    backend: Backend,
    backend_program: Option<String>,
    max_agents: usize,
}

/// The default per-user data directory, `~/.conseqa/projects`.
fn default_data_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    format!("{home}/.conseqa/projects")
}

/// Loads the stable API key: an explicit value, else `CONSEQA_API_KEY`,
/// else a key persisted at `<data_dir>/../api-key` (generated on first
/// run). Persisting it is what makes the UI config set-once.
fn load_or_create_api_key(
    explicit: Option<String>,
    data_dir: &std::path::Path,
) -> Result<String, String> {
    if let Some(key) = explicit {
        return Ok(key);
    }

    if let Ok(key) = std::env::var("CONSEQA_API_KEY")
        && !key.is_empty()
    {
        return Ok(key);
    }

    let path = data_dir
        .parent()
        .unwrap_or(data_dir)
        .join("api-key");

    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();

        if !existing.is_empty() {
            return Ok(existing);
        }
    }

    let key = conseqa::confluence::TaskToken::generate().0;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }

    std::fs::write(&path, format!("{key}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;

    Ok(key)
}

async fn serve_global(options: GlobalOptions) -> Result<(), String> {
    std::fs::create_dir_all(&options.data_dir)
        .map_err(|error| format!("cannot create {}: {error}", options.data_dir.display()))?;

    let api_key = load_or_create_api_key(options.api_key, &options.data_dir)?;

    let manager = Arc::new(conseqa::confluence::WorkspaceManager::new(&options.data_dir));

    let mcp_url = Arc::new(RwLock::new(String::new()));

    let backend = build_backend(options.backend, options.backend_program);

    let launcher: Arc<dyn DesignLauncher> = Arc::new(DaemonDesignLauncher {
        backend,
        mcp_url: Arc::clone(&mcp_url),
        out_dir: options.data_dir.join("design"),
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
                                "open": project.open,
                                "revision": project.revision.map(|revision| revision.0),
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

    print_global_config(&url, &api_key, &options.data_dir);

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("cannot wait for ctrl-c: {error}"))?;

    println!("shutting down");

    server.shutdown().await;

    Ok(())
}

/// Prints the set-once global MCP config for the Claude Code UI.
fn print_global_config(mcp_url: &str, api_key: &str, data_dir: &std::path::Path) {
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
        "conseqa-confluence global server\n  mcp:      {mcp_url}\n  projects: {}\n\n\
         Register this ONCE in Claude Code (user scope, so it is global):\n\n\
         Option A — the GUI: Settings → Connectors → Add → HTTP, URL `{mcp_url}`,\n\
         header `Authorization: Bearer {api_key}`.\n\n\
         Option B — the config file `~/.claude.json` (top-level `mcpServers`):\n\n{pretty}\n\n\
         Then, in any project, ask Claude to `list_projects`, `open_project`, or \n\
         `create_project` — each project is its own isolated model. Keep this server\n\
         running; the API key is stable across restarts.\n",
        data_dir.display(),
    );
}

fn build_backend(backend: Backend, program: Option<String>) -> Arc<dyn AgentBackend> {
    match backend {
        Backend::Claude => {
            let mut claude = ClaudeCliBackend::new();

            if let Some(program) = program {
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
    last_report: Mutex<Option<RunReport>>,
}

/// The daemon's implementation of the MCP `request_design` trigger:
/// runs the harness workflow against the caller's project engine, with
/// workers connecting back to this daemon's MCP endpoint.
struct DaemonDesignLauncher {
    backend: Arc<dyn AgentBackend>,
    mcp_url: Arc<RwLock<String>>,
    out_dir: PathBuf,
    max_agents: usize,
    state: Arc<DesignState>,
}

impl DesignLauncher for DaemonDesignLauncher {
    fn launch(
        &self,
        engine: ConfluenceEngine,
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

        let work_dir = self.out_dir.join("work");

        let supervisor = Supervisor::new(
            engine.clone(),
            Arc::clone(&self.backend),
            mcp_url,
            None,
            work_dir,
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
                out_dir: self.out_dir.clone(),
                analysis_timeout: Duration::from_secs(180),
                max_iterations: 8,
            },
        );

        let started_revision = engine.head_revision().0;
        let state = Arc::clone(&self.state);

        tokio::spawn(async move {
            tracing::info!("concurrent design workflow started");

            match workflow.run().await {
                Ok(report) => {
                    tracing::info!(?report.status, "concurrent design workflow finished");
                    *state.last_report.lock() = Some(report);
                }
                Err(error) => {
                    tracing::error!("concurrent design workflow failed: {error}");
                }
            }

            state.running.store(false, Ordering::SeqCst);
        });

        let _ = objective;

        Ok(serde_json::json!({
            "launched": true,
            "backend": self.backend.name(),
            "max_agents": self.max_agents,
            "started_at_revision": started_revision,
            "note": "Worker agents are running in the background against this shared model. \
                     Watch the head advance with task_context and read the growing model \
                     with the read tools; results also land in the design output directory.",
        }))
    }
}

/// The full authoring scope for an interactive UI session: it may
/// create and edit anything, including operations it creates during the
/// session. The human is the coordinator; read-before-reference, draft
/// validation, and OCC still apply.
fn interactive_scope() -> conseqa::confluence::WriteScope {
    conseqa::confluence::WriteScope::of([conseqa::confluence::WriteGrant::All])
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
    engine: ConfluenceEngine,
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

fn open_persistence(database: &std::path::Path) -> Result<Persistence, String> {
    Persistence::open_file(database)
        .map_err(|error| format!("cannot open {}: {error}", database.display()))
}

fn status(database: PathBuf) -> Result<(), String> {
    let persistence = open_persistence(&database)?;

    let head = persistence
        .head()
        .map_err(|error| error.to_string())?
        .ok_or("the database holds no head yet")?;

    let tasks = persistence.load_tasks().map_err(|error| error.to_string())?;
    let commits = persistence.load_commits().map_err(|error| error.to_string())?;

    println!("head revision: {}", head.0);
    println!("commits:       {}", commits.len());
    println!("tasks:         {}", tasks.len());

    for record in tasks {
        println!(
            "  {} {} [{}] {}",
            record.spec.id, record.spec.kind, record.state, record.spec.objective,
        );
    }

    Ok(())
}

fn export(database: PathBuf, out: PathBuf, report: Option<PathBuf>) -> Result<(), String> {
    let persistence = open_persistence(&database)?;

    let head = persistence
        .head()
        .map_err(|error| error.to_string())?
        .ok_or("the database holds no head yet")?;

    let workspace = persistence
        .load_workspace(head)
        .map_err(|error| error.to_string())?
        .ok_or("the head workspace is missing")?;

    let model = workspace
        .assemble_model()
        .map_err(|error| error.to_string())?;

    let yaml =
        serde_yaml::to_string(&model).map_err(|error| format!("cannot serialize model: {error}"))?;

    std::fs::write(&out, yaml)
        .map_err(|error| format!("cannot write {}: {error}", out.display()))?;

    println!("wrote {} (revision {})", out.display(), model.revision.0);

    if let Some(report_path) = report {
        let errors = conseqa::analyzer::validate(&model);

        if !errors.is_empty() {
            for error in errors {
                let diagnostic = conseqa::analyzer::Diagnostic::from(error);

                eprintln!("error: {}", diagnostic.message);
            }

            return Err("the model fails validation; no report written".to_string());
        }

        let verification = conseqa::analyzer::verification::verify(&model);
        let obligations = conseqa::analyzer::report::obligations(&model, &verification);

        let json = serde_json::to_string_pretty(&obligations)
            .map_err(|error| format!("cannot serialize report: {error}"))?;

        std::fs::write(&report_path, format!("{json}\n"))
            .map_err(|error| format!("cannot write {}: {error}", report_path.display()))?;

        println!("wrote {}", report_path.display());
    }

    Ok(())
}
