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

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Json;
use clap::{Parser, Subcommand};
use serde::Deserialize;

use conseqa::confluence::{
    AnalysisState, ConfluenceEngine, CreateTask, Persistence, PromptEvidence, RunId, RunMetadata,
    TaskBudget, TaskId, TaskKind, WorkspaceState, WriteScope, mcp,
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
        } => serve(database, bind, model, prompt, session).await,
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

async fn serve(
    database: PathBuf,
    bind: SocketAddr,
    model: Option<PathBuf>,
    prompt: Option<String>,
    session: bool,
) -> Result<(), String> {
    let initial = initial_workspace(model, prompt)?;

    let engine = ConfluenceEngine::open(&database, initial)
        .map_err(|error| format!("cannot open {}: {error}", database.display()))?;

    let admin = axum::Router::new()
        .route("/admin/status", get(admin_status))
        .route("/admin/tasks", post(admin_create_task))
        .route("/admin/session", post(admin_create_session))
        .route("/admin/tasks/{task}/cancel", post(admin_cancel_task))
        .route("/admin/analysis/{revision}", get(admin_analysis))
        .with_state(engine.clone());

    let router = mcp::router(engine.clone()).merge(admin);

    let server = mcp::serve_router(router, bind)
        .await
        .map_err(|error| format!("cannot bind {bind}: {error}"))?;

    let mcp_url = format!("http://{}/mcp", server.local_addr);

    println!(
        "conseqa-confluence serving\n  mcp:   {mcp_url}\n  admin: http://{}/admin/status\n  head:  revision {}",
        server.local_addr,
        engine.head_revision().0,
    );

    if session {
        let handle = engine
            .create_session(interactive_scope(), "interactive authoring session")
            .map_err(|error| format!("cannot create session: {error}"))?;

        print_session_config(&mcp_url, &handle.token.0);
    }

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("cannot wait for ctrl-c: {error}"))?;

    println!("shutting down");

    server.shutdown().await;

    Ok(())
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

#[derive(Deserialize)]
struct AdminCreateTask {
    kind: TaskKind,
    objective: String,
    write_scope: WriteScope,

    #[serde(default)]
    prompt_evidence: Vec<PromptEvidence>,
}

async fn admin_create_task(
    State(engine): State<ConfluenceEngine>,
    Json(request): Json<AdminCreateTask>,
) -> Json<serde_json::Value> {
    match engine.create_task(CreateTask {
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
async fn admin_create_session(
    State(engine): State<ConfluenceEngine>,
) -> Json<serde_json::Value> {
    match engine.create_session(interactive_scope(), "interactive authoring session") {
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
    State(engine): State<ConfluenceEngine>,
    Path(task): Path<String>,
) -> Json<serde_json::Value> {
    let Ok(task) = task
        .strip_prefix("task-")
        .unwrap_or(&task)
        .parse::<uuid::Uuid>()
    else {
        return Json(serde_json::json!({ "error": "not a task id" }));
    };

    match engine.cancel_task(TaskId(task)) {
        Ok(()) => Json(serde_json::json!({ "cancelled": true })),
        Err(error) => Json(serde_json::json!({ "error": error.to_string() })),
    }
}

async fn admin_status(State(engine): State<ConfluenceEngine>) -> Json<serde_json::Value> {
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
    State(engine): State<ConfluenceEngine>,
    Path(revision): Path<u64>,
) -> Json<serde_json::Value> {
    let state = engine.analysis_state(Revision(revision));

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
