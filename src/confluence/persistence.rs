//! Durable authoring state in redb.
//!
//! One write transaction persists the complete workspace revision, the
//! commit record, the new head pointer, the committing task's state,
//! and the idempotency nonce — atomically (§79 of the confluence
//! spec). Only after redb commits does the engine publish the new
//! in-memory head, so a crash either keeps the old head authoritative
//! or reconstructs the committed one on restart.
//!
//! V1 persists the full serialized workspace per revision (§78):
//! architecture specs are small, and this makes recovery, audit, and
//! snapshot pinning trivial. Graphs are never persisted — they are
//! rebuilt from the head on startup (§77).

use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spec::Revision;

use super::commit::CommitRecord;
use super::task::{DependencyRequest, TaskId, TaskSpec, TaskState};
use super::workspace::WorkspaceState;

const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const WORKSPACES: TableDefinition<u64, &[u8]> = TableDefinition::new("workspace_revisions");
const COMMITS: TableDefinition<u64, &[u8]> = TableDefinition::new("commits");
const TASKS: TableDefinition<&str, &[u8]> = TableDefinition::new("tasks");
const TASK_EVENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("task_events");
const DEPENDENCY_REQUESTS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("dependency_requests");
const NONCES: TableDefinition<&str, u64> = TableDefinition::new("commit_nonces");

const HEAD_KEY: &str = "head";
const FORMAT_KEY: &str = "format";
const EVENT_SEQ_KEY: &str = "task_event_seq";

/// The stored-workspace schema version.
///
/// Bumped to 2 when operations lost their execution facts and the L1
/// runtime model arrived: `DraftOperation` and `WorkspaceState` both
/// carry `deny_unknown_fields`, so a database written by format 1
/// cannot be deserialized at all. Refusing it by version gives that a
/// name, rather than surfacing a schema change as a corrupt value.
///
/// Bumped to 3 when the external boundary's single idempotency
/// guarantee split into identity / idempotency / result-replay and
/// the DSL contract version (`dsl: 1`) arrived: stored workspaces
/// embed the spec types, so a DSL bump forces a format bump — never
/// conversely.
const FORMAT: u64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("storage: {0}")]
    Storage(#[from] redb::Error),

    #[error("stored value does not deserialize: {0}")]
    Corrupt(#[from] serde_json::Error),

    #[error(
        "database is format {found}, but this build reads format {FORMAT}; the stored \
         workspace schema changed and cannot be migrated automatically \
         — start a new run against a fresh database"
    )]
    FormatMismatch { found: u64 },
}

impl From<redb::DatabaseError> for PersistenceError {
    fn from(error: redb::DatabaseError) -> Self {
        Self::Storage(error.into())
    }
}

impl From<redb::TransactionError> for PersistenceError {
    fn from(error: redb::TransactionError) -> Self {
        Self::Storage(error.into())
    }
}

impl From<redb::TableError> for PersistenceError {
    fn from(error: redb::TableError) -> Self {
        Self::Storage(error.into())
    }
}

impl From<redb::StorageError> for PersistenceError {
    fn from(error: redb::StorageError) -> Self {
        Self::Storage(error.into())
    }
}

impl From<redb::CommitError> for PersistenceError {
    fn from(error: redb::CommitError) -> Self {
        Self::Storage(error.into())
    }
}

/// A task as persisted: its immutable spec and current state. Read
/// sets are deliberately not persisted — a restart loses live read
/// tracking, so active tasks are conservatively invalidated (§84).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub spec: TaskSpec,
    pub state: TaskState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskEventRecord {
    task: TaskId,
    state: TaskState,
    timestamp_unix_ms: u64,
}

pub struct Persistence {
    db: Database,
}

impl Persistence {
    pub fn open_file(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                PersistenceError::Storage(redb::Error::Io(error))
            })?;
        }

        let db = Database::create(path)?;

        Self::init(db)
    }

    pub fn in_memory() -> Result<Self, PersistenceError> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())?;

        Self::init(db)
    }

    fn init(db: Database) -> Result<Self, PersistenceError> {
        let this = Self { db };

        let txn = this.db.begin_write()?;

        {
            // Opening creates the tables on a fresh database.
            let mut meta = txn.open_table(META)?;
            txn.open_table(WORKSPACES)?;
            txn.open_table(COMMITS)?;
            txn.open_table(TASKS)?;
            txn.open_table(TASK_EVENTS)?;
            txn.open_table(DEPENDENCY_REQUESTS)?;
            txn.open_table(NONCES)?;

            let found = meta.get(FORMAT_KEY)?.map(|value| value.value());

            match found {
                // Both directions are refused. An older database holds
                // a workspace shape this build cannot deserialize, and
                // reporting that as a corrupt value would send a reader
                // looking for disk damage.
                Some(found) => {
                    if found != FORMAT {
                        return Err(PersistenceError::FormatMismatch { found });
                    }
                }

                None => {
                    meta.insert(FORMAT_KEY, FORMAT)?;
                }
            }
        }

        txn.commit()?;

        Ok(this)
    }

    /// The persisted head revision, if any commit or initialization
    /// happened.
    pub fn head(&self) -> Result<Option<Revision>, PersistenceError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;

        Ok(meta.get(HEAD_KEY)?.map(|value| Revision(value.value())))
    }

    /// Persists the initial workspace as the head. Called once, when
    /// the database holds no head yet.
    pub fn init_head(&self, workspace: &WorkspaceState) -> Result<(), PersistenceError> {
        let bytes = serde_json::to_vec(workspace)?;

        let txn = self.db.begin_write()?;

        {
            let mut workspaces = txn.open_table(WORKSPACES)?;
            workspaces.insert(workspace.revision.0, bytes.as_slice())?;

            let mut meta = txn.open_table(META)?;
            meta.insert(HEAD_KEY, workspace.revision.0)?;
        }

        txn.commit()?;

        Ok(())
    }

    pub fn load_workspace(
        &self,
        revision: Revision,
    ) -> Result<Option<WorkspaceState>, PersistenceError> {
        let txn = self.db.begin_read()?;
        let workspaces = txn.open_table(WORKSPACES)?;

        match workspaces.get(revision.0)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(serde_json::from_slice(bytes.value())?)),
        }
    }

    /// Atomically persists one accepted commit: the new workspace
    /// revision, the commit record, the head pointer, the nonce, the
    /// committing task's new state, and its lifecycle event.
    pub fn persist_commit(
        &self,
        workspace: &WorkspaceState,
        record: &CommitRecord,
        task_state: TaskState,
    ) -> Result<(), PersistenceError> {
        let workspace_bytes = serde_json::to_vec(workspace)?;
        let record_bytes = serde_json::to_vec(record)?;

        let txn = self.db.begin_write()?;

        {
            let mut workspaces = txn.open_table(WORKSPACES)?;
            workspaces.insert(record.revision.0, workspace_bytes.as_slice())?;

            let mut commits = txn.open_table(COMMITS)?;
            commits.insert(record.revision.0, record_bytes.as_slice())?;

            let mut nonces = txn.open_table(NONCES)?;
            nonces.insert(
                nonce_key(record.task, record.client_nonce).as_str(),
                record.revision.0,
            )?;

            update_task_state_in(&txn, record.task, task_state, record.timestamp_unix_ms)?;

            let mut meta = txn.open_table(META)?;
            meta.insert(HEAD_KEY, record.revision.0)?;
        }

        txn.commit()?;

        Ok(())
    }

    /// The revision previously committed under this task's nonce, for
    /// idempotent resubmission (§84).
    pub fn nonce_revision(
        &self,
        task: TaskId,
        nonce: Uuid,
    ) -> Result<Option<Revision>, PersistenceError> {
        let txn = self.db.begin_read()?;
        let nonces = txn.open_table(NONCES)?;

        Ok(nonces
            .get(nonce_key(task, nonce).as_str())?
            .map(|value| Revision(value.value())))
    }

    pub fn record_task(
        &self,
        spec: &TaskSpec,
        state: TaskState,
        timestamp_unix_ms: u64,
    ) -> Result<(), PersistenceError> {
        let record = TaskRecord {
            spec: spec.clone(),
            state,
        };

        let bytes = serde_json::to_vec(&record)?;

        let txn = self.db.begin_write()?;

        {
            let mut tasks = txn.open_table(TASKS)?;
            tasks.insert(spec.id.0.to_string().as_str(), bytes.as_slice())?;

            append_task_event(&txn, spec.id, state, timestamp_unix_ms)?;
        }

        txn.commit()?;

        Ok(())
    }

    pub fn update_task_state(
        &self,
        task: TaskId,
        state: TaskState,
        timestamp_unix_ms: u64,
    ) -> Result<(), PersistenceError> {
        let txn = self.db.begin_write()?;

        update_task_state_in(&txn, task, state, timestamp_unix_ms)?;

        txn.commit()?;

        Ok(())
    }

    pub fn record_dependency_request(
        &self,
        request: &DependencyRequest,
    ) -> Result<(), PersistenceError> {
        let bytes = serde_json::to_vec(request)?;

        let txn = self.db.begin_write()?;

        {
            let mut requests = txn.open_table(DEPENDENCY_REQUESTS)?;
            requests.insert(request.id.0.to_string().as_str(), bytes.as_slice())?;
        }

        txn.commit()?;

        Ok(())
    }

    /// Every dependency request ever filed, in id order.
    pub fn load_dependency_requests(&self) -> Result<Vec<DependencyRequest>, PersistenceError> {
        let txn = self.db.begin_read()?;

        let requests = match txn.open_table(DEPENDENCY_REQUESTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };

        let mut loaded = Vec::new();

        for entry in requests.iter()? {
            let (_, value) = entry?;
            loaded.push(serde_json::from_slice(value.value())?);
        }

        Ok(loaded)
    }

    pub fn load_tasks(&self) -> Result<Vec<TaskRecord>, PersistenceError> {
        let txn = self.db.begin_read()?;
        let tasks = txn.open_table(TASKS)?;

        let mut records = Vec::new();

        for entry in tasks.iter()? {
            let (_, bytes) = entry?;

            records.push(serde_json::from_slice(bytes.value())?);
        }

        Ok(records)
    }

    pub fn load_commits(&self) -> Result<Vec<CommitRecord>, PersistenceError> {
        let txn = self.db.begin_read()?;
        let commits = txn.open_table(COMMITS)?;

        let mut records = Vec::new();

        for entry in commits.iter()? {
            let (_, bytes) = entry?;

            records.push(serde_json::from_slice(bytes.value())?);
        }

        Ok(records)
    }
}

fn nonce_key(task: TaskId, nonce: Uuid) -> String {
    format!("{}/{}", task.0, nonce)
}

fn update_task_state_in(
    txn: &redb::WriteTransaction,
    task: TaskId,
    state: TaskState,
    timestamp_unix_ms: u64,
) -> Result<(), PersistenceError> {
    let mut tasks = txn.open_table(TASKS)?;
    let key = task.0.to_string();

    let existing = tasks
        .get(key.as_str())?
        .map(|bytes| bytes.value().to_vec());

    if let Some(bytes) = existing {
        let mut record: TaskRecord = serde_json::from_slice(&bytes)?;

        record.state = state;

        let bytes = serde_json::to_vec(&record)?;

        tasks.insert(key.as_str(), bytes.as_slice())?;
    }

    drop(tasks);

    append_task_event(txn, task, state, timestamp_unix_ms)?;

    Ok(())
}

fn append_task_event(
    txn: &redb::WriteTransaction,
    task: TaskId,
    state: TaskState,
    timestamp_unix_ms: u64,
) -> Result<(), PersistenceError> {
    let mut meta = txn.open_table(META)?;

    let sequence = meta.get(EVENT_SEQ_KEY)?.map(|value| value.value()).unwrap_or(0);

    meta.insert(EVENT_SEQ_KEY, sequence + 1)?;

    drop(meta);

    let record = TaskEventRecord {
        task,
        state,
        timestamp_unix_ms,
    };

    let bytes = serde_json::to_vec(&record)?;

    let mut events = txn.open_table(TASK_EVENTS)?;

    events.insert(sequence, bytes.as_slice())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database written by an earlier stored-workspace format is
    /// refused by version with the named error — never surfaced as a
    /// corrupt value.
    #[test]
    fn an_earlier_format_database_is_refused_by_version() {
        let path = std::env::temp_dir().join(format!(
            "conseqa-format-test-{}.redb",
            uuid::Uuid::new_v4()
        ));

        // A fresh database stamps the current format.
        drop(Persistence::open_file(&path).expect("a fresh database opens"));

        // Rewind the stamp to the previous format, as an old binary
        // would have left it.
        {
            let db = Database::open(&path).expect("the database reopens raw");
            let txn = db.begin_write().expect("write txn");
            {
                let mut meta = txn.open_table(META).expect("meta table");
                meta.insert(FORMAT_KEY, FORMAT - 1).expect("stamp old format");
            }
            txn.commit().expect("commit");
        }

        let error = match Persistence::open_file(&path) {
            Err(error) => error,
            Ok(_) => panic!("the old format should be refused"),
        };

        assert!(
            matches!(error, PersistenceError::FormatMismatch { found } if found == FORMAT - 1),
            "{error:?}"
        );

        assert!(
            error.to_string().contains("start a new run against a fresh database"),
            "{error}"
        );

        std::fs::remove_file(&path).ok();
    }
}
