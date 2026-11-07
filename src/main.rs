//! The project-local Conseqa command-line interface.
//!
//! Conseqa project state belongs in `.conseqa/` beside the code it
//! describes. `init` is the only command that works outside a project;
//! every other command discovers the nearest project at or above the
//! current directory, so a pipeline with no MCP client — CI, a script —
//! can check, diff, export, visualize, and design the committed model.
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

use conseqa::analyzer::basis::TextualKey;
use conseqa::analyzer::report::{self, ProverReport, Status};
use conseqa::analyzer::{self, Diagnostic, Severity};
use conseqa::conformance::{
    self,
    attest::{AttestError, AttestInput},
    document::ReviewResult,
    plan::Scope,
    record::Party,
    state::Derivation,
};
use conseqa::design::change::{self, diff::Gate, render, side::Source};
use conseqa::design::export::write_artifacts;
use conseqa::design::{
    DesignEngine, PROJECT_DIR, ProjectRead, ProjectStore, mcp, new_project_workspace,
};
use conseqa::harness::backends::{ClaudeCliBackend, CodexCliBackend};
use conseqa::harness::review::{ReviewBackend, ReviewRun, Reviewer};
use conseqa::harness::{
    AgentBackend, RunStatus, Scheduler, SchedulerPolicy, Supervisor, Workflow, WorkflowConfig,
};
use conseqa::realization::catalogue::Catalogue;
use conseqa::realization::choice::{
    self, ChooseError, ChooseRequest, Explanation, Profile, Refusal,
};
use conseqa::realization::document::{self as realization_document, ChoiceOutcome, Listing};
use conseqa::spec::Model;

#[derive(Parser)]
#[command(
    name = "conseqa",
    about = "Initialize, inspect, check, diff, export, visualize, and design a Conseqa project, \
             judge which technologies can realize it, and hold its code to it"
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
    /// program yet, the model is invalid, or the realization profile
    /// `--require-realizable` reads does not load.
    Check {
        /// Also exit 2 when any obligation is not proven.
        #[arg(long)]
        require_proven: bool,

        /// Also exit 2 when a realizable declaration is ambiguous, has an
        /// invalid choice, or is infeasible on the project's stack.
        #[arg(long)]
        require_realizable: bool,

        /// Write the obligation report (JSON) here.
        #[arg(long)]
        report: Option<PathBuf>,
    },

    /// Compare two versions of the project's spec: the declarations
    /// added, removed, and modified, and what changed in what they
    /// prove. Exits 1 when a side cannot be read, and 2 when a gate
    /// named by `--fail-on` fails.
    Diff {
        /// The base, a commit expression; `HEAD` when omitted.
        base: Option<String>,

        /// The head, a commit expression; the working tree when omitted.
        head: Option<String>,

        /// Read the base at its merge base with the head: what a branch
        /// changed, not what its target changed since the branch point.
        #[arg(long)]
        merge_base: bool,

        /// Write the diff document (JSON) here.
        #[arg(long)]
        report: Option<PathBuf>,

        /// Exit 2 when any of these occurs.
        #[arg(long, value_enum, value_delimiter = ',')]
        fail_on: Vec<FailOn>,
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

    /// Hold the project's code to its spec: each declaration's
    /// conformance, the work the code owes, attests, reviews, the
    /// binding markers, and CI gates. `--json` prints each command's
    /// conformance document.
    #[command(subcommand)]
    Conform(ConformCommand),

    /// Judge which technologies realize the spec: each realizable
    /// declaration's needs and state, the candidates for one, the
    /// architect's choices, and the catalogue of entries. `--json`
    /// prints each command's realization document.
    #[command(subcommand)]
    Realize(RealizeCommand),
}

#[derive(Subcommand)]
enum RealizeCommand {
    /// Each realizable declaration's needs and state: chosen, automatic,
    /// ambiguous, invalid_choice, infeasible, or unneeded. Exits 1 when
    /// the spec, the stack, or the catalogue does not load.
    Status {
        /// Also exit 2 when a declaration is ambiguous, has an invalid
        /// choice, or is infeasible.
        #[arg(long)]
        require_realizable: bool,

        /// Print the status document instead of the summary.
        #[arg(long)]
        json: bool,
    },

    /// Every candidate for one declaration: what each provides, the
    /// needs it misses and why, and the kind's entries outside the
    /// stack. Exits 1 when the declaration is not realizable.
    Explain {
        /// The declaration's textual key, e.g. "data_model data.orders".
        key: String,

        /// Print the explain document instead of the report.
        #[arg(long)]
        json: bool,
    },

    /// Choose the entry that realizes a declaration, written to its
    /// choice file. Exits 2 when refused, naming what the entry lacks.
    Choose {
        /// The declaration's textual key.
        key: String,

        /// The entry's id, or its id at a version, e.g.
        /// "kafka.keyed-topic@1".
        entry: String,

        /// An option of the entry to choose with it; repeat for several.
        #[arg(long = "option")]
        options: Vec<String>,

        /// Why it was chosen, recorded in the choice file.
        #[arg(long)]
        reason: Option<String>,

        /// Print the choice document.
        #[arg(long)]
        json: bool,
    },

    /// The entries the project can choose from: the standard
    /// catalogue's and its own. Lists them when no action is given.
    Catalogue {
        #[command(subcommand)]
        action: Option<CatalogueAction>,
    },
}

#[derive(Subcommand)]
enum CatalogueAction {
    /// Every entry, with its technology and what it realizes.
    List {
        /// Print the catalogue document instead of the list.
        #[arg(long)]
        json: bool,
    },

    /// One entry in full. Exits 1 when no catalogue holds it.
    Show {
        /// The entry's id, or its id at a version.
        entry: String,

        /// Print the entry document instead of its YAML.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConformCommand {
    /// Each bindable declaration's conditions, and whether each proven
    /// obligation's code conforms. Exits 1 when the state cannot be
    /// derived.
    Status {
        /// Print the status document instead of the summary.
        #[arg(long)]
        json: bool,

        /// Write the status document here.
        #[arg(long)]
        report: Option<PathBuf>,
    },

    /// The ordered work list: what the code owes the spec, item by item.
    Plan {
        #[command(flatten)]
        scope: ScopeArgs,

        /// Print the plan document instead of the list.
        #[arg(long)]
        json: bool,
    },

    /// The work item for one declaration, or an orphaned key: everything
    /// an agent needs to make its code conform. Exits 1 when the key has
    /// none.
    Item {
        /// The declaration's textual key, e.g.
        /// "transaction operation.apply_payment/tx.apply_payment".
        key: String,

        /// Print the item document instead of the markdown.
        #[arg(long)]
        json: bool,
    },

    /// Record what a declaration's code was checked to provide, from an
    /// attest's JSON, as the MCP tool takes it, stamped with git's
    /// `user.name`. Exits 2 when refused.
    Attest {
        /// The attest's JSON; `-` reads it from stdin.
        #[arg(long)]
        input: PathBuf,

        /// Print the attest document.
        #[arg(long)]
        json: bool,
    },

    /// Review records with fresh-context agents, one per record: the
    /// named declarations', or every record with items the policy
    /// requires reviewed that no review confirms. Exits 2 when any is
    /// left unreviewed.
    Review(Box<ConformReviewArgs>),

    /// The binding markers in the project's files: every region and
    /// every scan error, read without the spec. Exits 2 on a scan
    /// error.
    Scan {
        /// Print the scan document instead of the list.
        #[arg(long)]
        json: bool,
    },

    /// Gate CI: fails on every scan error, and on the conditions
    /// `--fail-on` names. Given a base or a spec range, gates only on
    /// what that change owes. Exits 2 when a gate fails.
    Check {
        #[command(flatten)]
        scope: ScopeArgs,

        /// Also fail on these: conditions by name (unbound, orphaned,
        /// unattested, stale, drifted, missing_facts, incomplete,
        /// surplus, unreviewed, disputed), or not_conformant.
        #[arg(long, value_delimiter = ',')]
        fail_on: Vec<String>,

        /// Write the check document here.
        #[arg(long)]
        report: Option<PathBuf>,

        /// Print the check document instead of the outcome.
        #[arg(long)]
        json: bool,
    },
}

/// What scopes the work list or a check to a spec change.
#[derive(Args)]
struct ScopeArgs {
    /// Scope to what the spec's changes since this commit owe, from it
    /// to the working tree.
    base: Option<String>,

    /// Read the base at its merge base with the working tree.
    #[arg(long, requires = "base")]
    merge_base: bool,

    /// Scope to what a range of spec commits owes, `<a>..<b>`, instead.
    #[arg(long, conflicts_with = "base")]
    spec_range: Option<String>,
}

#[derive(Args)]
struct ConformReviewArgs {
    /// The declarations whose records to review; every record with
    /// unreviewed items when omitted.
    keys: Vec<String>,

    /// Coding-agent backend.
    #[arg(long, value_enum)]
    backend: Backend,

    /// Path to the backend executable (for a non-default install).
    #[arg(long)]
    backend_program: Option<String>,

    /// The model the agents run on, recorded with their reviews.
    #[arg(long)]
    model: Option<String>,

    /// Maximum agents reviewing at once.
    #[arg(long, default_value_t = 4)]
    max_agents: usize,

    /// Print the review document instead of the outcomes.
    #[arg(long)]
    json: bool,
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

/// The gates `diff --fail-on` names (§15.3 of the spec change analysis
/// revision).
#[derive(Debug, Clone, Copy, ValueEnum)]
enum FailOn {
    /// Any obligation regressed.
    Regressed,

    /// Any obligation was removed, whatever its base status.
    Removed,

    /// Any added obligation is not proven.
    AddedUnproven,

    /// Any obligation was rebased.
    Rebased,

    /// Any entry is unexplained.
    Unexplained,

    /// Any declaration on head is infeasible on its stack, or has an
    /// invalid choice.
    Infeasible,

    /// Any declaration on head is ambiguous on its stack.
    Ambiguous,
}

impl From<FailOn> for Gate {
    fn from(gate: FailOn) -> Self {
        match gate {
            FailOn::Regressed => Self::Regressed,
            FailOn::Removed => Self::Removed,
            FailOn::AddedUnproven => Self::AddedUnproven,
            FailOn::Rebased => Self::Rebased,
            FailOn::Unexplained => Self::Unexplained,
            FailOn::Infeasible => Self::Infeasible,
            FailOn::Ambiguous => Self::Ambiguous,
        }
    }
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
            require_realizable,
            report,
        } => check(require_proven, require_realizable, report),
        Command::Diff {
            base,
            head,
            merge_base,
            report,
            fail_on,
        } => diff(base, head, merge_base, report, &fail_on),
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
        Command::Conform(command) => conform(command),
        Command::Realize(command) => realize(command),
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

fn check(
    require_proven: bool,
    require_realizable: bool,
    report_path: Option<PathBuf>,
) -> Result<ExitCode, String> {
    let (project, _, model) = project_model()?;

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

    let mut failed = false;

    if require_proven && proven < report.obligations.len() {
        eprintln!("not every obligation is proven");

        failed = true;
    }

    if require_realizable && !realizable(&project, &model)? {
        failed = true;
    }

    Ok(if failed {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

/// Whether every realizable declaration is realizable on the project's
/// stack: the gate `--require-realizable` applies, printing each
/// declaration that fails it. Without a stack profile nothing is
/// realized, and the gate passes.
fn realizable(project: &Path, model: &Model) -> Result<bool, String> {
    let status = choice::Status::derive(project, model).map_err(|error| error.to_string())?;

    if status.profile.is_none() {
        println!("realization: no stack profile, so nothing to realize");

        return Ok(true);
    }

    let failing: Vec<_> = status.failing().collect();

    println!(
        "realization: {} realizable declarations, {} not realizable",
        status.declarations.len(),
        failing.len()
    );

    for realized in &failing {
        eprintln!("{} {}", realized.state.name(), realized.declaration);
    }

    if !failing.is_empty() {
        eprintln!("not every realizable declaration is realizable");
    }

    Ok(failing.is_empty())
}

fn diff(
    base: Option<String>,
    head: Option<String>,
    merge_base: bool,
    report_path: Option<PathBuf>,
    fail_on: &[FailOn],
) -> Result<ExitCode, String> {
    let project = project_dir()?;
    let head = head.map_or(Source::WorkingTree, Source::Commit);

    let document = change::run(
        &project,
        base.as_deref().unwrap_or("HEAD"),
        &head,
        merge_base,
    )
    .map_err(|error| error.to_string())?;

    print!("{}", render::render(&document));

    if let Some(path) = report_path {
        let json = serde_json::to_string_pretty(&document)
            .map_err(|error| format!("cannot serialize the diff: {error}"))?;

        write_file(&path, &format!("{json}\n"))?;
    }

    let gates: Vec<Gate> = fail_on.iter().map(|gate| Gate::from(*gate)).collect();
    let failed = document.failed_gates(&gates);

    for gate in &failed {
        eprintln!("gate failed: {gate}");
    }

    Ok(if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn realize(command: RealizeCommand) -> Result<ExitCode, String> {
    match command {
        RealizeCommand::Status {
            require_realizable,
            json,
        } => realize_status(require_realizable, json),
        RealizeCommand::Explain { key, json } => realize_explain(&key, json),
        RealizeCommand::Choose {
            key,
            entry,
            options,
            reason,
            json,
        } => realize_choose(&key, entry, options, reason, json),
        RealizeCommand::Catalogue { action } => {
            realize_catalogue(action.unwrap_or(CatalogueAction::List { json: false }))
        }
    }
}

/// The project's model, validated: what realization derives needs from.
fn realizable_model() -> Result<(PathBuf, Model), String> {
    let (project, _, model) = project_model()?;

    let errors = analyzer::validate(&model);

    if !errors.is_empty() {
        for error in errors {
            print_diagnostic(&Diagnostic::from(error));
        }

        return Err("the model is invalid, so it has no needs to realize".to_string());
    }

    Ok((project, model))
}

fn realize_status(require_realizable: bool, json: bool) -> Result<ExitCode, String> {
    let (project, model) = realizable_model()?;
    let status = choice::Status::derive(&project, &model).map_err(|error| error.to_string())?;

    if json {
        println!("{}", realization_document::status(&status).to_pretty_json());
    } else {
        print!("{}", status.render());
    }

    if require_realizable && status.failing().next().is_some() {
        eprintln!("not every realizable declaration is realizable");

        return Ok(ExitCode::from(2));
    }

    Ok(ExitCode::SUCCESS)
}

fn realize_explain(key: &str, json: bool) -> Result<ExitCode, String> {
    let key: TextualKey = key
        .parse()
        .map_err(|error| format!("`{key}` is not a textual key: {error}"))?;

    let (project, model) = realizable_model()?;

    let profile = Profile::read(&project)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| Refusal::NoStack.to_string())?;

    let explanation =
        Explanation::of(&profile, &model, &key).map_err(|refusal| refusal.to_string())?;

    if json {
        println!(
            "{}",
            realization_document::explain(&explanation).to_pretty_json()
        );
    } else {
        print!("{}", explanation.render());
    }

    Ok(ExitCode::SUCCESS)
}

fn realize_choose(
    key: &str,
    entry: String,
    options: Vec<String>,
    reason: Option<String>,
    json: bool,
) -> Result<ExitCode, String> {
    let declaration: TextualKey = key
        .parse()
        .map_err(|error| format!("`{key}` is not a textual key: {error}"))?;

    let (project, model) = realizable_model()?;

    let request = ChooseRequest {
        declaration,
        entry,
        options,
        reason,
    };

    let outcome = match choice::choose(&project, &model, &request) {
        Ok(chosen) => Ok(chosen),
        Err(ChooseError::Refused(refusal)) => Err(refusal),
        Err(ChooseError::Failed(error)) => return Err(error.to_string()),
    };

    if json {
        let document = realization_document::choice(ChoiceOutcome::of(
            outcome.as_ref().map_err(|refusal| refusal.as_ref()),
        ));

        println!("{}", document.to_pretty_json());
    }

    match outcome {
        Ok(chosen) => {
            if !json {
                print!("{}", chosen.render());
            }

            Ok(ExitCode::SUCCESS)
        }

        Err(refusal) => {
            if !json {
                eprintln!("refused: {refusal}");
            }

            Ok(ExitCode::from(2))
        }
    }
}

fn realize_catalogue(action: CatalogueAction) -> Result<ExitCode, String> {
    // The standard catalogue reads anywhere; inside a project, the
    // project's own entries join it.
    let catalogue = match ProjectStore::discover(&current_dir()?) {
        Some(project) => Catalogue::read(&project),
        None => Catalogue::standard(),
    }
    .map_err(|error| error.to_string())?;

    match action {
        CatalogueAction::List { json } => {
            let listing = Listing::of(&catalogue);

            if json {
                println!(
                    "{}",
                    realization_document::catalogue(&listing).to_pretty_json()
                );
            } else {
                print!("{}", listing.render());
            }
        }

        CatalogueAction::Show { entry, json } => {
            let (id, version) = match entry.rsplit_once('@') {
                Some((id, version)) => (id, Some(version)),
                None => (entry.as_str(), None),
            };

            let catalogued = catalogue
                .get(id)
                .filter(|catalogued| {
                    version.is_none_or(|version| catalogued.entry.version().to_string() == version)
                })
                .ok_or_else(|| format!("no catalogue holds `{entry}`"))?;

            if json {
                println!(
                    "{}",
                    realization_document::catalogue_entry(catalogued).to_pretty_json()
                );
            } else {
                let yaml = serde_yaml::to_string(&catalogued.entry)
                    .map_err(|error| format!("cannot write `{entry}` as YAML: {error}"))?;

                println!("# {} ({})", catalogued.entry.entry_ref(), catalogued.source);
                print!("{yaml}");
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn conform(command: ConformCommand) -> Result<ExitCode, String> {
    match command {
        ConformCommand::Status { json, report } => conform_status(json, report),
        ConformCommand::Plan { scope, json } => conform_plan(&scope, json),
        ConformCommand::Item { key, json } => conform_item(&key, json),
        ConformCommand::Attest { input, json } => conform_attest(&input, json),
        ConformCommand::Review(args) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot start the runtime: {error}"))
            .and_then(|runtime| runtime.block_on(conform_review(*args))),
        ConformCommand::Scan { json } => conform_scan(json),
        ConformCommand::Check {
            scope,
            fail_on,
            report,
            json,
        } => conform_check(&scope, &fail_on, report, json),
    }
}

fn conform_status(json: bool, report: Option<PathBuf>) -> Result<ExitCode, String> {
    let (project, read) = read_project()?;
    let derivation =
        Derivation::of(&project, &read.workspace).map_err(|error| error.to_string())?;
    let document = conformance::document::status(&derivation.state);

    if json {
        println!("{}", document.to_pretty_json());
    } else {
        print!("{}", derivation.state.summary());
    }

    if let Some(path) = report {
        write_file(&path, &format!("{}\n", document.to_pretty_json()))?;
    }

    Ok(ExitCode::SUCCESS)
}

/// The scope a base or a spec range names, if one does.
fn conform_scope(
    project: &Path,
    workspace: &conseqa::design::WorkspaceState,
    scope: &ScopeArgs,
) -> Result<Option<Scope>, String> {
    match (&scope.base, &scope.spec_range) {
        (Some(base), _) => Scope::since(project, base, scope.merge_base, workspace).map(Some),
        (None, Some(range)) => Scope::range(project, range).map(Some),
        (None, None) => Ok(None),
    }
}

fn conform_plan(scope: &ScopeArgs, json: bool) -> Result<ExitCode, String> {
    let (project, read) = read_project()?;
    let derivation =
        Derivation::of(&project, &read.workspace).map_err(|error| error.to_string())?;
    let scope = conform_scope(&project, &read.workspace, scope)?;

    let work = conformance::plan::plan(&derivation);

    let work = match &scope {
        Some(scope) => work.scoped(scope, &derivation),
        None => work,
    };

    if json {
        println!(
            "{}",
            conformance::document::plan(&work.items, scope.as_ref()).to_pretty_json()
        );
    } else {
        print!("{}", work.render());
    }

    Ok(ExitCode::SUCCESS)
}

fn conform_item(key: &str, json: bool) -> Result<ExitCode, String> {
    let key: TextualKey = key
        .parse()
        .map_err(|error| format!("`{key}` is not a textual key: {error}"))?;

    let (project, read) = read_project()?;
    let derivation =
        Derivation::of(&project, &read.workspace).map_err(|error| error.to_string())?;

    let packet = conformance::packet::packet(&derivation, &key).ok_or_else(|| {
        format!(
            "`{key}` has no work item: it is neither a bindable declaration of the spec nor an \
             orphaned key"
        )
    })?;

    if json {
        println!("{}", conformance::document::item(&packet).to_pretty_json());
    } else {
        print!("{}", packet.render());
    }

    Ok(ExitCode::SUCCESS)
}

fn conform_attest(input: &Path, json: bool) -> Result<ExitCode, String> {
    let text = if input == Path::new("-") {
        std::io::read_to_string(std::io::stdin())
            .map_err(|error| format!("cannot read the attest from stdin: {error}"))?
    } else {
        std::fs::read_to_string(input)
            .map_err(|error| format!("cannot read {}: {error}", input.display()))?
    };

    let input: AttestInput = serde_json::from_str(&text)
        .map_err(|error| format!("the attest does not parse: {error}"))?;

    let (project, read) = read_project()?;
    let by = Party::person(&project).map_err(|error| error.to_string())?;

    let outcome = match conformance::attest::attest(&project, &read.workspace, &input, by) {
        Ok(attested) => Ok(attested),
        Err(AttestError::Refused(refusal)) => Err(refusal),
        Err(AttestError::Failed(error)) => return Err(error.to_string()),
    };

    let document = conformance::document::attest(conformance::document::AttestOutcome::of(
        outcome.as_ref().map_err(|refusal| refusal.as_ref()),
    ));

    if json {
        println!("{}", document.to_pretty_json());
    }

    match outcome {
        Ok(attested) => {
            if !json {
                println!(
                    "{} {}",
                    if attested.written {
                        "wrote"
                    } else {
                        "unchanged"
                    },
                    attested.record
                );
            }

            Ok(ExitCode::SUCCESS)
        }

        Err(refusal) => {
            eprintln!("attest refused: {refusal}");

            Ok(ExitCode::from(2))
        }
    }
}

async fn conform_review(args: ConformReviewArgs) -> Result<ExitCode, String> {
    let ConformReviewArgs {
        keys,
        backend,
        backend_program,
        model,
        max_agents,
        json,
    } = args;

    let keys = keys
        .iter()
        .map(|key| {
            key.parse::<TextualKey>()
                .map_err(|error| format!("`{key}` is not a textual key: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let project = project_dir()?;

    let (provider, backend) = match backend {
        Backend::Claude => {
            let mut claude = ClaudeCliBackend::new();

            if let Some(program) = &backend_program {
                claude = claude.with_program(program);
            }

            if let Some(model) = &model {
                claude = claude.with_model(model);
            }

            ("claude", ReviewBackend::Claude(claude))
        }

        Backend::Codex => {
            let mut codex = CodexCliBackend::new();

            if let Some(program) = &backend_program {
                codex = codex.with_program(program);
            }

            if let Some(model) = &model {
                codex = codex.with_model(model);
            }

            ("codex", ReviewBackend::Codex(codex))
        }
    };

    let reviewer = Reviewer {
        backend,
        name: match &model {
            Some(model) => format!("{provider}/{model}"),
            None => provider.to_string(),
        },
    };

    let outcomes = conseqa::harness::review::review(
        &project,
        &reviewer,
        &ReviewRun {
            keys,
            max_agents,
            budget: conseqa::harness::InvocationBudget::default(),
        },
    )
    .await?;

    let unreviewed = outcomes
        .iter()
        .filter(|outcome| {
            !matches!(
                outcome.result,
                ReviewResult::Recorded { .. } | ReviewResult::Unchanged { .. }
            )
        })
        .count();

    if json {
        println!(
            "{}",
            conformance::document::review(outcomes).to_pretty_json()
        );
    } else if outcomes.is_empty() {
        println!("nothing to review: every record's required reviews are confirmed");
    } else {
        for outcome in &outcomes {
            let what = match &outcome.result {
                ReviewResult::Recorded { .. } => "recorded".to_string(),
                ReviewResult::Unchanged { .. } => "unchanged".to_string(),
                ReviewResult::Refused { message, .. } => format!("refused: {message}"),
                ReviewResult::NotReported { message } => format!("not reported: {message}"),
            };

            let by = outcome
                .by
                .as_ref()
                .map(|by| format!(" by {}", by.name))
                .unwrap_or_default();

            println!("{}{by}: {what}", outcome.declaration);
        }
    }

    Ok(if unreviewed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn conform_scan(json: bool) -> Result<ExitCode, String> {
    let project = project_dir()?;
    let scan = conformance::markers::scan(&project).map_err(|error| error.to_string())?;

    if json {
        println!(
            "{}",
            conformance::document::scan(scan.files.len(), &scan.regions, &scan.errors)
                .to_pretty_json()
        );
    } else {
        for region in &scan.regions {
            println!(
                "{}:{}-{}  {}",
                region.path, region.lines[0], region.lines[1], region.key
            );
        }

        for error in &scan.errors {
            println!("scan error  {error}");
        }

        println!(
            "{} file{} read: {} region{}, {} scan error{}",
            scan.files.len(),
            if scan.files.len() == 1 { "" } else { "s" },
            scan.regions.len(),
            if scan.regions.len() == 1 { "" } else { "s" },
            scan.errors.len(),
            if scan.errors.len() == 1 { "" } else { "s" },
        );
    }

    Ok(if scan.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn conform_check(
    scope: &ScopeArgs,
    fail_on: &[String],
    report: Option<PathBuf>,
    json: bool,
) -> Result<ExitCode, String> {
    let gates = fail_on
        .iter()
        .map(|name| name.parse::<conformance::check::Gate>())
        .collect::<Result<Vec<_>, _>>()?;

    let (project, read) = read_project()?;
    let derivation =
        Derivation::of(&project, &read.workspace).map_err(|error| error.to_string())?;
    let scope = conform_scope(&project, &read.workspace, scope)?;

    let check = conformance::check::check(&derivation, scope.as_ref(), &gates);
    let document = conformance::document::check(&check, scope.as_ref());

    if json {
        println!("{}", document.to_pretty_json());
    } else {
        print!("{}", check.render());
    }

    if let Some(path) = report {
        write_file(&path, &format!("{}\n", document.to_pretty_json()))?;
    }

    Ok(if check.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
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
