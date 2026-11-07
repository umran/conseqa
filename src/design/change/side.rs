//! Reading the two sides of a change (§6 of the spec change analysis
//! revision).
//!
//! A side is a git commit or the working tree, and both are read by the
//! store's own loader. A side is therefore refused for exactly what
//! opening the project would refuse: another store format or DSL
//! version, an unexpected entry, a file name that is not an id's
//! canonical encoding, a file that does not load (§6.2). Every refusal
//! names the side it was read for.
//!
//! A git side is read into memory with read-only plumbing. Nothing is
//! checked out, no worktree is made, the index is untouched, no file is
//! written, and the project's writer lock is not taken, so a side reads
//! as well while a design server holds the project open. A commit that
//! holds no project at the project's path is the empty project (§6.4);
//! a ref that names no commit is an error, never an empty side.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::design::store::{self, ProjectDirectory, StoreError};
use crate::design::workspace::WorkspaceState;

/// Where one side is read from (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A commit, named by any expression `git rev-parse` resolves to
    /// one: a branch, a tag, `HEAD~2`, a hash.
    Commit(String),

    /// The project's files as they are on disk, uncommitted edits
    /// included.
    WorkingTree,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(rev) => f.write_str(rev),
            Self::WorkingTree => f.write_str("working tree"),
        }
    }
}

/// Which side of a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideName {
    Base,
    Head,
}

impl fmt::Display for SideName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Head => "head",
        })
    }
}

/// One side, as read.
#[derive(Debug, Clone)]
pub struct Side {
    pub name: SideName,
    pub source: Source,

    /// The commit a git side resolved to.
    pub commit: Option<String>,

    /// What the side holds, or `None` for the empty project.
    pub workspace: Option<WorkspaceState>,
}

impl Side {
    /// Whether the side holds no project at the project's path (§6.4).
    pub fn is_empty(&self) -> bool {
        self.workspace.is_none()
    }
}

/// The git repository a project lives in, and the project's place in
/// it (§6.3): its path relative to the repository's root is the path
/// read at every git side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    root: PathBuf,
    project: PathBuf,
}

impl Repository {
    /// The repository holding the project at `project_dir`, or `None`
    /// when it is in none.
    pub fn of(project_dir: &Path) -> Option<Self> {
        let output = store::git(project_dir, &["rev-parse", "--show-toplevel"], None).ok()?;
        let root = PathBuf::from(String::from_utf8(output).ok()?.trim());

        let root = root.canonicalize().ok()?;
        let project = project_dir.canonicalize().ok()?;

        let project = project.strip_prefix(&root).ok()?.to_path_buf();

        Some(Self { root, project })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The project's directory relative to the repository's root.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The commit a revision expression names, if it names one.
    pub fn resolve(&self, rev: &str) -> Option<String> {
        let commit = format!("{rev}^{{commit}}");

        let output = store::git(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &commit,
            ],
            None,
        )
        .ok()?;

        let hash = String::from_utf8(output).ok()?.trim().to_string();

        (!hash.is_empty()).then_some(hash)
    }

    /// The best common ancestor of two commits.
    pub fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        let output =
            store::git(&self.root, &["merge-base", "--end-of-options", a, b], None).ok()?;
        let hash = String::from_utf8(output).ok()?.trim().to_string();

        (!hash.is_empty()).then_some(hash)
    }
}

/// Why a side could not be read.
#[derive(Debug)]
pub struct SideError {
    pub side: SideName,
    pub source: Source,
    pub reason: SideErrorReason,
}

#[derive(Debug)]
pub enum SideErrorReason {
    /// A git side was named for a project in no git repository (§6.3).
    NotARepository { project: PathBuf },

    /// The expression names no commit (§6.4).
    UnresolvedRef { rev: String },

    /// Two commits have no common ancestor to diff from.
    NoMergeBase { base: String, head: String },

    /// The side does not load, or is of another store format or DSL
    /// version: the store's own refusal, naming the file and both
    /// versions (§6.2, §6.5).
    Store(StoreError),
}

impl fmt::Display for SideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): ", self.side, self.source)?;

        match &self.reason {
            SideErrorReason::NotARepository { project } => write!(
                f,
                "{} is in no git repository, so it has no commit to read",
                project.display()
            ),

            SideErrorReason::UnresolvedRef { rev } => write!(f, "`{rev}` names no commit"),

            SideErrorReason::NoMergeBase { base, head } => {
                write!(f, "`{base}` and `{head}` have no common ancestor")
            }

            SideErrorReason::Store(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SideError {}

/// Reads one side of the project at `project_dir`, the directory that
/// holds its `.conseqa/`.
pub fn read(project_dir: &Path, name: SideName, source: &Source) -> Result<Side, SideError> {
    let error = |reason| SideError {
        side: name,
        source: source.clone(),
        reason,
    };

    match source {
        Source::WorkingTree => {
            let workspace = store::load_source(&ProjectDirectory::of_project(project_dir))
                .map_err(|refusal| error(SideErrorReason::Store(refusal)))?;

            Ok(Side {
                name,
                source: source.clone(),
                commit: None,
                workspace,
            })
        }

        Source::Commit(rev) => {
            let repository = Repository::of(project_dir).ok_or_else(|| {
                error(SideErrorReason::NotARepository {
                    project: project_dir.to_path_buf(),
                })
            })?;

            let commit = repository
                .resolve(rev)
                .ok_or_else(|| error(SideErrorReason::UnresolvedRef { rev: rev.clone() }))?;

            let tree = store::read_commit(repository.root(), &commit, repository.project())
                .map_err(|refusal| error(SideErrorReason::Store(refusal)))?;

            let workspace = match tree {
                Some(tree) => store::load_source(&tree)
                    .map_err(|refusal| error(SideErrorReason::Store(refusal)))?,

                None => None,
            };

            Ok(Side {
                name,
                source: source.clone(),
                commit: Some(commit),
                workspace,
            })
        }
    }
}

/// The merge base of `base` and `head`, which `--merge-base` reads in
/// place of `base` (§15.1). The working tree counts as `HEAD`.
pub fn merge_base(project_dir: &Path, base: &str, head: &Source) -> Result<String, SideError> {
    let error = |reason| SideError {
        side: SideName::Base,
        source: Source::Commit(base.to_string()),
        reason,
    };

    let repository = Repository::of(project_dir).ok_or_else(|| {
        error(SideErrorReason::NotARepository {
            project: project_dir.to_path_buf(),
        })
    })?;

    if repository.resolve(base).is_none() {
        return Err(error(SideErrorReason::UnresolvedRef {
            rev: base.to_string(),
        }));
    }

    let head_source = head.clone();

    let head = match head {
        Source::Commit(rev) => rev.as_str(),
        Source::WorkingTree => "HEAD",
    };

    if repository.resolve(head).is_none() {
        return Err(SideError {
            side: SideName::Head,
            source: head_source,
            reason: SideErrorReason::UnresolvedRef {
                rev: head.to_string(),
            },
        });
    }

    repository.merge_base(base, head).ok_or_else(|| {
        error(SideErrorReason::NoMergeBase {
            base: base.to_string(),
            head: head.to_string(),
        })
    })
}
