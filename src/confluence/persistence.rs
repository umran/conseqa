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

const FORMAT: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("storage: {0}")]
    Storage(#[from] redb::Error),

    #[error("stored value does not deserialize: {0}")]
    Corrupt(#[from] serde_json::Error),

    #[error("database format {found} is newer than this build understands ({FORMAT})")]
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
                Some(found) => {
                    if found > FORMAT {
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
