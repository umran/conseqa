//! Multiple confluence projects hosted by one daemon.
//!
//! A single global MCP server can serve arbitrary projects, each its
//! own isolated model and history in its own database, opened lazily
//! on first use. State is still per project (§7.2 of the confluence
//! spec) — the manager just keeps several projects side by side so the
//! server need not be told a database up front.

use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use rustc_hash::FxHashMap;

use crate::spec::Revision;

use super::engine::{ConfluenceEngine, EngineError};
use super::workspace::{RunId, RunMetadata, WorkspaceState};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("`{0}` is not a valid project name; use letters, digits, `.`, `_`, or `-`")]
    InvalidName(String),

    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Hosts one lazily-opened confluence engine per project, under a data
/// directory.
pub struct WorkspaceManager {
    data_dir: PathBuf,
    projects: RwLock<FxHashMap<String, ConfluenceEngine>>,
}

impl WorkspaceManager {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            projects: RwLock::new(FxHashMap::default()),
        }
    }

    /// Opens the project's engine, creating a fresh one seeded with
    /// `prompt` if the project does not exist yet. Cached after first
    /// open, so the same engine is returned for the life of the daemon.
    pub fn open_or_create(
        &self,
        project: &str,
        prompt: Option<String>,
    ) -> Result<ConfluenceEngine, ProjectError> {
        validate_name(project)?;

        if let Some(engine) = self.projects.read().get(project) {
            return Ok(engine.clone());
        }

        let mut guard = self.projects.write();

        // Re-check under the write lock.
        if let Some(engine) = guard.get(project) {
            return Ok(engine.clone());
        }

        let path = self.db_path(project);

        let mut run_meta = RunMetadata::new(RunId(project.to_string()));
        run_meta.prompt = prompt;

        let engine = ConfluenceEngine::open(&path, WorkspaceState::empty(run_meta))?;

        guard.insert(project.to_string(), engine.clone());

        Ok(engine)
    }

    /// The engine for an already-open project, if any.
    pub fn get(&self, project: &str) -> Option<ConfluenceEngine> {
        self.projects.read().get(project).cloned()
    }

    /// Every project the manager knows: those already open plus any
    /// on-disk database in the data directory.
    pub fn list(&self) -> Vec<ProjectInfo> {
        let open = self.projects.read();

        let mut names: std::collections::BTreeSet<String> = open.keys().cloned().collect();

        if let Ok(entries) = std::fs::read_dir(&self.data_dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                if path.extension().is_some_and(|ext| ext == "redb")
                    && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
                {
                    names.insert(stem.to_string());
                }
            }
        }

        names
            .into_iter()
            .map(|name| {
                let open_engine = open.get(&name);

                ProjectInfo {
                    revision: open_engine.map(|engine| engine.head_revision()),
                    open: open_engine.is_some(),
                    name,
                }
            })
            .collect()
    }

    /// Every currently-open engine — the set worker capability tokens
    /// are searched against.
    pub fn open_engines(&self) -> Vec<ConfluenceEngine> {
        self.projects.read().values().cloned().collect()
    }

    fn db_path(&self, project: &str) -> PathBuf {
        self.data_dir.join(format!("{project}.redb"))
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

/// One project's summary.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub name: String,
    pub open: bool,
    pub revision: Option<Revision>,
}

/// Accepts project names that are safe as a single filesystem
/// component: non-empty, no path separators, not `.`/`..`, and limited
/// to an unambiguous character set.
fn validate_name(project: &str) -> Result<(), ProjectError> {
    let ok = !project.is_empty()
        && project != "."
        && project != ".."
        && project.len() <= 128
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));

    if ok {
        Ok(())
    } else {
        Err(ProjectError::InvalidName(project.to_string()))
    }
}
