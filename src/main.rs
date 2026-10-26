//! The project-local Conseqa command-line interface.
//!
//! Conseqa project state belongs in `.conseqa/` beside the code it
//! describes. `init` is the only command that works outside a project;
//! every other command discovers the nearest project at or above the
//! current directory.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use conseqa::design::{ProjectStore, RunId, RunMetadata, WorkspaceState};

#[derive(Parser)]
#[command(name = "conseqa", about = "Initialize and inspect a Conseqa project")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create minimal Conseqa state in the current directory.
    Init,

    /// Show the Conseqa project's current local state.
    Status,
}

fn main() -> ExitCode {
    let outcome = match Cli::parse().command {
        Command::Init => init(),
        Command::Status => status(),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|error| format!("cannot read the current directory: {error}"))
}

fn project_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project")
        .to_owned()
}

fn init() -> Result<(), String> {
    let dir = current_dir()?;
    let workspace = WorkspaceState::empty(RunMetadata::new(RunId(project_name(&dir))));
    let store = ProjectStore::create(&dir, &workspace)
        .map_err(|error| format!("cannot initialize Conseqa: {error}"))?;
    let project = store.project_dir().unwrap_or(&dir);

    println!("initialized {}", project.join(".conseqa").display());
    println!("commit .conseqa with the project; .conseqa/local is ignored");

    Ok(())
}

fn status() -> Result<(), String> {
    let dir = current_dir()?;
    let project = ProjectStore::discover(&dir).ok_or_else(|| {
        "not inside a Conseqa project; run `conseqa init` from the project root".to_string()
    })?;
    let read = ProjectStore::read(&project)
        .map_err(|error| format!("cannot read {}: {error}", project.display()))?;

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

    Ok(())
}
