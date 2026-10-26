//! Multiple design projects hosted by one server.
//!
//! A project is a directory holding a `.conseqa/` (see
//! [`super::store`]), so a single server can serve any project on the
//! machine by its path, opened lazily on first use and kept open for the
//! life of the server. State stays in each project; the manager only
//! keeps several engines side by side, keyed by project directory.

use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use rustc_hash::FxHashMap;

use crate::spec::Revision;

use super::engine::{DesignEngine, EngineError};
use super::store::{ProjectStore, StoreError, default_project_name};
use super::workspace::{RunId, RunMetadata, WorkspaceState};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("`{0}` is not a valid project name; give a non-empty, single-line name")]
    InvalidName(String),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Hosts one lazily-opened design engine per project directory.
#[derive(Default)]
pub struct WorkspaceManager {
    projects: RwLock<FxHashMap<PathBuf, DesignEngine>>,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the project at or above `path`. Cached after first open,
    /// so the same engine is returned for the life of the server.
    pub fn open(&self, path: &Path) -> Result<DesignEngine, ProjectError> {
        let project = ProjectStore::discover(path)
            .ok_or_else(|| StoreError::NotAProject(path.to_path_buf()))?;

        if let Some(engine) = self.projects.read().get(&project) {
            return Ok(engine.clone());
        }

        let mut guard = self.projects.write();

        // Re-check under the write lock.
        if let Some(engine) = guard.get(&project) {
            return Ok(engine.clone());
        }

        let engine = DesignEngine::open_project(&project)?;

        guard.insert(project, engine.clone());

        Ok(engine)
    }

    /// Creates a project in `path`, named `name` (the directory's name
    /// when omitted).
    pub fn create(&self, path: &Path, name: Option<String>) -> Result<DesignEngine, ProjectError> {
        let name = name.unwrap_or_else(|| default_project_name(path));

        if name.trim().is_empty() || name.contains('\n') {
            return Err(ProjectError::InvalidName(name));
        }

        let run_meta = RunMetadata::new(RunId(name));

        let mut guard = self.projects.write();

        let engine = DesignEngine::create_project(path, WorkspaceState::empty(run_meta))?;

        if let Some(project) = engine.project_dir() {
            guard.insert(project, engine.clone());
        }

        Ok(engine)
    }

    /// Every project open in this server.
    pub fn list(&self) -> Vec<ProjectInfo> {
        let mut projects: Vec<ProjectInfo> = self
            .projects
            .read()
            .iter()
            .map(|(path, engine)| ProjectInfo::of(path.clone(), engine))
            .collect();

        projects.sort_by(|a, b| a.path.cmp(&b.path));

        projects
    }

    /// Every currently-open engine — the set worker capability tokens
    /// are searched against.
    pub fn open_engines(&self) -> Vec<DesignEngine> {
        self.projects.read().values().cloned().collect()
    }
}

/// One project's summary.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub name: String,
    pub path: PathBuf,
    pub revision: Revision,
}

impl ProjectInfo {
    pub fn of(path: PathBuf, engine: &DesignEngine) -> Self {
        let head = engine.head_snapshot();

        Self {
            name: head.workspace.run_meta.run.0.clone(),
            path,
            revision: head.revision,
        }
    }
}
