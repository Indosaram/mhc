pub type RecordListenerCallback = std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<dyn Fn(&str, Option<&TaskRecord>) + Send + Sync>>>>;
pub type ListenerCallback = Option<std::sync::Arc<dyn Fn(&str, Option<&TaskRecord>) + Send + Sync>>;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::event_log::{append_task_event, event_log_path};
use super::record_lock::{remove_if_present, retry_windows_sharing, with_task_record_lock};
use super::record_parse::parse_task_record;
use super::types::{
    ListTaskRecordsResult, PersistedTaskEvent, StateDirConfig, StoreError, TaskRecordDiagnostic,
    TombstoneResult, resolve_state_dir,
};
use crate::state::{
    TaskId, TaskRecord, TaskStatus, TaskTransition, TaskTransitionResult, parse_task_id,
    transition_task_record,
};

const TOMBSTONE_SUFFIX: &str = ".json.expunging";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRecordStoreResidentFigures {
    pub cached_records: usize,
    pub cached_record_bytes: usize,
    pub open_event_logs: usize,
}

/// Encode paused records additively so rollback readers retain a valid status.
pub fn to_storable_task_record(record: &TaskRecord) -> Result<Value, serde_json::Error> {
    let mut stored = serde_json::to_value(record)?;
    if let Some(fallback) = stored.get_mut("parent_model_fallback").and_then(Value::as_object_mut) {
        fallback.remove("phase");
    }
    if record.status == TaskStatus::Paused {
        stored["status"] = Value::String("running".into());
        stored["paused"] = Value::Bool(true);
    }
    Ok(stored)
}

/// The post-commit observer every manager/lifecycle clone shares.
type MutationListener = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Committed record and its status read under the same record lock, delivered after unlocking.
pub type WriteListener = std::sync::Arc<dyn Fn(&TaskRecord, Option<TaskStatus>) + Send + Sync>;

/// A parsed record plus the length and hash of the file text it was parsed from. Reuse is keyed
/// on content, not metadata: a same-length rewrite inside one mtime tick (or onto a reused inode)
/// must never serve a stale record, so only the JSON parse is skipped, never the read.
#[derive(Clone)]
struct CachedRecord {
    len: usize,
    hash: u64,
    record: TaskRecord,
    warnings: Vec<String>,
}

impl CachedRecord {
    fn matches(&self, text: &str) -> bool {
        self.len == text.len() && self.hash == text_hash(text)
    }
}

fn text_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

type ListCache = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, CachedRecord>>>;

/// One parse cache per state directory for the whole process. Session replacement rebuilds the
/// task engine, and a per-store cache made every `/new` re-parse every retained record; entries
/// are validated against the file text on each read, so sharing never serves a stale record.
fn shared_list_cache(state_dir: &Path) -> ListCache {
    static CACHES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, ListCache>>,
    > = std::sync::OnceLock::new();
    CACHES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(state_dir.to_path_buf())
        .or_default()
        .clone()
}

/// File-backed task record store rooted at the resolved state directory.
#[derive(Clone)]
pub struct TaskRecordStore {
    state_dir: PathBuf,
    record_listener: RecordListenerCallback,
    mutation_listener: std::sync::Arc<std::sync::Mutex<Option<MutationListener>>>,
    write_listener: std::sync::Arc<std::sync::Mutex<Option<WriteListener>>>,
    list_cache: ListCache,
}

impl std::fmt::Debug for TaskRecordStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskRecordStore")
            .field("state_dir", &self.state_dir)
            .finish_non_exhaustive()
    }
}

enum WriteMode {
    Create,
    Replace,
}

impl TaskRecordStore {
    pub fn new(config: &StateDirConfig) -> Self {
        let state_dir = resolve_state_dir(config);
        Self {
            list_cache: shared_list_cache(&state_dir),
            state_dir,
            record_listener: Default::default(),
            mutation_listener: Default::default(),
            write_listener: Default::default(),
        }
    }

    pub fn resident_figures(&self) -> TaskRecordStoreResidentFigures {
        let cache = self
            .list_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        TaskRecordStoreResidentFigures {
            cached_records: cache.len(),
            cached_record_bytes: cache.values().map(|entry| entry.len).sum(),
            // Event appends open and close their file within append_task_event; none stay resident.
            open_event_logs: 0,
        }
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// All manager/lifecycle clones share the post-commit observer. Clear it during shutdown.
    pub fn set_record_listener(&self, listener: ListenerCallback) {
        *self.record_listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = listener;
    }
    fn notify_record(&self, id: &str, record: Option<&TaskRecord>) {
        let listener = self.record_listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(listener) = listener { listener(id, record); }
    }
    pub fn set_mutation_listener(&self, listener: Option<MutationListener>) {
        *self
            .mutation_listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = listener;
    }

    /// Independent of the payload-free mutation observer; shared by all store clones.
    pub fn set_write_listener(&self, listener: Option<WriteListener>) {
        *self
            .write_listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = listener;
    }

    fn notify_write(&self, record: &TaskRecord, previous_status: Option<TaskStatus>) {
        let listener = self
            .write_listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(listener) = listener
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                listener(record, previous_status)
            }))
            .is_err()
        {
            eprintln!("task write observer panicked after commit");
        }
    }

    fn notify_mutation(&self) {
        let listener = self
            .mutation_listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(listener) = listener {
            listener();
        }
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
        // As in pinned record-store.ts, exclusive record creation is the claim arbiter.
        // A lock adds a delete-pending race without strengthening CREATE_NEW uniqueness.
        self.write_record(record, WriteMode::Create)?;
        self.notify_write(record, None);
        self.notify_record(&record.task_id, Some(record));
        self.notify_mutation();
        Ok(())
    }

    /// Manager-owned overwrite for bookkeeping outside the transition table.
    pub fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let path = self.task_path(parse_task_id(&record.task_id)?);
        let previous_status = with_task_record_lock(&path, || {
            // An unreadable old record must not prevent a manager-owned replacement.
            let previous = read_record(&path, &mut Vec::new())
                .ok()
                .flatten()
                .map(|old| old.status);
            self.write_record(record, WriteMode::Replace)?;
            Ok(previous)
        })?;
        self.notify_write(record, previous_status);
        self.notify_record(&record.task_id, Some(record));
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
        let (result, previous_status, written) = with_task_record_lock(&path, || {
            let Some(current) = read_record(&path, &mut Vec::new())? else {
                return Ok((None, None, false));
            };
            let next = mutation(&current);
            let written = next != current;
            if written {
                self.write_record(&next, WriteMode::Replace)?;
            }
            Ok((Some(next), Some(current.status), written))
        })?;
        if written && let Some(record) = &result {
            self.notify_write(record, previous_status);
        }
        self.notify_record(task_id, result.as_ref());
        self.notify_mutation();
        Ok(result)
    }

    pub fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        let path = self.task_path(parse_task_id(task_id)?);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.list_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&path);
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        self.parse_and_cache(&path, text, &mut Vec::new())
    }

    pub fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        self.list_matching(|_| true)
    }

    /// [`Self::list`] that keeps (and therefore clones) only the records `keep` accepts;
    /// diagnostics are reported for every file exactly as `list` does.
    // PERF-DIVERGENCE(senpi task-record-store.ts): upstream lists and clones every task record and lets callers filter afterwards; this port filters on the cached record by reference and clones only the matches to avoid an O(N) clone per admission pass. The kept records and diagnostics are identical to filtering the `list` result.
    pub fn list_matching(
        &self,
        mut keep: impl FnMut(&TaskRecord) -> bool,
    ) -> Result<ListTaskRecordsResult, StoreError> {
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
        self.list_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|path, _| files.binary_search(path).is_ok());
        let mut records = Vec::new();
        let mut diagnostics = Vec::new();
        for path in files {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            {
                let cache = self
                    .list_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(cached) = cache.get(&path).filter(|cached| cached.matches(&text)) {
                    if keep(&cached.record) {
                        records.push(cached.record.clone());
                    }
                    diagnostics.extend(cached.warnings.iter().map(|message| {
                        TaskRecordDiagnostic::ParseWarning {
                            path: path.clone(),
                            message: message.clone(),
                        }
                    }));
                    continue;
                }
            }
            let mut warnings = Vec::new();
            match self.parse_and_cache(&path, text, &mut warnings) {
                Ok(Some(record)) => {
                    if keep(&record) {
                        records.push(record);
                    }
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
        if !records.iter().any(|record| {
            matches!(
                record.status,
                TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused
            )
        }) {
            let mut cache = self
                .list_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.clear();
            cache.shrink_to_fit();
        }
        Ok(ListTaskRecordsResult {
            records,
            diagnostics,
        })
    }

    /// First record, in [`Self::list`] order, matching `pred`; stops scanning at the match.
    /// Parse errors are skipped as in `list`. Shares the list cache, which is revalidated by
    /// file stamp on every read, so cross-process owner claims are always seen.
    pub fn find_record(
        &self,
        mut pred: impl FnMut(&TaskRecord) -> bool,
    ) -> Result<Option<TaskRecord>, StoreError> {
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
        for path in files {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            {
                let cache = self
                    .list_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(cached) = cache.get(&path).filter(|cached| cached.matches(&text)) {
                    if pred(&cached.record) {
                        return Ok(Some(cached.record.clone()));
                    }
                    continue;
                }
            }
            match self.parse_and_cache(&path, text, &mut Vec::new()) {
                Ok(Some(record)) if pred(&record) => return Ok(Some(record)),
                Ok(_) | Err(StoreError::Parse(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    /// Parses already-read `text` for `path` outside the cache lock and caches the parse keyed by
    /// that exact text; a failed parse drops any entry for the path.
    fn parse_and_cache(
        &self,
        path: &Path,
        text: String,
        warnings: &mut Vec<String>,
    ) -> Result<Option<TaskRecord>, StoreError> {
        let parsed = parse_record_text(path, &text, warnings);
        let (len, hash) = (text.len(), text_hash(&text));
        drop(text);
        let mut cache = self
            .list_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &parsed {
            Ok(Some(record)) => {
                cache.insert(
                    path.to_path_buf(),
                    CachedRecord {
                        len,
                        hash,
                        record: record.clone(),
                        warnings: warnings.clone(),
                    },
                );
            }
            _ => {
                cache.remove(path);
            }
        }
        parsed
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
        self.transition_expected_epoch(task_id, transition, None)
            .and_then(|result| {
                result.ok_or_else(|| {
                    StoreError::Parse("unfenced transition was unexpectedly stale".to_string())
                })
            })
    }

    pub fn transition_expected_epoch(
        &self,
        task_id: &str,
        transition: &TaskTransition,
        expected_run_epoch: Option<i64>,
    ) -> Result<Option<TaskTransitionResult>, StoreError> {
        let parsed = parse_task_id(task_id)?;
        let path = self.task_path(parsed);
        let (result, previous_status) = with_task_record_lock(&path, || {
            let record = read_record(&path, &mut Vec::new())?
                .ok_or_else(|| StoreError::NotFound(task_id.to_string()))?;
            if expected_run_epoch.is_some_and(|expected| record.notification.run_epoch != expected)
            {
                return Ok((None, Some(record.status)));
            }
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
            Ok((Some(result), Some(record.status)))
        })?;
        if let Some(result) = &result
            && result.applied
        {
            self.notify_write(&result.record, previous_status);
        }
        if let Some(result) = &result {
            self.notify_record(task_id, Some(&result.record));
            self.notify_mutation();
        }
        Ok(result)
    }

    /// Deletes every durable artifact of a task, record last; idempotent.
    pub fn remove(&self, task_id: &str) -> Result<(), StoreError> {
        let parsed = parse_task_id(task_id)?;
        let path = self.task_path(parsed);
        with_task_record_lock(&path, || self.remove_record(parsed))?;
        self.notify_record(task_id, None);
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
        if matches!(result, TombstoneResult::Tombstoned(_)) {
            self.notify_record(task_id, None);
            self.notify_mutation();
        }
        Ok(result)
    }

    /// TTL expunge phase 2 and crash recovery; the record is already committed to deletion.
    pub fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        let parsed = parse_task_id(task_id)?;
        self.remove_record(parsed)?;
        remove_if_present(&self.tombstone_path(parsed))?;
        self.notify_record(task_id, None);
        self.notify_mutation();
        Ok(())
    }

    pub fn restore_expunging(&self, task_id: &str) -> Result<(), StoreError> {
        let parsed = parse_task_id(task_id)?;
        let path = self.task_path(parsed);
        with_task_record_lock(&path, || {
            std::fs::rename(self.tombstone_path(parsed), &path)?;
            Ok(())
        })?;
        let record = self.load(task_id)?;
        self.notify_record(task_id, record.as_ref());
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
        remove_if_present(&self.task_path(task_id))?;
        self.list_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.task_path(task_id));
        Ok(())
    }

    fn write_record(&self, record: &TaskRecord, mode: WriteMode) -> Result<(), StoreError> {
        let task_id = parse_task_id(&record.task_id)?;
        std::fs::create_dir_all(self.tasks_dir())?;
        let path = self.task_path(task_id);
        let payload = if record.status == TaskStatus::Paused {
            serde_json::to_string(
                &to_storable_task_record(record)
                    .map_err(|error| StoreError::Parse(error.to_string()))?,
            )
        } else {
            serde_json::to_string(record)
        }
        .map_err(|error| StoreError::Parse(error.to_string()))?;
        let cached = CachedRecord {
            len: payload.len(),
            hash: text_hash(&payload),
            record: record.clone(),
            warnings: Vec::new(),
        };
        match mode {
            WriteMode::Create => {
                let mut file = match retry_windows_sharing(|| {
                    OpenOptions::new().write(true).create_new(true).open(&path)
                }) {
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
                tmp.push(format!(
                    ".{}.{}.tmp",
                    std::process::id(),
                    uuid::Uuid::new_v4()
                ));
                let tmp = PathBuf::from(tmp);
                let result = std::fs::write(&tmp, payload)
                    .and_then(|()| retry_windows_sharing(|| std::fs::rename(&tmp, &path)));
                let cleanup = remove_if_present(&tmp);
                result?;
                cleanup?;
            }
        }
        self.list_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path, cached);
        Ok(())
    }
}

fn read_record(path: &Path, warnings: &mut Vec<String>) -> Result<Option<TaskRecord>, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    parse_record_text(path, &text, warnings)
}

fn parse_record_text(
    path: &Path,
    text: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<TaskRecord>, StoreError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| StoreError::Parse(format!("JSON Parse error: {error}")))?;
    parse_task_record(&value, &path.to_string_lossy(), warnings)
        .map(Some)
        .map_err(StoreError::Parse)
}

#[cfg(test)]
mod write_observer_tests {
    use super::*;
    use crate::state::{TaskRecordInput, create_task_record};
    use std::sync::{Arc, Mutex};

    #[test]
    fn observes_committed_status_and_preserves_mutation_callback_after_unlock() {
        // Given
        let root = tempfile::tempdir().expect("state");
        let store = TaskRecordStore::new(&StateDirConfig {
            project_dir: root.path().into(),
            task_state_dir: None,
        });
        let mut record = create_task_record(
            TaskRecordInput {
                execution_mode: "in-process".into(),
                ..Default::default()
            },
            Some(0),
        )
        .expect("record");
        store.save(&record).expect("save");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let output = observed.clone();
        let reader = store.clone();
        store.set_write_listener(Some(Arc::new(move |record, previous| {
            assert_eq!(
                reader.load(&record.task_id).expect("read"),
                Some(record.clone())
            );
            assert!(
                !reader
                    .task_path(parse_task_id(&record.task_id).expect("id"))
                    .with_extension("json.lock")
                    .exists()
            );
            output
                .lock()
                .expect("edges")
                .push((previous, record.status));
        })));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        store.set_mutation_listener(Some(Arc::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));
        record.status = TaskStatus::Lost;
        record.terminal_at = Some(record.updated_at.clone());
        // When
        store.clone().replace(&record).expect("replace");
        // Then
        assert_eq!(
            *observed.lock().expect("edges"),
            vec![(Some(TaskStatus::Pending), TaskStatus::Lost)]
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn does_not_observe_failed_stale_or_unchanged_writes() {
        // Given
        let root = tempfile::tempdir().expect("state");
        let store = TaskRecordStore::new(&StateDirConfig {
            project_dir: root.path().into(),
            task_state_dir: None,
        });
        let record = create_task_record(
            TaskRecordInput {
                execution_mode: "in-process".into(),
                ..Default::default()
            },
            Some(0),
        )
        .expect("record");
        store.save(&record).expect("save");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        store.set_write_listener(Some(Arc::new(move |_, _| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));
        // When
        assert!(store.save(&record).is_err());
        store
            .mutate(&record.task_id, Clone::clone)
            .expect("unchanged");
        assert!(
            store
                .transition_expected_epoch(
                    &record.task_id,
                    &TaskTransition::Lose {
                        timestamp: record.updated_at.clone(),
                        error_message: "lost".into()
                    },
                    Some(1)
                )
                .expect("stale")
                .is_none()
        );
        // Then
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
