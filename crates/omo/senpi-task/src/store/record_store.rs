use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::event_log::{append_task_event, event_log_path};
use super::record_lock::{remove_if_present, with_task_record_lock};
use super::record_parse::parse_task_record;
use super::types::{
    ListTaskRecordsResult, PersistedTaskEvent, StateDirConfig, StoreError, TaskRecordDiagnostic,
    TombstoneResult, resolve_state_dir,
};
use crate::state::{
    TaskId, TaskRecord, TaskTransition, TaskTransitionResult, parse_task_id, transition_task_record,
};

const TOMBSTONE_SUFFIX: &str = ".json.expunging";

/// The post-commit observer every manager/lifecycle clone shares.
type MutationListener = std::sync::Arc<dyn Fn() + Send + Sync>;

/// File-backed task record store rooted at the resolved state directory.
#[derive(Clone)]
pub struct TaskRecordStore {
    state_dir: PathBuf,
    mutation_listener: std::sync::Arc<std::sync::Mutex<Option<MutationListener>>>,
}

impl std::fmt::Debug for TaskRecordStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskRecordStore").field("state_dir", &self.state_dir).finish_non_exhaustive()
    }
}

enum WriteMode {
    Create,
    Replace,
}

impl TaskRecordStore {
    pub fn new(config: &StateDirConfig) -> Self {
        Self {
            state_dir: resolve_state_dir(config),
            mutation_listener: Default::default(),
        }
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// All manager/lifecycle clones share the post-commit observer. Clear it during shutdown.
    pub fn set_mutation_listener(&self, listener: Option<MutationListener>) {
        *self.mutation_listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = listener;
    }

    fn notify_mutation(&self) {
        let listener = self.mutation_listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(listener) = listener { listener(); }
    }

    fn tasks_dir(&self) -> PathBuf {
        self.state_dir.join("tasks")
    }

    fn task_path(&self, task_id: TaskId) -> PathBuf {
        self.tasks_dir().join(format!("{task_id}.json"))
    }

    fn tombstone_path(&self, task_id: TaskId) -> PathBuf {
        self.tasks_dir()
            .join(format!("{task_id}{TOMBSTONE_SUFFIX}"))
    }

    /// Creates a new record file; an existing file for the id is a [`StoreError::Collision`].
    pub fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.write_record(record, WriteMode::Create)?;
        self.notify_mutation();
        Ok(())
    }

    /// Manager-owned overwrite for bookkeeping outside the transition table.
    pub fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let path = self.task_path(parse_task_id(&record.task_id)?);
        with_task_record_lock(&path, || self.write_record(record, WriteMode::Replace))?;
        self.notify_mutation();
        Ok(())
    }

    /// Serialized read-modify-write over the freshest on-disk record; returning an equal record
    /// skips the write.
    pub fn mutate(
        &self,
        task_id: &str,
        mutation: impl FnOnce(&TaskRecord) -> TaskRecord,
    ) -> Result<Option<TaskRecord>, StoreError> {
        let path = self.task_path(parse_task_id(task_id)?);
        let result = with_task_record_lock(&path, || {
            let Some(current) = read_record(&path, &mut Vec::new())? else {
                return Ok(None);
            };
            let next = mutation(&current);
            if next != current {
                self.write_record(&next, WriteMode::Replace)?;
            }
            Ok(Some(next))
        })?;
        self.notify_mutation();
        Ok(result)
    }

    pub fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        read_record(&self.task_path(parse_task_id(task_id)?), &mut Vec::new())
    }

    pub fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        let tasks_dir = self.tasks_dir();
        std::fs::create_dir_all(&tasks_dir)?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&tasks_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        files.sort();
        let mut records = Vec::new();
        let mut diagnostics = Vec::new();
        for path in files {
            let mut warnings = Vec::new();
            match read_record(&path, &mut warnings) {
                Ok(Some(record)) => {
                    records.push(record);
                    diagnostics.extend(warnings.into_iter().map(|message| {
                        TaskRecordDiagnostic::ParseWarning {
                            path: path.clone(),
                            message,
                        }
                    }));
                }
                Ok(None) => {}
                Err(StoreError::Parse(message)) => {
                    diagnostics.push(TaskRecordDiagnostic::ParseError { path, message });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ListTaskRecordsResult {
            records,
            diagnostics,
        })
    }

    pub fn append_event(
        &self,
        task_id: &str,
        event: &PersistedTaskEvent,
    ) -> Result<PathBuf, StoreError> {
        append_task_event(&self.state_dir, parse_task_id(task_id)?, event)
    }

    pub fn transition(
        &self,
        task_id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError> {
        let parsed = parse_task_id(task_id)?;
        let path = self.task_path(parsed);
        let result = with_task_record_lock(&path, || {
            let record = read_record(&path, &mut Vec::new())?
                .ok_or_else(|| StoreError::NotFound(task_id.to_string()))?;
            let result = transition_task_record(&record, transition);
            let payload = serde_json::to_value(&result.audit).unwrap_or(Value::Null);
            append_task_event(
                &self.state_dir,
                parsed,
                &PersistedTaskEvent {
                    event_type: result.audit.type_name().to_string(),
                    payload,
                },
            )?;
            if result.applied {
                self.write_record(&result.record, WriteMode::Replace)?;
            }
            Ok(result)
        })?;
        self.notify_mutation();
        Ok(result)
    }

    /// Deletes every durable artifact of a task, record last; idempotent.
    pub fn remove(&self, task_id: &str) -> Result<(), StoreError> {
        let parsed = parse_task_id(task_id)?;
        let path = self.task_path(parsed);
        with_task_record_lock(&path, || self.remove_record(parsed))?;
        self.notify_mutation();
        Ok(())
    }

    /// TTL expunge phase 1: under the lock, re-read and tombstone unless `should_retain` holds.
    pub fn tombstone_if_expired(
        &self,
        task_id: &str,
        should_retain: impl FnOnce(&TaskRecord) -> bool,
    ) -> Result<TombstoneResult, StoreError> {
        let parsed = parse_task_id(task_id)?;
        let path = self.task_path(parsed);
        std::fs::create_dir_all(self.tasks_dir())?;
        let result = with_task_record_lock(&path, || {
            let Some(current) = read_record(&path, &mut Vec::new())? else {
                return Ok(TombstoneResult::Missing);
            };
            if should_retain(&current) {
                return Ok(TombstoneResult::Retained);
            }
            std::fs::rename(&path, self.tombstone_path(parsed))?;
            Ok(TombstoneResult::Tombstoned(Box::new(current)))
        })?;
        if matches!(result, TombstoneResult::Tombstoned(_)) { self.notify_mutation(); }
        Ok(result)
    }

    /// TTL expunge phase 2 and crash recovery; the record is already committed to deletion.
    pub fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        let parsed = parse_task_id(task_id)?;
        self.remove_record(parsed)?;
        remove_if_present(&self.tombstone_path(parsed))?;
        self.notify_mutation();
        Ok(())
    }

    pub fn list_expunging(&self) -> Result<Vec<String>, StoreError> {
        let tasks_dir = self.tasks_dir();
        std::fs::create_dir_all(&tasks_dir)?;
        let mut ids: Vec<String> = std::fs::read_dir(&tasks_dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(TOMBSTONE_SUFFIX).map(str::to_string))
            .filter(|name| parse_task_id(name).is_ok())
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn remove_record(&self, task_id: TaskId) -> Result<(), StoreError> {
        let children = self.state_dir.join("children").join(task_id.to_string());
        match std::fs::remove_dir_all(&children) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        remove_if_present(
            &self
                .state_dir
                .join("completion-results")
                .join(format!("{task_id}.txt")),
        )?;
        remove_if_present(&event_log_path(&self.state_dir, task_id))?;
        remove_if_present(&self.task_path(task_id))
    }

    fn write_record(&self, record: &TaskRecord, mode: WriteMode) -> Result<(), StoreError> {
        let task_id = parse_task_id(&record.task_id)?;
        std::fs::create_dir_all(self.tasks_dir())?;
        let path = self.task_path(task_id);
        let payload =
            serde_json::to_string(record).map_err(|error| StoreError::Parse(error.to_string()))?;
        match mode {
            WriteMode::Create => {
                let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err(StoreError::Collision { task_id, path });
                    }
                    Err(error) => return Err(error.into()),
                };
                file.write_all(payload.as_bytes())?;
            }
            WriteMode::Replace => {
                let mut tmp = path.as_os_str().to_os_string();
                tmp.push(format!(".{}.tmp", std::process::id()));
                let tmp = PathBuf::from(tmp);
                std::fs::write(&tmp, payload)?;
                std::fs::rename(&tmp, &path)?;
            }
        }
        Ok(())
    }
}

fn read_record(path: &Path, warnings: &mut Vec<String>) -> Result<Option<TaskRecord>, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| StoreError::Parse(format!("JSON Parse error: {error}")))?;
    parse_task_record(&value, &path.to_string_lossy(), warnings)
        .map(Some)
        .map_err(StoreError::Parse)
}
