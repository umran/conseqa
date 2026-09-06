//! conseqa-harness: orchestration CLI that embeds the confluence
//! engine, starts an MCP endpoint, and launches Claude Code / Codex
//! workers to design an architecture from a prompt (§6.1, §88 of the
//! confluence spec).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use conseqa::confluence::{ConfluenceEngine, RunId, RunMetadata, RunPolicy, WorkspaceState, mcp};
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::{
    AgentBackend, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};

#[derive(Parser)]
#[command(
    name = "conseqa-harness",
    about = "Design a Conseqa architecture from a prompt with concurrent coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the full design workflow: prompt to validated model.
    Design {
        /// The natural-language application prompt.
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,

        /// Read the prompt from a file.
        #[arg(long)]
        prompt_file: Option<PathBuf>,

        /// The application source repository (read-only evidence).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Adopt an existing Conseqa model instead of decomposing.
        #[arg(long)]
        model: Option<PathBuf>,

        /// Coding-agent backend.
        #[arg(long, value_enum, default_value_t = Backend::Claude)]
        backend: Backend,

        /// Authoring database path.
        #[arg(long, default_value = ".conseqa/confluence.redb")]
        database: PathBuf,

        /// Where to write the finalized model and reports.
        #[arg(long, default_value = ".")]
        out: PathBuf,

        /// Adopt only requirements the checker can support strictly.
        #[arg(long)]
        strict_requirements: bool,

        /// Maximum fresh sessions per logical task.
        #[arg(long, default_value_t = 4)]
        max_restarts: u32,

        /// Keep the authoring database after the run.
        #[arg(long)]
        keep_workspace: bool,

        /// Path to the backend executable (for a non-default install).
        #[arg(long)]
        backend_program: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Claude,
    Codex,
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

    match run(cli.command).await {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");

            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn run(command: Command) -> Result<ExitCode, String> {
    let Command::Design {
        prompt,
        prompt_file,
        repo,
        model,
        backend,
        database,
        out,
        strict_requirements,
        max_restarts,
        keep_workspace,
        backend_program,
    } = command;

    let prompt = match (prompt, prompt_file) {
        (Some(prompt), _) => Some(prompt),
        (None, Some(path)) => Some(
            std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        ),
        (None, None) => None,
    };

    if prompt.is_none() && model.is_none() {
        return Err("provide --prompt/--prompt-file or --model".to_string());
    }

    let mut run_meta = RunMetadata::new(RunId(uuid::Uuid::new_v4().to_string()));

    run_meta.prompt = prompt;
    run_meta.policy = RunPolicy {
        strict_requirements,
        adopt_recommended: false,
    };

    let initial = match &model {
        None => WorkspaceState::empty(run_meta),

        Some(path) => {
            let source = std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

            let parsed = conseqa::parser::yaml::parse(&source)
                .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;

            WorkspaceState::from_model(&parsed, run_meta)
        }
    };

    let engine = ConfluenceEngine::open(&database, initial)
        .map_err(|error| format!("cannot open {}: {error}", database.display()))?;

    // The workers connect to a loopback MCP endpoint on an ephemeral
    // port.
    let bind: SocketAddr = "127.0.0.1:0".parse().expect("valid bind address");

    let server = mcp::serve(engine.clone(), bind)
        .await
        .map_err(|error| format!("cannot start MCP server: {error}"))?;

    let mcp_url = format!("http://{}/mcp", server.local_addr);

    println!("confluence MCP serving at {mcp_url}");

    let backend_impl: Arc<dyn AgentBackend> = match backend {
        Backend::Claude => {
            let mut claude = ClaudeCliBackend::new();

            if let Some(program) = backend_program {
                claude = claude.with_program(program);
            }

            Arc::new(claude)
        }

        Backend::Codex => {
            let mut codex = CodexCliBackend::new();

            if let Some(program) = backend_program {
                codex = codex.with_program(program);
            }

            Arc::new(codex)
        }
    };

    let work_dir = database
        .parent()
        .map(|parent| parent.join("harness-work"))
        .unwrap_or_else(|| PathBuf::from(".conseqa/harness-work"));

    let supervisor = Supervisor::new(
        engine.clone(),
        backend_impl,
        mcp_url,
        repo,
        work_dir,
    );

    let scheduler = Scheduler::new(
        engine.clone(),
        supervisor,
        SchedulerPolicy {
            max_attempts: max_restarts,
            ..Default::default()
        },
    );

    let mut workflow = Workflow::new(
        scheduler,
        WorkflowConfig {
            out_dir: out,
            analysis_timeout: Duration::from_secs(180),
            max_iterations: 8,
        },
    );

    let report = workflow.run().await.map_err(|error| error.to_string())?;

    server.shutdown().await;

    if !keep_workspace {
        // Leave the database in place by default for audit; a future
        // flag could remove it. keep_workspace is accepted for CLI
        // stability.
        let _ = keep_workspace;
    }

    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("cannot serialize report: {error}"))?;

    println!("{json}");

    match report.status {
        conseqa::harness::RunStatus::Success { .. } => Ok(ExitCode::SUCCESS),
        conseqa::harness::RunStatus::Incomplete { .. } => Ok(ExitCode::from(2)),
    }
}
