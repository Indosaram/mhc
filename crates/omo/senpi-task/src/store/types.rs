use std::path::PathBuf;

use serde_json::Value;

use crate::state::{InvalidTaskIdError, TaskRecord};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateDirConfig {
    pub project_dir: PathBuf,
    /// `task.state_dir` override from `omo.json`.
    pub task_state_dir: Option<PathBuf>,
}

pub fn resolve_state_dir(config: &StateDirConfig) -> PathBuf {
    config
        .task_state_dir
        .clone()
        .unwrap_or_else(|| config.project_dir.join(".omo").join("senpi-task"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRecordDiagnostic {
    ParseError { path: PathBuf, message: String },
    ParseWarning { path: PathBuf, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListTaskRecordsResult {
    pub records: Vec<TaskRecord>,
    pub diagnostics: Vec<TaskRecordDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedTaskEvent {
    pub event_type: String,
    pub payload: Value,
}

/// Outcome of TTL expunge phase 1; `Tombstoned` carries the record re-read under the lock.
#[derive(Debug, Clone, PartialEq)]
pub enum TombstoneResult {
    Tombstoned(Box<TaskRecord>),
    Retained,
    Missing,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    InvalidTaskId(#[from] InvalidTaskIdError),
    #[error("Task record already exists: {task_id}")]
    Collision {
        task_id: crate::state::TaskId,
        path: PathBuf,
    },
    #[error("Task record not found: {0}")]
    NotFound(String),
    #[error("Timed out acquiring task record lock: {}", .0.display())]
    LockTimeout(PathBuf),
    /// A persisted record failed to parse; the message is the diagnostic text.
    #[error("{0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
