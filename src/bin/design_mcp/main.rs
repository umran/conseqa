//! conseqa-design-mcp: the standalone design daemon (§6.2, §88 of
//! the harness spec).
//!
//! `serve` exposes the shared MCP endpoint for interactive Claude
//! Code / Codex sessions, IDE integration, and external orchestrators,
//! plus a minimal localhost admin surface for creating tasks; `global`
//! and `stdio` host many projects for one coordinator. Every serving
//! mode builds designs in process (`build_design`); coding agents run
//! over a design from the `conseqa design` command instead. `decider`
//! asks a System One decider directly and vets a server before it is
//! trusted. Project lifecycle commands live in the project-local
//! `conseqa` CLI.
//!
//! A project is the `.conseqa/` directory of the repository it
//! describes (see `conseqa::design::store`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Json;
use clap::{Parser, Subcommand};
use serde::Deserialize;

use conseqa::design::mcp::{self, DesignBuilder};
use conseqa::design::{
    AnalysisState, CreateTask, DesignEngine, PromptEvidence, TaskBudget, TaskId, TaskKind,
    WriteScope,
};
use conseqa::harness::InProcessBuilder;
use conseqa::spec::Revision;

#[cfg(feature = "system-one")]
mod decider;

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

        /// On startup, create an interactive authoring session and
        /// print a ready-to-paste `.mcp.json` for the Claude Code UI.
        /// The session can commit repeatedly under one token.
        #[arg(long)]
        session: bool,

        /// Where a finished build writes its model, report, and manifest;
        /// the project's uncommitted `.conseqa/local/design` when omitted.
        #[arg(long)]
        design_out: Option<PathBuf>,

        #[command(flatten)]
        decider: Decider,
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

        #[command(flatten)]
        decider: Decider,
    },

    /// Serve one shared multi-project instance over stdio, for the
    /// Claude Desktop / Code UI. The app spawns this command and speaks
    /// MCP over stdin/stdout — no URL, no TLS, no certificate. The
    /// project the app starts it in is the default one; others are
    /// opened and created by path through the MCP project tools.
    Stdio {
        #[command(flatten)]
        decider: Decider,
    },

    /// Ask a System One decider directly, or vet a server before it is
    /// trusted.
    #[cfg(feature = "system-one")]
    Decider {
        #[command(subcommand)]
        command: Box<decider::DeciderCommand>,
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
            session,
            design_out,
            decider,
        } => {
            serve(ServeOptions {
                project,
                bind,
                model,
                session,
                design_out,
                decider,
            })
            .await
        }
        Command::Global {
            bind,
            api_key,
            decider,
        } => serve_global(bind, api_key, decider).await,
        Command::Stdio { decider } => serve_stdio_cmd(decider).await,

        #[cfg(feature = "system-one")]
        Command::Decider { command } => {
            return decider::run(*command).await.unwrap_or_else(|message| {
                eprintln!("error: {message}");

                ExitCode::FAILURE
            });
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

/// The decider a build's System One builders ask (§26 of the System One
/// orchestration revision). Nothing is asked unless one is explicitly
/// configured; without one, the builders settle what code alone can.
#[cfg(feature = "system-one")]
type Decider = conseqa::harness::executors::cli::DeciderArgs;

/// Without the feature the flags do not exist, and there are no
/// builders.
#[cfg(not(feature = "system-one"))]
#[derive(clap::Args, Debug, Clone)]
struct Decider {}

/// The server's design builder. Validates the settings, and says what a
/// build will send where, before anything is opened or served: a daemon
/// must not come up on settings that are wrong.
#[cfg(feature = "system-one")]
fn builder(decider: &Decider, design_out: Option<PathBuf>) -> Result<Arc<dyn DesignBuilder>, String> {
    let builders = conseqa::harness::executors::cli::every_builder(decider)?;

    Ok(Arc::new(InProcessBuilder::new(builders).with_design_out(design_out)))
}

#[cfg(not(feature = "system-one"))]
fn builder(_: &Decider, design_out: Option<PathBuf>) -> Result<Arc<dyn DesignBuilder>, String> {
    Ok(Arc::new(InProcessBuilder::new().with_design_out(design_out)))
}

struct ServeOptions {
    project: PathBuf,
    bind: SocketAddr,
    model: Option<PathBuf>,
    session: bool,
    design_out: Option<PathBuf>,
    decider: Decider,
}

async fn serve(options: ServeOptions) -> Result<(), String> {
    let builder = builder(&options.decider, options.design_out)?;

    let initial =
        conseqa::design::new_project_workspace(&options.project, options.model.as_deref())
            .map_err(|error| error.to_string())?;

    let engine = DesignEngine::open(&options.project, initial)
        .map_err(|error| format!("cannot open {}: {error}", options.project.display()))?;

    let admin = axum::Router::new()
        .route("/admin/status", get(admin_status))
        .route("/admin/tasks", post(admin_create_task))
        .route("/admin/session", post(admin_create_session))
        .route("/admin/tasks/{task}/cancel", post(admin_cancel_task))
        .route("/admin/analysis/{revision}", get(admin_analysis))
        .with_state(AdminState {
            engine: engine.clone(),
        });

    let router = mcp::router_with_builder(engine.clone(), builder).merge(admin);

    let server = mcp::serve_router(router, options.bind)
        .await
        .map_err(|error| format!("cannot bind {}: {error}", options.bind))?;

    let url = format!("http://{}/mcp", server.local_addr);

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

async fn serve_global(
    bind: SocketAddr,
    api_key: Option<String>,
    decider: Decider,
) -> Result<(), String> {
    let builder = builder(&decider, None)?;

    let api_key = load_or_create_api_key(api_key)?;

    let manager = Arc::new(conseqa::design::WorkspaceManager::new());

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

    let router = mcp::router_multi(Arc::clone(&manager), api_key.clone(), Some(builder)).merge(admin);

    let server = mcp::serve_router(router, bind)
        .await
        .map_err(|error| format!("cannot bind {bind}: {error}"))?;

    let url = format!("http://{}/mcp", server.local_addr);

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

async fn serve_stdio_cmd(decider: Decider) -> Result<(), String> {
    let builder = builder(&decider, None)?;

    let manager = Arc::new(conseqa::design::WorkspaceManager::new());

    // The app starts the server in the user's working directory; the
    // project there is the one meant when a client names none.
    let home = std::env::current_dir().ok();

    tracing::info!(
        home = %home.as_deref().unwrap_or(std::path::Path::new("?")).display(),
        "conseqa stdio server ready"
    );

    // Blocks until the UI client disconnects.
    mcp::serve_stdio(manager, Some(builder), home)
        .await
        .map_err(|error| format!("stdio server error: {error}"))
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
         \x20  server, and ask it to use the conseqa tools — start with `spec_status`\n\
         \x20  and `dsl_guide`.\n\n\
         The session commits repeatedly under this one token. Keep this daemon\n\
         running; the token is valid only for this run.\n"
    );
}

/// Shared state for the admin surface: the engine.
#[derive(Clone)]
struct AdminState {
    engine: DesignEngine,
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
