//! The project-local Conseqa command-line interface.
//!
//! Conseqa project state belongs in `.conseqa/` beside the code it
//! describes. `init` is the only command that works outside a project;
//! every other command discovers the nearest project at or above the
//! current directory, so a pipeline with no MCP client — CI, a script —
//! can check, export, visualize, and design the committed model.
//!
//! Validation errors are fatal: verification is meaningful only over a
//! structurally coherent model. Verification verdicts are epistemic —
//! an unproven obligation is reported with the checker's evidence, not
//! as an error — unless `check --require-proven` asks otherwise.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use conseqa::analyzer::report::{self, ProverReport, Status};
use conseqa::analyzer::{self, Diagnostic, Severity};
use conseqa::design::export::write_artifacts;
use conseqa::design::{
    DesignEngine, PROJECT_DIR, ProjectRead, ProjectStore, mcp, new_project_workspace,
};
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::{
    AgentBackend, RunStatus, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};
use conseqa::spec::Model;

#[derive(Parser)]
#[command(
    name = "conseqa",
    about = "Initialize, inspect, check, export, visualize, and design a Conseqa project"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create minimal Conseqa state in the current directory.
    Init {
        /// Adopt an existing Conseqa model (a `conseqa.yaml`) as the
        /// project's spec instead of starting empty.
        #[arg(long)]
        model: Option<PathBuf>,
    },

    /// Show the Conseqa project's current local state.
    Status,

    /// Validate and verify the project. Exits 1 when an operation has no
    /// program yet or the model is invalid.
    Check {
        /// Also exit 2 when any obligation is not proven.
        #[arg(long)]
        require_proven: bool,

        /// Write the obligation report (JSON) here.
        #[arg(long)]
        report: Option<PathBuf>,
    },

    /// Write the canonical model, the verification report, and the
    /// visualization into a directory.
    Export {
        /// The directory to write into, created if missing.
        dir: PathBuf,

        /// Skip the visualization (`spec.html`).
        #[arg(long)]
        no_render: bool,

        /// The visualization's title; the project's name when omitted.
        #[arg(long)]
        title: Option<String>,
    },

    /// Render the project as a self-contained, interactive HTML page,
    /// with the checker's verdicts overlaid when the model is valid.
    Viz {
        /// Where to write the page; `.conseqa/local/spec.html` when
        /// omitted, or stdout with `--json`.
        #[arg(long)]
        out: Option<PathBuf>,

        /// The page's title; the project's name when omitted.
        #[arg(long)]
        title: Option<String>,

        /// Write the page data (model, graph, proofs, report) as JSON
        /// instead, for the front end's development server.
        #[arg(long)]
        json: bool,
    },

    /// Run the design workflow on the project with coding agents:
    /// decompose the prompt, synthesize every operation, discover and
    /// repair requirements. Exits 2 when the run ends incomplete.
    Design(Box<DesignArgs>),
}

#[derive(Args)]
struct DesignArgs {
    /// What to design, in natural language. Every task of this run sees
    /// it; the project records only the correctness obligations
    /// requirement discovery extracts from it. Without one, the run
    /// works from the project's skeleton and recorded obligations.
    #[arg(long, conflicts_with = "prompt_file")]
    prompt: Option<String>,

    /// Read the prompt from a file.
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// The application source repository, read-only evidence for the
    /// agents; none when omitted.
    #[arg(long)]
    repo: Option<PathBuf>,

    /// Coding-agent backend.
    #[arg(long, value_enum, default_value_t = Backend::Claude)]
    backend: Backend,

    /// Path to the backend executable (for a non-default install).
    #[arg(long)]
    backend_program: Option<String>,

    /// Maximum agent sessions reasoning concurrently during a fanout.
    #[arg(long, default_value_t = 4)]
    max_agents: usize,

    /// Maximum fresh sessions per logical task.
    #[arg(long, default_value_t = 4)]
    max_restarts: u32,

    /// Where to write the finalized model, report, and manifest; the
    /// project's uncommitted `.conseqa/local/design` when omitted.
    #[arg(long)]
    out: Option<PathBuf>,

    #[cfg(feature = "system-one")]
    #[command(flatten)]
    system_one: conseqa::harness::executors::cli::SystemOneArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Claude,
    Codex,
}

fn main() -> ExitCode {
    let outcome = match Cli::parse().command {
        Command::Init { model } => init(model),
        Command::Status => status(),
        Command::Check {
            require_proven,
            report,
        } => check(require_proven, report),
        Command::Export {
            dir,
            no_render,
            title,
        } => export(dir, no_render, title),
        Command::Viz { out, title, json } => viz(out, title, json),
        Command::Design(args) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot start the runtime: {error}"))
            .and_then(|runtime| runtime.block_on(design(*args))),
    };

    match outcome {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|error| format!("cannot read the current directory: {error}"))
}

fn init(model: Option<PathBuf>) -> Result<ExitCode, String> {
    let dir = current_dir()?;

    let workspace = new_project_workspace(&dir, model.as_deref())
        .map_err(|error| format!("cannot initialize Conseqa: {error}"))?;
    let store = ProjectStore::create(&dir, &workspace)
        .map_err(|error| format!("cannot initialize Conseqa: {error}"))?;
    let project = store.project_dir().unwrap_or(&dir);

    println!("initialized {}", project.join(PROJECT_DIR).display());
    println!("commit {PROJECT_DIR} with the project; {PROJECT_DIR}/local is ignored");

    Ok(ExitCode::SUCCESS)
}

/// The directory of the project at or above the current directory.
fn project_dir() -> Result<PathBuf, String> {
    ProjectStore::discover(&current_dir()?).ok_or_else(|| {
        "not inside a Conseqa project; run `conseqa init` from the project root".to_string()
    })
}

/// The project at or above the current directory, read without
/// opening it for writing.
fn read_project() -> Result<(PathBuf, ProjectRead), String> {
    let project = project_dir()?;
    let read = ProjectStore::read(&project)
        .map_err(|error| format!("cannot read {}: {error}", project.display()))?;

    Ok((project, read))
}

/// The project's model, assembled from its head: an error while an
/// operation has no program yet.
fn project_model() -> Result<(PathBuf, ProjectRead, Model), String> {
    let (project, read) = read_project()?;
    let model = read
        .workspace
        .assemble_model()
        .map_err(|error| error.to_string())?;

    Ok((project, read, model))
}

fn status() -> Result<ExitCode, String> {
    let (project, read) = read_project()?;

    println!("project:       {}", read.workspace.run_meta.run);
    println!("root:          {}", project.display());
    println!("head revision: {}", read.workspace.revision.0);
    println!("commits:       {}", read.log.len());
    println!("tasks:         {}", read.tasks.len());

    for record in read.tasks {
        println!(
            "  {} {} [{}] {}",
            record.spec.id, record.spec.kind, record.state, record.spec.objective,
        );
    }

    Ok(ExitCode::SUCCESS)
}

fn check(require_proven: bool, report_path: Option<PathBuf>) -> Result<ExitCode, String> {
    let (_, _, model) = project_model()?;

    let (report, notes) = match analyze(&model) {
        Ok(analysis) => analysis,
        Err(errors) => {
            errors.iter().for_each(print_diagnostic);
            eprintln!("model is invalid; verification not attempted");

            return Ok(ExitCode::FAILURE);
        }
    };

    notes.iter().for_each(print_diagnostic);

    let count = |status: Status| {
        report
            .obligations
            .iter()
            .filter(|obligation| obligation.status == status)
            .count()
    };

    let proven = count(Status::Proven);

    println!(
        "obligations: {proven} proven, {} unknown, {} disproven ({} total)",
        count(Status::Unknown),
        count(Status::Disproven),
        report.obligations.len()
    );

    if let Some(path) = report_path {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|error| format!("cannot serialize the report: {error}"))?;

        write_file(&path, &format!("{json}\n"))?;
    }

    if require_proven && proven < report.obligations.len() {
        eprintln!("not every obligation is proven");

        return Ok(ExitCode::from(2));
    }

    Ok(ExitCode::SUCCESS)
}

fn export(dir: PathBuf, no_render: bool, title: Option<String>) -> Result<ExitCode, String> {
    let (_, read, model) = project_model()?;
    let report = report_or_warn(&model, "no verification report written");

    let title =
        (!no_render).then(|| title.unwrap_or_else(|| read.workspace.run_meta.run.0.clone()));

    let written = write_artifacts(&dir, &model, report.as_ref(), title.as_deref())
        .map_err(|error| error.message)?;

    for path in written {
        println!("wrote {}", path.display());
    }

    Ok(ExitCode::SUCCESS)
}

fn viz(out: Option<PathBuf>, title: Option<String>, json: bool) -> Result<ExitCode, String> {
    let (project, read, model) = project_model()?;
    let report = report_or_warn(&model, "no verdicts overlaid");
    let title = title.unwrap_or_else(|| read.workspace.run_meta.run.0.clone());

    if json {
        let data = conseqa::viz::page_data_json(&model, report.as_ref(), &title)?;

        match out {
            Some(path) => write_file(&path, &format!("{data}\n"))?,
            None => println!("{data}"),
        }

        return Ok(ExitCode::SUCCESS);
    }

    let html = conseqa::viz::render(&model, report.as_ref(), &title)?;
    let path = out.unwrap_or_else(|| project.join(PROJECT_DIR).join("local/spec.html"));

    write_file(&path, &html)?;

    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_lines)]
async fn design(args: DesignArgs) -> Result<ExitCode, String> {
    let DesignArgs {
        prompt,
        prompt_file,
        repo,
        backend,
        backend_program,
        max_agents,
        max_restarts,
        out,
        #[cfg(feature = "system-one")]
        system_one,
    } = args;

    // Validated before anything is opened or served: a run must not
    // start, and nothing may be sent, on settings that are wrong.
    #[cfg(feature = "system-one")]
    let executor = system_one.configure()?;

    let prompt = match (prompt, prompt_file) {
        (Some(prompt), _) => Some(prompt),
        (None, Some(path)) => Some(
            std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        ),
        (None, None) => None,
    };

    let project = project_dir()?;

    let engine = DesignEngine::open_project(&project)
        .map_err(|error| format!("cannot open {}: {error}", project.display()))?;

    if prompt.is_none() && engine.head_snapshot().workspace.operations.is_empty() {
        return Err(
            "nothing to design: the project has no operations yet; say what to build with \
             --prompt or --prompt-file"
                .to_string(),
        );
    }

    // The workers connect to a loopback MCP endpoint on an ephemeral
    // port.
    let bind: SocketAddr = "127.0.0.1:0".parse().expect("valid bind address");

    let server = mcp::serve(engine.clone(), bind)
        .await
        .map_err(|error| format!("cannot start the MCP endpoint: {error}"))?;

    let mcp_url = format!("http://{}/mcp", server.local_addr);

    eprintln!("design MCP endpoint for the agents: {mcp_url}");

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

    let out_dir = out.unwrap_or_else(|| project.join(PROJECT_DIR).join("local/design"));

    let workflow = Workflow::new(
        scheduler,
        WorkflowConfig {
            out_dir,
            analysis_timeout: Duration::from_secs(180),
            max_iterations: 8,
            prompt,
        },
    );

    let report = workflow.run().await.map_err(|error| error.to_string())?;

    server.shutdown().await;

    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("cannot serialize the run report: {error}"))?;

    println!("{json}");

    match report.status {
        RunStatus::Success { .. } => Ok(ExitCode::SUCCESS),
        RunStatus::Incomplete { .. } => Ok(ExitCode::from(2)),
    }
}

/// The checker's verdict on `model`: its obligation report and the
/// verification's notes, or the validation errors that keep it from
/// being verified.
fn analyze(model: &Model) -> Result<(ProverReport, Vec<Diagnostic>), Vec<Diagnostic>> {
    let errors = analyzer::validate(model);

    if !errors.is_empty() {
        return Err(errors.into_iter().map(Diagnostic::from).collect());
    }

    let verification = analyzer::verification::verify(model);

    Ok((
        report::obligations(model, &verification),
        verification.diagnostics(),
    ))
}

/// The obligation report on `model`, or `None` with the validation
/// errors printed and `consequence` said, when it is invalid.
fn report_or_warn(model: &Model, consequence: &str) -> Option<ProverReport> {
    match analyze(model) {
        Ok((report, _)) => Some(report),
        Err(errors) => {
            errors.iter().for_each(print_diagnostic);
            eprintln!("model is invalid; {consequence}");

            None
        }
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }

    std::fs::write(path, contents)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;

    eprintln!("wrote {}", path.display());

    Ok(())
}

fn print_diagnostic(diagnostic: &Diagnostic) {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Unknown => "note",
    };

    let subject = diagnostic
        .subject
        .as_ref()
        .map(|id| format!(" [{id}]"))
        .unwrap_or_default();

    eprintln!("{severity}{subject}: {}", diagnostic.message);

    for evidence in &diagnostic.evidence {
        let subject = evidence
            .subject
            .as_ref()
            .map(|id| format!("[{id}] "))
            .unwrap_or_default();

        eprintln!("    {subject}{}", evidence.message);
    }
}
