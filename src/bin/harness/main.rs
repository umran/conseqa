//! conseqa-harness: orchestration CLI that embeds the confluence
//! engine, starts an MCP endpoint, and launches Claude Code / Codex
//! workers to design an architecture from a prompt (§6.1, §88 of the
//! confluence spec).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use conseqa::confluence::{
    ConfluenceEngine, ProjectStore, RunId, RunMetadata, RunPolicy, WorkspaceState, mcp,
};
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::{
    AgentBackend, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};

#[cfg(feature = "system-one")]
mod decider;

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
    Design(Box<DesignArgs>),

    /// Ask a System One decider directly, or vet a server before it is
    /// trusted.
    #[cfg(feature = "system-one")]
    Decider {
        #[command(subcommand)]
        command: Box<decider::DeciderCommand>,
    },
}

#[derive(Args)]
struct DesignArgs {
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

    /// The project: its directory or any directory inside it. Created
    /// there from the prompt or model when none exists; an existing
    /// project resumes from its files.
    #[arg(long, default_value = ".")]
    project: PathBuf,

    /// Where to write the finalized model and reports.
    #[arg(long, default_value = ".")]
    out: PathBuf,

    /// Adopt only requirements the checker can support strictly.
    #[arg(long)]
    strict_requirements: bool,

    /// Maximum fresh sessions per logical task.
    #[arg(long, default_value_t = 4)]
    max_restarts: u32,

    /// Maximum agent sessions reasoning concurrently during a
    /// fanout.
    #[arg(long, default_value_t = 4)]
    max_agents: usize,

    /// Path to the backend executable (for a non-default install).
    #[arg(long)]
    backend_program: Option<String>,

    #[cfg(feature = "system-one")]
    #[command(flatten)]
    system_one: conseqa::harness::executors::cli::SystemOneArgs,
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

async fn run(command: Command) -> Result<ExitCode, String> {
    match command {
        Command::Design(args) => design(*args).await,

        #[cfg(feature = "system-one")]
        Command::Decider { command } => decider::run(*command).await,
    }
}

#[allow(clippy::too_many_lines)]
async fn design(args: DesignArgs) -> Result<ExitCode, String> {
    let DesignArgs {
        prompt,
        prompt_file,
        repo,
        model,
        backend,
        project,
        out,
        strict_requirements,
        max_restarts,
        max_agents,
        backend_program,
        #[cfg(feature = "system-one")]
        system_one,
    } = args;

    let prompt = match (prompt, prompt_file) {
        (Some(prompt), _) => Some(prompt),
        (None, Some(path)) => Some(
            std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        ),
        (None, None) => None,
    };

    let existing = ProjectStore::discover(&project);

    match &existing {
        None if prompt.is_none() && model.is_none() => {
            return Err(format!(
                "no project at {} yet: provide --prompt/--prompt-file or --model to create one",
                project.display()
            ));
        }

        Some(found) if prompt.is_some() || model.is_some() => eprintln!(
            "note: resuming the project at {}; --prompt and --model apply only when creating one",
            found.display()
        ),

        _ => {}
    }

    // Validated before anything is opened or served: a run must not
    // start, and nothing may be sent, on settings that are wrong.
    #[cfg(feature = "system-one")]
    let executor = system_one.configure()?;

    let name = project
        .canonicalize()
        .ok()
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "project".to_string());

    let mut run_meta = RunMetadata::new(RunId(name));

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

    let engine = ConfluenceEngine::open(&project, initial)
        .map_err(|error| format!("cannot open {}: {error}", project.display()))?;

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

    #[cfg(feature = "system-one")]
    let backend_impl = match executor {
        Some(executor) => executor.wrap(&engine, backend_impl),
        None => backend_impl,
    };

    // The workers' scratch directories hold only their MCP
    // configuration; outside every repository, so no worker picks up a
    // project's own agent settings.
    let supervisor = Supervisor::new(
        engine.clone(),
        backend_impl,
        mcp_url,
        repo,
        std::env::temp_dir().join("conseqa-work"),
    );

    let scheduler = Scheduler::new(
        engine.clone(),
        supervisor,
        SchedulerPolicy {
            max_attempts: max_restarts,
            max_concurrent_agents: max_agents.max(1),
            ..Default::default()
        },
    );

    let workflow = Workflow::new(
        scheduler,
        WorkflowConfig {
            out_dir: out,
            analysis_timeout: Duration::from_secs(180),
            max_iterations: 8,
            objective: None,
        },
    );

    let report = workflow.run().await.map_err(|error| error.to_string())?;

    server.shutdown().await;

    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("cannot serialize report: {error}"))?;

    println!("{json}");

    match report.status {
        conseqa::harness::RunStatus::Success { .. } => Ok(ExitCode::SUCCESS),
        conseqa::harness::RunStatus::Incomplete { .. } => Ok(ExitCode::from(2)),
    }
}
