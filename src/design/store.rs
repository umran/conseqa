//! Project state, kept in the project it describes.
//!
//! A Conseqa project is a `.conseqa/` directory beside the code it
//! describes, committed with that code: the architecture is versioned,
//! reviewed, branched, and merged like any other source.
//!
//! ```text
//! .conseqa/
//!   project.yaml                    format, dsl, name, run policy
//!   spec/<kind>/<id>.yaml           one file per service, schema, data
//!                                   model, topic, state machine, operation
//!   spec/runtime/<kind>/<id>.yaml   one file per runtime declaration
//!   intent/obligations.yaml         the prompt's correctness obligations
//!   intent/proposals.yaml           requirement proposals, with provenance
//!   .gitignore                      keeps local/ out of version control
//!   local/                          this machine's engine state
//! ```
//!
//! The committed files are the source of truth, and git is the history:
//! the store keeps no past revisions, and nothing committed names an
//! engine revision, so two branches that each change the model merge
//! like any other text. The engine adopts what changes the files from
//! outside — a hand edit, a checkout, a pull — as a revision of its own
//! ([`ProjectStore::disk_change`]).
//!
//! `local/` holds what only this machine's engine needs: the exclusive
//! lock, the head revision number, the commit and task logs, open
//! dependency requests, design-run artifacts, and a write-ahead record
//! of an in-flight commit, which the next open completes if a crash
//! interrupted it.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::spec::{DSL_VERSION, DslVersion, Id, Revision, RuntimeModel};

use super::commit::CommitRecord;
use super::symbol::SymbolKey;
use super::task::{DependencyRequest, TaskId, TaskSpec, TaskState};
use super::workspace::{RunId, RunMetadata, RunPolicy, WorkspaceState};

/// The directory, at a project's root, that holds its state.
pub const PROJECT_DIR: &str = ".conseqa";

/// The project store's format.
///
/// Formats 1 to 8 were a central redb database per project, holding
/// every revision of the workspace as one serialized blob; their bump
/// history is in git, in the persistence module this store replaced.
/// Format 9 moved the state into the project, as one YAML file per
/// entity. Format 10 dropped the prompt from `project.yaml`: what a
/// design run is asked to build is that run's input, not project state.
/// A `dsl` bump forces a format bump — the files hold spec types —
/// never conversely.
pub const FORMAT: u64 = 10;

const PROJECT_FILE: &str = "project.yaml";
const OBLIGATIONS: &str = "intent/obligations.yaml";
const PROPOSALS: &str = "intent/proposals.yaml";

const SPEC: &str = "spec";
const RUNTIME: &str = "spec/runtime";
const INTENT: &str = "intent";

const SERVICES: &str = "spec/services";
const SCHEMAS: &str = "spec/schemas";
const DATA_MODELS: &str = "spec/data_models";
const TOPICS: &str = "spec/topics";
const STATE_MACHINES: &str = "spec/state_machines";
const OPERATIONS: &str = "spec/operations";

const RUNTIME_TOPICS: &str = "spec/runtime/topics";
const RUNTIME_SUBSCRIPTIONS: &str = "spec/runtime/subscriptions";
const RUNTIME_OUTBOXES: &str = "spec/runtime/outboxes";
const RUNTIME_POOLS: &str = "spec/runtime/execution_pools";
const RUNTIME_ROUTERS: &str = "spec/runtime/routers";
const RUNTIME_STORAGE: &str = "spec/runtime/storage_layouts";

/// What `spec/` and `spec/runtime/` may hold. Anything else is refused
/// by name rather than ignored: a misspelled directory would otherwise
/// make every entity in it silently vanish from the model.
const SPEC_ENTRIES: [&str; 7] = [
    "services",
    "schemas",
    "data_models",
    "topics",
    "state_machines",
    "operations",
    "runtime",
];

const RUNTIME_ENTRIES: [&str; 6] = [
    "topics",
    "subscriptions",
    "outboxes",
    "execution_pools",
    "routers",
    "storage_layouts",
];

const INTENT_ENTRIES: [&str; 2] = ["obligations.yaml", "proposals.yaml"];

const LOCAL: &str = "local";
const LOCK: &str = "local/lock";
const STATE: &str = "local/state.json";
const PENDING: &str = "local/pending.json";
const COMMIT_LOG: &str = "local/commits.jsonl";
const TASK_LOG: &str = "local/tasks.jsonl";
const DEPENDENCY_REQUESTS: &str = "local/dependency_requests.json";

const GITIGNORE: &str = "\
# This machine's conseqa engine state: the lock, logs, and design runs.
/local/
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: {error}", .path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },

    #[error("{} does not load: {message}", .path.display())]
    Parse { path: PathBuf, message: String },

    #[error("cannot serialize {what}: {message}")]
    Serialize { what: String, message: String },

    #[error(
        "{} is project store format {found}, but this build reads format {FORMAT}; the \
         stored layout changed and is not migrated automatically",
        .path.display()
    )]
    FormatMismatch { path: PathBuf, found: u64 },

    #[error(
        "{} is authored in dsl {found}, but this build reads dsl {DSL_VERSION}; the \
         project must be re-authored",
        .path.display()
    )]
    DslMismatch { path: PathBuf, found: DslVersion },

    #[error(
        "no conseqa project at {} or any directory above it (a project is a \
         `.conseqa/project.yaml`)",
        .0.display()
    )]
    NotAProject(PathBuf),

    #[error(
        "{} is the home directory, whose `.conseqa` holds this machine's conseqa \
         configuration; create the project in a repository instead",
        .0.display()
    )]
    MachineHome(PathBuf),

    #[error("{} already holds a conseqa project", .0.display())]
    AlreadyAProject(PathBuf),

    #[error(
        "{} is inside the conseqa project at {}; open that one instead",
        .path.display(),
        .project.display()
    )]
    InsideProject { path: PathBuf, project: PathBuf },

    #[error(
        "the project at {} is open in another conseqa process{}",
        .path.display(),
        .holder.as_ref().map(|pid| format!(" (pid {pid})")).unwrap_or_default()
    )]
    Locked {
        path: PathBuf,
        holder: Option<String>,
    },

    #[error(
        "{}: unexpected entry; it holds only {expected}",
        .path.display()
    )]
    Unexpected { path: PathBuf, expected: String },

    #[error(
        "`{first}` and `{second}` differ only in letter case, so their files would \
         collide on a case-insensitive filesystem; rename one"
    )]
    CaseCollision { first: String, second: String },

    #[error("an empty id cannot be stored")]
    EmptyId,
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |error| StoreError::Io {
        path: path.to_path_buf(),
        error,
    }
}

/// A task as the local log records it: its immutable spec and latest
/// state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub spec: TaskSpec,
    pub state: TaskState,
}

/// One line of the local commit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEntry {
    /// A commit through the engine's gate.
    Commit(CommitRecord),

    /// The files changed from outside the engine, and it adopted them.
    External(ExternalChange),
}

/// A revision the engine adopted from the files rather than committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalChange {
    pub revision: Revision,
    pub parent: Revision,
    pub changed_symbols: Vec<SymbolKey>,
    pub timestamp_unix_ms: u64,
}

/// What the files on disk hold when they differ from the engine's
/// head: a workspace to adopt as the next revision.
pub struct DiskChange {
    pub workspace: WorkspaceState,
    files: FileSet,
    fingerprint: blake3::Hash,
}

/// A project's state read without opening it for writing: no lock is
/// taken, so a running engine elsewhere is undisturbed.
pub struct ProjectRead {
    pub workspace: WorkspaceState,
    pub log: Vec<LogEntry>,
    pub tasks: Vec<TaskRecord>,
}

/// The committed files, by path relative to `.conseqa/`.
type FileSet = BTreeMap<String, String>;

/// Where a project's state is kept: the project's own directory, or
/// memory for tests and ephemeral runs.
pub struct ProjectStore {
    directory: Option<DirectoryStore>,
}

struct DirectoryStore {
    project: PathBuf,
    root: PathBuf,

    /// Held for the store's lifetime: one engine writes a project.
    _lock: File,

    disk: Mutex<Disk>,
}

/// What the engine believes the committed files hold.
struct Disk {
    /// The canonical rendering of the head.
    files: FileSet,

    /// The files' metadata as last written or read, so a change made
    /// from outside is noticed without reading every file.
    fingerprint: blake3::Hash,
}

/// `local/state.json`: the head revision and the digest of the files it
/// names, so a reopened engine keeps counting from where it left off
/// unless the files changed while it was closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LocalState {
    format: u64,
    revision: Revision,
    digest: String,
}

/// `local/pending.json`: a commit's file writes, recorded before any is
/// made, so a crash between them is completed rather than left half
/// applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    state: LocalState,
    writes: FileSet,
    deletes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    format: u64,
    dsl: DslVersion,
    name: String,

    #[serde(default)]
    policy: RunPolicy,
}

/// The version fields of `project.yaml`, read before the rest so a
/// project of another format is refused by name rather than as a shape
/// error.
#[derive(Deserialize)]
struct ProjectProbe {
    format: Option<u64>,
    dsl: Option<DslVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskLogEntry {
    Recorded {
        spec: TaskSpec,
        state: TaskState,
        timestamp_unix_ms: u64,
    },
    State {
        task: TaskId,
        state: TaskState,
        timestamp_unix_ms: u64,
    },
}

impl ProjectStore {
    /// A store that keeps nothing.
    pub fn in_memory() -> Self {
        Self { directory: None }
    }

    /// The project directory holding `start`, or `start` itself: the
    /// nearest directory at or above it with a `.conseqa/project.yaml`.
    pub fn discover(start: &Path) -> Option<PathBuf> {
        discover_below(start, machine_home().as_deref())
    }

    /// Creates a project in `project_dir`, holding `workspace`.
    pub fn create(project_dir: &Path, workspace: &WorkspaceState) -> Result<Self, StoreError> {
        Self::create_below(project_dir, workspace, machine_home().as_deref())
    }

    fn create_below(
        project_dir: &Path,
        workspace: &WorkspaceState,
        home: Option<&Path>,
    ) -> Result<Self, StoreError> {
        let project_dir = project_dir.canonicalize().map_err(io_error(project_dir))?;

        if Some(project_dir.as_path()) == home {
            return Err(StoreError::MachineHome(project_dir));
        }

        let root = project_dir.join(PROJECT_DIR);

        if root.join(PROJECT_FILE).exists() {
            return Err(StoreError::AlreadyAProject(project_dir));
        }

        if let Some(enclosing) = project_dir
            .parent()
            .and_then(|parent| discover_below(parent, home))
        {
            return Err(StoreError::InsideProject {
                path: project_dir,
                project: enclosing,
            });
        }

        let local = root.join(LOCAL);

        fs::create_dir_all(&local).map_err(io_error(&local))?;

        let gitignore = root.join(".gitignore");

        if !gitignore.exists() {
            write_atomic(&gitignore, GITIGNORE.as_bytes())?;
        }

        let lock = acquire_lock(&root)?;

        let files = render(workspace)?;

        let state = LocalState {
            format: FORMAT,
            revision: workspace.revision,
            digest: digest(&files),
        };

        apply(&root, &files, &[])?;
        write_json(&root.join(STATE), &state)?;

        let fingerprint = scan(&root)?;

        Ok(Self {
            directory: Some(DirectoryStore {
                project: project_dir,
                root,
                _lock: lock,
                disk: Mutex::new(Disk { files, fingerprint }),
            }),
        })
    }

    /// Opens the project in `project_dir` for writing, returning its
    /// head. Completes a commit a crash interrupted.
    pub fn open(project_dir: &Path) -> Result<(Self, WorkspaceState), StoreError> {
        let project_dir = project_dir.canonicalize().map_err(io_error(project_dir))?;

        let root = project_dir.join(PROJECT_DIR);

        if !root.join(PROJECT_FILE).is_file() {
            return Err(StoreError::NotAProject(project_dir));
        }

        let local = root.join(LOCAL);

        fs::create_dir_all(&local).map_err(io_error(&local))?;

        let lock = acquire_lock(&root)?;

        complete_pending(&root)?;

        // Scanned before loading: an edit landing in between is then
        // seen as a change at the next look, never missed.
        let fingerprint = scan(&root)?;

        let mut workspace = load_workspace(&root)?;
        let files = render(&workspace)?;

        let state = LocalState {
            format: FORMAT,
            revision: continued_revision(&root, &files),
            digest: digest(&files),
        };

        if read_local_state(&root).as_ref() != Some(&state) {
            write_json(&root.join(STATE), &state)?;
        }

        workspace.revision = state.revision;

        let store = Self {
            directory: Some(DirectoryStore {
                project: project_dir,
                root,
                _lock: lock,
                disk: Mutex::new(Disk { files, fingerprint }),
            }),
        };

        Ok((store, workspace))
    }

    /// Reads the project in `project_dir` without opening it for
    /// writing.
    pub fn read(project_dir: &Path) -> Result<ProjectRead, StoreError> {
        let root = project_dir.join(PROJECT_DIR);

        if !root.join(PROJECT_FILE).is_file() {
            return Err(StoreError::NotAProject(project_dir.to_path_buf()));
        }

        let mut workspace = load_workspace(&root)?;
        let files = render(&workspace)?;

        workspace.revision = continued_revision(&root, &files);

        Ok(ProjectRead {
            workspace,
            log: read_lines(&root.join(COMMIT_LOG)),
            tasks: fold_tasks(read_lines(&root.join(TASK_LOG))),
        })
    }

    /// The directory the project lives in, for a project on disk.
    pub fn project_dir(&self) -> Option<&Path> {
        self.directory
            .as_ref()
            .map(|directory| directory.project.as_path())
    }

    /// The project's machine-local, uncommitted directory.
    pub fn local_dir(&self) -> Option<PathBuf> {
        self.directory
            .as_ref()
            .map(|directory| directory.root.join(LOCAL))
    }

    /// Writes an accepted commit's workspace to the files, and logs the
    /// commit. On failure the files are restored and the old head stays
    /// authoritative.
    pub fn persist_commit(
        &self,
        workspace: &WorkspaceState,
        record: &CommitRecord,
    ) -> Result<(), StoreError> {
        let Some(directory) = &self.directory else {
            return Ok(());
        };

        let files = render(workspace)?;

        let mut disk = directory.disk.lock();

        directory.write(&mut disk, files, workspace.revision)?;

        append_line(
            &directory.root.join(COMMIT_LOG),
            &LogEntry::Commit(record.clone()),
        );

        Ok(())
    }

    /// The files' content when something outside the engine changed it
    /// since the engine last wrote or read them; `None` when they still
    /// hold the head, reformatting included. An error when they no
    /// longer load: the engine must not write over an edit in progress.
    pub fn disk_change(&self) -> Result<Option<DiskChange>, StoreError> {
        let Some(directory) = &self.directory else {
            return Ok(None);
        };

        let mut disk = directory.disk.lock();

        let fingerprint = scan(&directory.root)?;

        if fingerprint == disk.fingerprint {
            return Ok(None);
        }

        let workspace = load_workspace(&directory.root)?;
        let files = render(&workspace)?;

        if files == disk.files {
            disk.fingerprint = fingerprint;

            return Ok(None);
        }

        Ok(Some(DiskChange {
            workspace,
            files,
            fingerprint,
        }))
    }

    /// Records that the engine adopted `change` as `entry.revision`.
    pub fn record_disk_change(
        &self,
        change: DiskChange,
        entry: ExternalChange,
    ) -> Result<(), StoreError> {
        let Some(directory) = &self.directory else {
            return Ok(());
        };

        let mut disk = directory.disk.lock();

        write_json(
            &directory.root.join(STATE),
            &LocalState {
                format: FORMAT,
                revision: entry.revision,
                digest: digest(&change.files),
            },
        )?;

        disk.files = change.files;
        disk.fingerprint = change.fingerprint;

        append_line(&directory.root.join(COMMIT_LOG), &LogEntry::External(entry));

        Ok(())
    }

    pub fn record_task(
        &self,
        spec: &TaskSpec,
        state: TaskState,
        timestamp_unix_ms: u64,
    ) -> Result<(), StoreError> {
        if let Some(directory) = &self.directory {
            append_line(
                &directory.root.join(TASK_LOG),
                &TaskLogEntry::Recorded {
                    spec: spec.clone(),
                    state,
                    timestamp_unix_ms,
                },
            );
        }

        Ok(())
    }

    pub fn update_task_state(
        &self,
        task: TaskId,
        state: TaskState,
        timestamp_unix_ms: u64,
    ) -> Result<(), StoreError> {
        if let Some(directory) = &self.directory {
            append_line(
                &directory.root.join(TASK_LOG),
                &TaskLogEntry::State {
                    task,
                    state,
                    timestamp_unix_ms,
                },
            );
        }

        Ok(())
    }

    /// Every task the log records, at its latest state.
    pub fn load_tasks(&self) -> Vec<TaskRecord> {
        match &self.directory {
            Some(directory) => fold_tasks(read_lines(&directory.root.join(TASK_LOG))),
            None => Vec::new(),
        }
    }

    pub fn save_dependency_requests(
        &self,
        requests: &[DependencyRequest],
    ) -> Result<(), StoreError> {
        match &self.directory {
            Some(directory) => write_json(&directory.root.join(DEPENDENCY_REQUESTS), &requests),
            None => Ok(()),
        }
    }

    pub fn load_dependency_requests(&self) -> Result<Vec<DependencyRequest>, StoreError> {
        let Some(directory) = &self.directory else {
            return Ok(Vec::new());
        };

        let path = directory.root.join(DEPENDENCY_REQUESTS);

        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| StoreError::Parse {
                path,
                message: error.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(StoreError::Io { path, error }),
        }
    }
}

impl DirectoryStore {
    /// Makes the files hold `files`: journal first, then each file
    /// atomically, then the head pointer.
    fn write(&self, disk: &mut Disk, files: FileSet, revision: Revision) -> Result<(), StoreError> {
        let writes: FileSet = files
            .iter()
            .filter(|(path, text)| disk.files.get(*path) != Some(*text))
            .map(|(path, text)| (path.clone(), text.clone()))
            .collect();

        let deletes: Vec<String> = disk
            .files
            .keys()
            .filter(|path| !files.contains_key(*path))
            .cloned()
            .collect();

        let state = LocalState {
            format: FORMAT,
            revision,
            digest: digest(&files),
        };

        if writes.is_empty() && deletes.is_empty() {
            write_json(&self.root.join(STATE), &state)?;
        } else {
            let pending_path = self.root.join(PENDING);

            write_json(
                &pending_path,
                &Pending {
                    state: state.clone(),
                    writes: writes.clone(),
                    deletes: deletes.clone(),
                },
            )?;

            if let Err(error) = apply(&self.root, &writes, &deletes) {
                self.roll_back(disk, &writes, &deletes);

                return Err(error);
            }

            write_json(&self.root.join(STATE), &state)?;

            fs::remove_file(&pending_path).map_err(io_error(&pending_path))?;
        }

        disk.files = files;
        disk.fingerprint = scan(&self.root)?;

        Ok(())
    }

    /// Best effort: puts back what a failed write changed. Should this
    /// fail too, the journal stays, and the next open completes the
    /// commit instead — consistent either way.
    fn roll_back(&self, disk: &Disk, writes: &FileSet, deletes: &[String]) {
        let restore: FileSet = writes
            .keys()
            .chain(deletes)
            .filter_map(|path| Some((path.clone(), disk.files.get(path)?.clone())))
            .collect();

        let remove: Vec<String> = writes
            .keys()
            .filter(|path| !disk.files.contains_key(*path))
            .cloned()
            .collect();

        if apply(&self.root, &restore, &remove).is_ok() {
            let _ = fs::remove_file(self.root.join(PENDING));
        }
    }
}

/// The workspace a new project in `dir` starts from: named after the
/// directory, and holding the model at `model` when one is adopted,
/// else empty.
pub fn new_project_workspace(
    dir: &Path,
    model: Option<&Path>,
) -> Result<WorkspaceState, StoreError> {
    let run_meta = RunMetadata::new(RunId(default_project_name(dir)));

    let Some(path) = model else {
        return Ok(WorkspaceState::empty(run_meta));
    };

    let source = fs::read_to_string(path).map_err(io_error(path))?;

    let model = crate::parser::yaml::parse(&source).map_err(|error| StoreError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    Ok(WorkspaceState::from_model(&model, run_meta))
}

/// The name a project gets when none is given: its directory's.
pub fn default_project_name(dir: &Path) -> String {
    dir.canonicalize()
        .ok()
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

/// The home directory, whose `.conseqa` is this machine's conseqa
/// configuration — the server's API key, `stdio.args`, decider keys —
/// and so never a project.
fn machine_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .and_then(|home| PathBuf::from(home).canonicalize().ok())
}

/// The nearest project at or above `start`, looking no higher than
/// below `home`: a project there would claim every directory of the
/// user's as its own.
fn discover_below(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;

    start
        .ancestors()
        .take_while(|dir| Some(*dir) != home)
        .find(|dir| dir.join(PROJECT_DIR).join(PROJECT_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The revision a reopened project continues from: the last one this
/// machine recorded when the files still hold it, the next one when
/// they changed while no engine had them open, and 0 for a project this
/// machine has never opened.
fn continued_revision(root: &Path, files: &FileSet) -> Revision {
    match read_local_state(root) {
        Some(state) if state.digest == digest(files) => state.revision,
        Some(state) => Revision(state.revision.0 + 1),
        None => Revision(0),
    }
}

fn read_local_state(root: &Path) -> Option<LocalState> {
    let text = fs::read_to_string(root.join(STATE)).ok()?;

    serde_json::from_str::<LocalState>(&text)
        .ok()
        .filter(|state| state.format == FORMAT)
}

/// Takes the project's exclusive lock, recording this process as its
/// holder.
fn acquire_lock(root: &Path) -> Result<File, StoreError> {
    let path = root.join(LOCK);

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io_error(&path))?;

    match file.try_lock() {
        Ok(()) => {}

        Err(std::fs::TryLockError::WouldBlock) => {
            let holder = fs::read_to_string(&path)
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());

            return Err(StoreError::Locked {
                path: root.parent().unwrap_or(root).to_path_buf(),
                holder,
            });
        }

        Err(std::fs::TryLockError::Error(error)) => {
            return Err(StoreError::Io { path, error });
        }
    }

    file.set_len(0).map_err(io_error(&path))?;
    write!(file, "{}", std::process::id()).map_err(io_error(&path))?;

    Ok(file)
}

/// Finishes a commit a crash interrupted, and clears temporary files a
/// crash left behind.
fn complete_pending(root: &Path) -> Result<(), StoreError> {
    let path = root.join(PENDING);

    match fs::read_to_string(&path) {
        Ok(text) => {
            let pending: Pending =
                serde_json::from_str(&text).map_err(|error| StoreError::Parse {
                    path: path.clone(),
                    message: error.to_string(),
                })?;

            apply(root, &pending.writes, &pending.deletes)?;
            write_json(&root.join(STATE), &pending.state)?;

            fs::remove_file(&path).map_err(io_error(&path))?;
        }

        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}

        Err(error) => return Err(StoreError::Io { path, error }),
    }

    remove_temporaries(&root.join(SPEC));
    remove_temporaries(&root.join(INTENT));

    Ok(())
}

fn remove_temporaries(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if path.is_dir() {
            remove_temporaries(&path);
        } else if name.starts_with('.') && name.contains(".tmp-") {
            let _ = fs::remove_file(&path);
        }
    }
}

// ---------------------------------------------------------------------
// Rendering and loading
// ---------------------------------------------------------------------

/// The committed files a workspace is, in canonical form.
fn render(workspace: &WorkspaceState) -> Result<FileSet, StoreError> {
    let mut files = FileSet::new();

    let project = ProjectFile {
        format: FORMAT,
        dsl: DSL_VERSION,
        name: workspace.run_meta.run.0.clone(),
        policy: workspace.run_meta.policy,
    };

    files.insert(PROJECT_FILE.to_string(), to_yaml(&project, PROJECT_FILE)?);

    render_entities(&mut files, SERVICES, &workspace.services)?;
    render_entities(&mut files, SCHEMAS, &workspace.schemas)?;
    render_entities(&mut files, DATA_MODELS, &workspace.data_models)?;
    render_entities(&mut files, TOPICS, &workspace.topics)?;
    render_entities(&mut files, STATE_MACHINES, &workspace.state_machines)?;
    render_entities(&mut files, OPERATIONS, &workspace.operations)?;

    let runtime = &workspace.runtime;

    render_entities(&mut files, RUNTIME_TOPICS, &runtime.topics)?;
    render_entities(&mut files, RUNTIME_SUBSCRIPTIONS, &runtime.subscriptions)?;
    render_entities(&mut files, RUNTIME_OUTBOXES, &runtime.outboxes)?;
    render_entities(&mut files, RUNTIME_POOLS, &runtime.execution_pools)?;
    render_entities(&mut files, RUNTIME_ROUTERS, &runtime.routers)?;
    render_entities(&mut files, RUNTIME_STORAGE, &runtime.storage_layouts)?;

    if !workspace.prompt_obligations.is_empty() {
        files.insert(
            OBLIGATIONS.to_string(),
            to_yaml(&workspace.prompt_obligations, OBLIGATIONS)?,
        );
    }

    if !workspace.requirement_proposals.is_empty() {
        files.insert(
            PROPOSALS.to_string(),
            to_yaml(&workspace.requirement_proposals, PROPOSALS)?,
        );
    }

    Ok(files)
}

fn render_entities<T: Serialize>(
    files: &mut FileSet,
    dir: &str,
    entities: &BTreeMap<Id, T>,
) -> Result<(), StoreError> {
    let mut folded: BTreeMap<String, &Id> = BTreeMap::new();

    for (id, entity) in entities {
        let name = file_name(id)?;

        if let Some(first) = folded.insert(name.to_ascii_lowercase(), id) {
            return Err(StoreError::CaseCollision {
                first: first.0.clone(),
                second: id.0.clone(),
            });
        }

        let path = format!("{dir}/{name}");
        let text = to_yaml(entity, &path)?;

        files.insert(path, text);
    }

    Ok(())
}

/// The workspace the files hold, at revision 0.
fn load_workspace(root: &Path) -> Result<WorkspaceState, StoreError> {
    let project = read_project_file(root)?;

    check_entries(root, SPEC, &SPEC_ENTRIES, true)?;
    check_entries(root, RUNTIME, &RUNTIME_ENTRIES, true)?;
    check_entries(root, INTENT, &INTENT_ENTRIES, false)?;

    Ok(WorkspaceState {
        revision: Revision(0),
        services: load_entities(root, SERVICES)?,
        schemas: load_entities(root, SCHEMAS)?,
        data_models: load_entities(root, DATA_MODELS)?,
        topics: load_entities(root, TOPICS)?,
        state_machines: load_entities(root, STATE_MACHINES)?,
        operations: load_entities(root, OPERATIONS)?,
        runtime: RuntimeModel {
            topics: load_entities(root, RUNTIME_TOPICS)?,
            subscriptions: load_entities(root, RUNTIME_SUBSCRIPTIONS)?,
            outboxes: load_entities(root, RUNTIME_OUTBOXES)?,
            execution_pools: load_entities(root, RUNTIME_POOLS)?,
            routers: load_entities(root, RUNTIME_ROUTERS)?,
            storage_layouts: load_entities(root, RUNTIME_STORAGE)?,
        },
        prompt_obligations: load_optional(root, OBLIGATIONS)?.unwrap_or_default(),
        requirement_proposals: load_optional(root, PROPOSALS)?.unwrap_or_default(),
        run_meta: RunMetadata {
            run: RunId(project.name),
            policy: project.policy,
        },
    })
}

fn read_project_file(root: &Path) -> Result<ProjectFile, StoreError> {
    let path = root.join(PROJECT_FILE);
    let text = fs::read_to_string(&path).map_err(io_error(&path))?;

    let probe: ProjectProbe = from_yaml(&text, &path)?;

    match probe.format {
        Some(FORMAT) => {}
        Some(found) => return Err(StoreError::FormatMismatch { path, found }),
        None => {
            return Err(StoreError::Parse {
                path,
                message: "missing `format`".to_string(),
            });
        }
    }

    if let Some(found) = probe.dsl
        && found != DSL_VERSION
    {
        return Err(StoreError::DslMismatch { path, found });
    }

    from_yaml(&text, &path)
}

/// Refuses anything in `dir` but `allowed`: directories when `dirs`,
/// else files. Dotfiles are an editor's or the OS's, and pass.
fn check_entries(root: &Path, dir: &str, allowed: &[&str], dirs: bool) -> Result<(), StoreError> {
    let path = root.join(dir);

    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StoreError::Io { path, error }),
    };

    for entry in entries {
        let entry = entry.map_err(io_error(&path))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with('.') {
            continue;
        }

        let is_dir = entry.path().is_dir();

        if is_dir != dirs || !allowed.contains(&name.as_ref()) {
            return Err(StoreError::Unexpected {
                path: entry.path(),
                expected: allowed
                    .iter()
                    .map(|name| {
                        if dirs {
                            format!("{name}/")
                        } else {
                            name.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }

    Ok(())
}

fn load_entities<T: DeserializeOwned>(
    root: &Path,
    dir: &str,
) -> Result<BTreeMap<Id, T>, StoreError> {
    let path = root.join(dir);
    let mut entities = BTreeMap::new();

    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entities),
        Err(error) => return Err(StoreError::Io { path, error }),
    };

    for entry in entries {
        let entry = entry.map_err(io_error(&path))?;
        let file = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with('.') {
            continue;
        }

        let unexpected = || StoreError::Unexpected {
            path: file.clone(),
            expected: "one `<id>.yaml` file per entity".to_string(),
        };

        if file.is_dir() {
            return Err(unexpected());
        }

        let id = name
            .strip_suffix(".yaml")
            .and_then(decode_file_stem)
            .ok_or_else(unexpected)?;

        let text = fs::read_to_string(&file).map_err(io_error(&file))?;

        entities.insert(Id(id), from_yaml(&text, &file)?);
    }

    Ok(entities)
}

fn load_optional<T: DeserializeOwned>(root: &Path, file: &str) -> Result<Option<T>, StoreError> {
    let path = root.join(file);

    match fs::read_to_string(&path) {
        Ok(text) => from_yaml(&text, &path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::Io { path, error }),
    }
}

/// YAML with every enum as a one-key map rather than a YAML tag: the
/// tag form does not survive the buffering that internally tagged
/// enums (sketch steps, among others) deserialize through.
fn to_yaml<T: Serialize>(value: &T, what: &str) -> Result<String, StoreError> {
    let mut buffer = Vec::new();
    let mut serializer = serde_yaml::Serializer::new(&mut buffer);

    serde_yaml::with::singleton_map_recursive::serialize(value, &mut serializer).map_err(
        |error| StoreError::Serialize {
            what: what.to_string(),
            message: error.to_string(),
        },
    )?;

    String::from_utf8(buffer).map_err(|error| StoreError::Serialize {
        what: what.to_string(),
        message: error.to_string(),
    })
}

fn from_yaml<T: DeserializeOwned>(text: &str, path: &Path) -> Result<T, StoreError> {
    serde_yaml::with::singleton_map_recursive::deserialize(serde_yaml::Deserializer::from_str(text))
        .map_err(|error| StoreError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })
}

// ---------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------

/// An id's file name: its escaped spelling (`Id::encoded`), so any id
/// is a file name and the common ones read as themselves.
fn file_name(id: &Id) -> Result<String, StoreError> {
    if id.0.is_empty() {
        return Err(StoreError::EmptyId);
    }

    Ok(format!("{}.yaml", id.encoded()))
}

/// The id a file stem names, if the stem is an id's canonical encoding.
/// A non-canonical spelling is refused, so two files can never name
/// the same id.
fn decode_file_stem(stem: &str) -> Option<String> {
    Id::decode(stem).map(|id| id.0)
}

// ---------------------------------------------------------------------
// The filesystem
// ---------------------------------------------------------------------

/// Writes `writes` and removes `deletes`, each file atomically.
fn apply(root: &Path, writes: &FileSet, deletes: &[String]) -> Result<(), StoreError> {
    for (path, text) in writes {
        write_atomic(&root.join(path), text.as_bytes())?;
    }

    for path in deletes {
        let file = root.join(path);

        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::Io { path: file, error }),
        }

        prune_empty_dirs(root, &file);
    }

    Ok(())
}

/// Removes the directories a delete emptied, up to `.conseqa/`.
fn prune_empty_dirs(root: &Path, file: &Path) {
    let mut dir = file.parent();

    while let Some(current) = dir {
        if current == root || !current.starts_with(root) || fs::remove_dir(current).is_err() {
            break;
        }

        dir = current.parent();
    }
}

/// Writes through a temporary sibling and a rename, so a reader sees
/// the old file or the new one, never part of either.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));

    fs::create_dir_all(dir).map_err(io_error(dir))?;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let temporary = dir.join(format!(".{name}.tmp-{}", std::process::id()));

    fs::write(&temporary, contents).map_err(io_error(&temporary))?;
    fs::rename(&temporary, path).map_err(io_error(path))?;

    Ok(())
}

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), StoreError> {
    let mut text = serde_json::to_string_pretty(value).map_err(|error| StoreError::Serialize {
        what: path.display().to_string(),
        message: error.to_string(),
    })?;

    text.push('\n');

    write_atomic(path, text.as_bytes())
}

/// Appends one JSON line to a local log. Best effort: the logs are an
/// audit trail, and a commit already on disk must not fail over one.
fn append_line<T: Serialize>(path: &Path, value: &T) {
    let Ok(line) = serde_json::to_string(value) else {
        return;
    };

    let appended = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{line}"));

    if let Err(error) = appended {
        tracing::warn!("cannot append to {}: {error}", path.display());
    }
}

/// A local log's entries. A line that does not parse — the torn tail of
/// a crash — is skipped.
fn read_lines<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn fold_tasks(entries: Vec<TaskLogEntry>) -> Vec<TaskRecord> {
    let mut tasks: BTreeMap<TaskId, TaskRecord> = BTreeMap::new();

    for entry in entries {
        match entry {
            TaskLogEntry::Recorded { spec, state, .. } => {
                tasks.insert(spec.id, TaskRecord { spec, state });
            }

            TaskLogEntry::State { task, state, .. } => {
                if let Some(record) = tasks.get_mut(&task) {
                    record.state = state;
                }
            }
        }
    }

    tasks.into_values().collect()
}

/// The digest of a set of files: what the head revision number names.
fn digest(files: &FileSet) -> String {
    let mut hasher = blake3::Hasher::new();

    for (path, text) in files {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(text.len() as u64).to_le_bytes());
        hasher.update(text.as_bytes());
    }

    hasher.finalize().to_hex().to_string()
}

/// The committed files' metadata — path, size, modification time, and
/// on Unix the inode — hashed: cheap enough to take on every request,
/// and moved by any edit, checkout, or atomic replace.
fn scan(root: &Path) -> Result<blake3::Hash, StoreError> {
    let mut hasher = blake3::Hasher::new();

    scan_file(&mut hasher, root, &root.join(PROJECT_FILE))?;
    scan_dir(&mut hasher, root, &root.join(SPEC))?;
    scan_dir(&mut hasher, root, &root.join(INTENT))?;

    Ok(hasher.finalize())
}

fn scan_dir(hasher: &mut blake3::Hasher, root: &Path, dir: &Path) -> Result<(), StoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(StoreError::Io {
                path: dir.to_path_buf(),
                error,
            });
        }
    };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect();

    paths.sort();

    for path in paths {
        if path.is_dir() {
            scan_dir(hasher, root, &path)?;
        } else {
            scan_file(hasher, root, &path)?;
        }
    }

    Ok(())
}

fn scan_file(hasher: &mut blake3::Hasher, root: &Path, path: &Path) -> Result<(), StoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                error,
            });
        }
    };

    let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();

    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();

    hasher.update(relative.as_bytes());
    hasher.update(&[0]);
    hasher.update(&metadata.len().to_le_bytes());
    hasher.update(&modified.to_le_bytes());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        hasher.update(&metadata.ino().to_le_bytes());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("conseqa-store-{}", uuid::Uuid::new_v4()));

        fs::create_dir_all(&dir).expect("project dir");

        dir.canonicalize().expect("canonical")
    }

    fn workspace() -> WorkspaceState {
        let source = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop.yaml"),
        )
        .expect("fixture readable");

        let model = crate::parser::yaml::parse(&source).expect("fixture parses");

        WorkspaceState::from_model(&model, RunMetadata::new(RunId("shop".to_string())))
    }

    #[test]
    fn ids_encode_to_file_names_and_back() {
        for id in [
            "operation.create_order",
            "a/b c",
            "x%y",
            ".hidden",
            "é",
            "a.b-c_d",
        ] {
            let name = file_name(&Id(id.to_string())).expect("storable");
            let stem = name.strip_suffix(".yaml").expect("yaml");

            assert!(!stem.starts_with('.'), "{stem}");
            assert!(!stem.contains('/'), "{stem}");
            assert_eq!(decode_file_stem(stem).as_deref(), Some(id), "{stem}");
        }

        // A spelling that is not the canonical encoding names nothing,
        // so no two files can claim one id.
        assert_eq!(decode_file_stem("a%2Eb"), None);
        assert_eq!(decode_file_stem("a%2"), None);
        assert!(matches!(
            file_name(&Id(String::new())),
            Err(StoreError::EmptyId)
        ));
    }

    #[test]
    fn ids_differing_only_in_case_are_refused() {
        let mut workspace = workspace();
        let service = workspace
            .services
            .values()
            .next()
            .cloned()
            .expect("a service");

        workspace
            .services
            .insert(Id("svc".to_string()), service.clone());
        workspace.services.insert(Id("SVC".to_string()), service);

        assert!(matches!(
            render(&workspace),
            Err(StoreError::CaseCollision { .. })
        ));
    }

    #[test]
    fn a_created_project_reopens_to_the_same_workspace() {
        let dir = temp_project();
        let workspace = workspace();

        drop(ProjectStore::create(&dir, &workspace).expect("creates"));

        assert!(dir.join(".conseqa/spec/operations").is_dir());
        assert_eq!(
            fs::read_to_string(dir.join(".conseqa/.gitignore")).expect("gitignore"),
            GITIGNORE
        );

        let (_store, reopened) = ProjectStore::open(&dir).expect("reopens");

        assert_eq!(reopened, workspace);

        fs::remove_dir_all(&dir).ok();
    }

    /// Every valid fixture — runtime declarations included — reopens
    /// exactly as it was written.
    #[test]
    fn every_fixture_survives_the_files() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

        let mut paths: Vec<PathBuf> = fs::read_dir(&fixtures)
            .expect("fixtures")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
            .collect();

        paths.sort();

        let mut checked = 0;

        for path in paths {
            let source = fs::read_to_string(&path).expect("fixture readable");

            let Ok(model) = crate::parser::yaml::parse(&source) else {
                continue;
            };

            let workspace =
                WorkspaceState::from_model(&model, RunMetadata::new(RunId("fixture".into())));

            let dir = temp_project();

            drop(ProjectStore::create(&dir, &workspace).expect("creates"));

            let (_store, reopened) = ProjectStore::open(&dir).expect("reopens");

            assert_eq!(reopened, workspace, "{}", path.display());

            checked += 1;

            fs::remove_dir_all(&dir).ok();
        }

        assert!(checked >= 8, "only {checked} fixtures parsed");
    }

    /// A sketch's conditions and answers are externally tagged and
    /// untagged enums inside internally tagged steps — the shapes YAML
    /// tags would not carry back — and the intent files hold the
    /// requirements' provenance. All of it reopens as written.
    #[test]
    fn sketches_and_intent_survive_the_files() {
        use super::super::sketch::OperationSketch;
        use super::super::symbol::RequirementFamily;
        use super::super::workspace::{
            PromptObligation, PromptObligationId, PromptObligationStatus, ProposalStatus,
            ProposedRequirement, RequirementOrigin, RequirementProposal, RequirementRef,
        };

        let mut workspace = workspace();

        let sketch: OperationSketch = serde_json::from_value(serde_json::json!({ "steps": [
            { "kind": "find", "as": "order", "record": "object.order",
              "by": { "order_id": "input.order_id", "count": 3, "open": true, "note": "true" } },
            { "kind": "when", "if": { "any": [
                { "present": "order.status" },
                { "all": [ { "not": { "equals": ["order.status", "state.order.paid"] } } ] },
                { "unspecified": "the order looks risky" } ] },
              "then": [ { "kind": "reject", "error": "not_shippable" } ] },
            { "kind": "race", "as": "read",
              "steps": [
                { "kind": "call", "name": "store-a",
                  "result": { "ok": "schema.Row", "errors": { "miss": "schema.Miss" } },
                  "from": ["input.id"] },
                { "kind": "call", "name": "store-b",
                  "result": { "ok": "schema.Row",
                              "errors": { "busy": { "schema": "schema.Busy",
                                                    "disposition": "retryable" } } },
                  "from": { "id": "input.id" } } ],
              "on_error": { "miss": [ { "kind": "reject", "error": "miss" } ] } }
        ], "returns": ["read.id"] }))
        .expect("the sketch parses");

        let (operation, draft) = workspace
            .operations
            .iter_mut()
            .find(|(_, draft)| !draft.requirements.idempotency.is_empty())
            .expect("an operation declares idempotency");

        draft.sketch = Some(sketch);

        let operation = operation.clone();
        let requirement = draft.requirements.idempotency[0].clone();

        let reference = RequirementRef {
            operation: operation.clone(),
            family: RequirementFamily::Idempotency,
            transaction: None,
            index: 0,
        };

        let obligation = PromptObligationId("obligation.no_double_charge".to_string());

        workspace.prompt_obligations.insert(
            obligation.clone(),
            PromptObligation {
                source_span: Some("never charge twice".to_string()),
                normalized_intent: "a retried order charges once".to_string(),
                targets: vec![operation.clone()],
                status: PromptObligationStatus::Mapped {
                    requirements: vec![reference.clone()],
                },
            },
        );

        workspace.requirement_proposals.push(RequirementProposal {
            operation,
            requirement: ProposedRequirement::Idempotency(requirement),
            origin: RequirementOrigin::ExplicitPrompt { obligation },
            status: ProposalStatus::Adopted { reference },
        });

        let dir = temp_project();

        drop(ProjectStore::create(&dir, &workspace).expect("creates"));

        assert!(dir.join(".conseqa/intent/obligations.yaml").is_file());
        assert!(dir.join(".conseqa/intent/proposals.yaml").is_file());

        let (_store, reopened) = ProjectStore::open(&dir).expect("reopens");

        assert_eq!(reopened, workspace);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_writer_is_refused_while_the_first_holds_the_project() {
        let dir = temp_project();

        let store = ProjectStore::create(&dir, &workspace()).expect("creates");

        assert!(matches!(
            ProjectStore::open(&dir),
            Err(StoreError::Locked {
                holder: Some(_),
                ..
            })
        ));

        // Reading needs no lock.
        assert!(ProjectStore::read(&dir).is_ok());

        drop(store);

        assert!(ProjectStore::open(&dir).is_ok());

        fs::remove_dir_all(&dir).ok();
    }

    /// The home directory's `.conseqa` is the machine's configuration:
    /// no project is created there, and none found there, so it can
    /// never claim the user's repositories as its own.
    #[test]
    fn the_home_directory_is_never_a_project() {
        let home = temp_project();
        let repository = home.join("code/shop");

        fs::create_dir_all(repository.join("src")).expect("repository");

        assert!(matches!(
            ProjectStore::create_below(&home, &workspace(), Some(&home)),
            Err(StoreError::MachineHome(_))
        ));

        // A project.yaml left there by an older build is not found from
        // below, and does not stop a project in a repository.
        fs::create_dir_all(home.join(PROJECT_DIR)).expect("machine config dir");
        fs::write(home.join(PROJECT_DIR).join(PROJECT_FILE), "format: 9\n").expect("stray");

        assert_eq!(discover_below(&repository.join("src"), Some(&home)), None);
        assert_eq!(
            discover_below(&repository.join("src"), None),
            Some(home.clone())
        );

        drop(
            ProjectStore::create_below(&repository, &workspace(), Some(&home))
                .expect("a repository under home holds its own project"),
        );

        assert_eq!(
            discover_below(&repository.join("src"), Some(&home)),
            Some(repository.clone())
        );

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_project_inside_another_is_refused() {
        let dir = temp_project();

        drop(ProjectStore::create(&dir, &workspace()).expect("creates"));

        let nested = dir.join("services/api");
        fs::create_dir_all(&nested).expect("nested dir");

        assert!(matches!(
            ProjectStore::create(&nested, &workspace()),
            Err(StoreError::InsideProject { .. })
        ));

        assert_eq!(ProjectStore::discover(&nested), Some(dir.clone()));
        assert!(matches!(
            ProjectStore::create(&dir, &workspace()),
            Err(StoreError::AlreadyAProject(_))
        ));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_outside_edit_is_a_change_and_a_reformat_is_not() {
        let dir = temp_project();
        let store = ProjectStore::create(&dir, &workspace()).expect("creates");

        assert!(store.disk_change().expect("scans").is_none());

        let project_file = dir.join(".conseqa/project.yaml");
        let text = fs::read_to_string(&project_file).expect("project file");

        // Same content, different spelling: not a change.
        fs::write(&project_file, format!("# a comment\n{text}")).expect("rewrite");

        assert!(store.disk_change().expect("scans").is_none());

        fs::write(&project_file, text.replace("name: shop", "name: store")).expect("edit");

        let change = store
            .disk_change()
            .expect("scans")
            .expect("the edit is a change");

        assert_eq!(change.workspace.run_meta.run, RunId("store".to_string()));

        // A file that no longer loads is an error, never a silent
        // overwrite.
        fs::write(&project_file, "format: [").expect("break");

        assert!(matches!(store.disk_change(), Err(StoreError::Parse { .. })));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_interrupted_commit_is_completed_on_open() {
        let dir = temp_project();
        let workspace = workspace();

        drop(ProjectStore::create(&dir, &workspace).expect("creates"));

        let root = dir.join(PROJECT_DIR);

        let mut next = workspace.clone();
        next.revision = Revision(7);
        next.run_meta.policy.adopt_recommended = true;
        let removed = next.services.keys().next().cloned().expect("a service");
        next.services.remove(&removed);

        let before = render(&workspace).expect("renders");
        let after = render(&next).expect("renders");

        let writes: FileSet = after
            .iter()
            .filter(|(path, text)| before.get(*path) != Some(*text))
            .map(|(path, text)| (path.clone(), text.clone()))
            .collect();

        let deletes: Vec<String> = before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .cloned()
            .collect();

        assert!(!deletes.is_empty());

        // The journal was written, and the crash came before any file.
        write_json(
            &root.join(PENDING),
            &Pending {
                state: LocalState {
                    format: FORMAT,
                    revision: next.revision,
                    digest: digest(&after),
                },
                writes,
                deletes,
            },
        )
        .expect("journal");

        let (_store, reopened) = ProjectStore::open(&dir).expect("reopens");

        assert_eq!(reopened, next);
        assert!(!root.join(PENDING).exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn another_format_is_refused_by_name() {
        let dir = temp_project();

        drop(ProjectStore::create(&dir, &workspace()).expect("creates"));

        let project_file = dir.join(".conseqa/project.yaml");
        let text = fs::read_to_string(&project_file).expect("project file");

        fs::write(
            &project_file,
            text.replace(&format!("format: {FORMAT}"), "format: 8"),
        )
        .expect("edit");

        assert!(matches!(
            ProjectStore::open(&dir),
            Err(StoreError::FormatMismatch { found: 8, .. })
        ));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_directory_in_spec_is_refused() {
        let dir = temp_project();

        drop(ProjectStore::create(&dir, &workspace()).expect("creates"));

        fs::create_dir_all(dir.join(".conseqa/spec/operation")).expect("misspelled dir");

        assert!(matches!(
            ProjectStore::open(&dir),
            Err(StoreError::Unexpected { .. })
        ));

        fs::remove_dir_all(&dir).ok();
    }
}
