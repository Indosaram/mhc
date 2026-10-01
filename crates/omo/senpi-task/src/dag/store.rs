//! `dag/store.ts`: crash-safe DAG persistence (event WAL, checkpoints, keys, results, locks, GC).
// allow: SIZE_OK - crash-safe DAG persistence is kept in one module so WAL, checkpoint, lock, and GC invariants share one filesystem boundary.
//!
//! Filesystem primitives that the TS tests intercept through `spyOn(fs, ...)` (exclusive create,
//! fsync, rename, hard link) go through the injectable [`DagFs`] seam.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::TimeZone;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::dag::types::{
    DAG_SETTINGS_DEFAULTS, DagEventLane, DagRunEvent, DagRunEventType, DagRunId, SchemaVersion1,
};

const LOCK_RETRY_MS: u64 = 10;
const LOCK_WAIT_TIMEOUT_MS: i64 = 1_000;
const READ_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const TERMINAL_STATUSES: [&str; 3] = ["completed", "failed", "cancelled"];
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Default task state directory, relative to the project directory.
pub const DEFAULT_STATE_DIR: &str = ".senpi/tasks";

pub type DagNowFn = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type IsProcessAliveFn = Arc<dyn Fn(i64) -> bool + Send + Sync>;

/// `Partial<DagSettings>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DagSettingsOverrides {
    pub max_nodes_per_run: Option<usize>,
    pub max_runs_per_session: Option<usize>,
    pub subscriber_ring: Option<usize>,
    pub heartbeat_ms: Option<u64>,
    pub history_default_limit: Option<usize>,
    pub history_max_limit: Option<usize>,
    pub retention_days: Option<u64>,
    pub max_prompt_bytes: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DagStoreTaskConfig {
    pub state_dir: Option<PathBuf>,
    pub dag: Option<DagSettingsOverrides>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagStoreConfig {
    pub project_dir: PathBuf,
    pub task: Option<DagStoreTaskConfig>,
}

impl DagStoreConfig {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.into(),
            task: None,
        }
    }

    fn dag(&self) -> Option<&DagSettingsOverrides> {
        self.task.as_ref().and_then(|task| task.dag.as_ref())
    }
}

/// Resolves the task state directory: explicit `task.state_dir` (relative to the project) or the
/// default project-local directory.
pub fn resolve_dag_state_dir(config: &DagStoreConfig) -> PathBuf {
    match config.task.as_ref().and_then(|task| task.state_dir.as_ref()) {
        Some(dir) if dir.is_absolute() => dir.clone(),
        Some(dir) => config.project_dir.join(dir),
        None => config.project_dir.join(DEFAULT_STATE_DIR),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DagStoreDiagnostic {
    #[serde(rename_all = "camelCase")]
    EventLogRecovered {
        run_id: DagRunId,
        path: String,
        message: String,
        at: String,
    },
    #[serde(rename_all = "camelCase")]
    JournalCorrupt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<DagRunId>,
        path: String,
        message: String,
        at: String,
    },
}

impl DagStoreDiagnostic {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::EventLogRecovered { .. } => "event_log_recovered",
            Self::JournalCorrupt { .. } => "journal_corrupt",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::EventLogRecovered { message, .. } | Self::JournalCorrupt { message, .. } => {
                message
            }
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::EventLogRecovered { run_id, .. } => Some(run_id),
            Self::JournalCorrupt { run_id, .. } => run_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DagEventReadOptions {
    pub limit: usize,
    pub lane: Option<DagEventLane>,
    pub types: Option<Vec<DagRunEventType>>,
    pub through_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DagEventPage {
    pub events: Vec<DagRunEvent>,
    pub next_since_seq: u64,
    pub head_seq: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagKeyRecord {
    pub schema_version: SchemaVersion1,
    pub parent_session_id: String,
    pub run_key: String,
    pub run_id: DagRunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_fingerprint: Option<String>,
}

/// `DagJournalCorruptError`: a fail-closed persistence error carrying its diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagJournalCorruptError {
    pub diagnostic: Box<DagStoreDiagnostic>,
}

impl DagJournalCorruptError {
    pub const NAME: &'static str = "DagJournalCorruptError";

    pub fn new(diagnostic: DagStoreDiagnostic) -> Self {
        Self {
            diagnostic: Box::new(diagnostic),
        }
    }
}

impl fmt::Display for DagJournalCorruptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic.message())
    }
}

impl std::error::Error for DagJournalCorruptError {}

#[derive(Debug, thiserror::Error)]
pub enum DagStoreError {
    #[error(transparent)]
    JournalCorrupt(#[from] DagJournalCorruptError),
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// The filesystem primitives the store's crash-safety protocol is built from.
pub trait DagFs: Send + Sync {
    /// `openSync(path, "wx")`.
    fn create_new(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new().write(true).create_new(true).open(path)
    }

    fn fsync(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn hard_link(&self, existing: &Path, new: &Path) -> io::Result<()> {
        std::fs::hard_link(existing, new)
    }
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealDagFs;

impl DagFs for RealDagFs {}

#[derive(Clone, Default)]
pub struct DagStoreOptions {
    pub now: Option<DagNowFn>,
    pub is_process_alive: Option<IsProcessAliveFn>,
    /// A Node platform name (`"linux"`, `"darwin"`, `"win32"`, ...).
    pub platform: Option<String>,
    /// Ordering-focused tests can disable fsync without bypassing real filesystem writes.
    pub fsync: Option<bool>,
    pub fs: Option<Arc<dyn DagFs>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagStorePaths {
    pub root: PathBuf,
    pub keys: PathBuf,
    pub runs: PathBuf,
    pub events: PathBuf,
    pub results: PathBuf,
    pub locks: PathBuf,
}

impl DagStorePaths {
    pub fn new(state_dir: &Path) -> Self {
        let root = state_dir.join("dag");
        Self {
            keys: root.join("keys"),
            runs: root.join("runs"),
            events: root.join("events"),
            results: root.join("results"),
            locks: root.join("locks"),
            root,
        }
    }

    pub fn key(&self, parent_session_id: &str, run_key: &str) -> PathBuf {
        self.keys
            .join(format!("{}.json", dag_key_hash(parent_session_id, run_key)))
    }

    pub fn run(&self, run_id: &str) -> PathBuf {
        self.runs.join(format!("{run_id}.json"))
    }

    pub fn event(&self, run_id: &str) -> PathBuf {
        self.events.join(format!("{run_id}.jsonl"))
    }

    pub fn result(&self, run_id: &str, node_id: &str) -> PathBuf {
        self.results.join(run_id).join(format!("{node_id}.txt"))
    }

    pub fn run_lock(&self, run_id: &str) -> PathBuf {
        self.locks.join(format!("{run_id}.lock"))
    }

    pub fn key_lock(&self, parent_session_id: &str, run_key: &str) -> PathBuf {
        self.locks.join(format!(
            "key-{}.lock",
            dag_key_hash(parent_session_id, run_key)
        ))
    }

    pub fn task_owner_lock(&self, task_owner: &str) -> PathBuf {
        self.locks
            .join(format!("task-owner-{}.lock", sha256(task_owner)))
    }
}

pub fn dag_key_hash(parent_session_id: &str, run_key: &str) -> String {
    sha256(&format!("{parent_session_id}\0{run_key}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockHolder {
    pid: Option<i64>,
    content: String,
}

/// Test-only seam mirroring the TS suite's `{ ...store, writeCheckpoint: () => { throw } }` object
/// spread: lets a test make the next checkpoint write fail once without a real I/O fault.
#[cfg(test)]
type CheckpointHook = Arc<dyn Fn(&Value) -> Result<(), DagStoreError> + Send + Sync>;
#[cfg(test)]
type AppendHook = Arc<dyn Fn(&DagRunEvent) -> Result<(), DagStoreError> + Send + Sync>;

pub struct DagFileStore {
    pub state_dir: PathBuf,
    pub paths: DagStorePaths,
    diagnostic_log: Mutex<Vec<DagStoreDiagnostic>>,
    recovered_paths: Mutex<HashSet<PathBuf>>,
    now: DagNowFn,
    is_process_alive: IsProcessAliveFn,
    platform: String,
    fsync_writes: bool,
    fs: Arc<dyn DagFs>,
    max_runs_per_session: usize,
    retention_days: u64,
    #[cfg(test)]
    checkpoint_hook: Mutex<Option<CheckpointHook>>,
    #[cfg(test)]
    append_hook: Mutex<Option<AppendHook>>,
}

pub fn create_dag_file_store(
    config: &DagStoreConfig,
    options: DagStoreOptions,
) -> Result<DagFileStore, DagStoreError> {
    let state_dir = resolve_dag_state_dir(config);
    let paths = DagStorePaths::new(&state_dir);
    let dag = config.dag();
    let store = DagFileStore {
        state_dir,
        diagnostic_log: Mutex::new(Vec::new()),
        recovered_paths: Mutex::new(HashSet::new()),
        now: options.now.unwrap_or_else(|| Arc::new(system_now)),
        is_process_alive: options
            .is_process_alive
            .unwrap_or_else(|| Arc::new(default_is_process_alive)),
        platform: options.platform.unwrap_or_else(default_platform),
        fsync_writes: options.fsync.unwrap_or(true),
        fs: options.fs.unwrap_or_else(|| Arc::new(RealDagFs)),
        max_runs_per_session: dag
            .and_then(|dag| dag.max_runs_per_session)
            .unwrap_or(DAG_SETTINGS_DEFAULTS.max_runs_per_session),
        retention_days: dag
            .and_then(|dag| dag.retention_days)
            .unwrap_or(DAG_SETTINGS_DEFAULTS.retention_days),
        paths,
        #[cfg(test)]
        checkpoint_hook: Mutex::new(None),
        #[cfg(test)]
        append_hook: Mutex::new(None),
    };
    for directory in [
        &store.paths.keys,
        &store.paths.runs,
        &store.paths.events,
        &store.paths.results,
        &store.paths.locks,
    ] {
        fs::create_dir_all(directory)?;
    }
    store.inspect_existing_event_logs()?;
    Ok(store)
}

impl DagFileStore {
    #[cfg(test)]
    pub(crate) fn set_checkpoint_hook(&self, hook: CheckpointHook) {
        *guard(&self.checkpoint_hook) = Some(hook);
    }

    #[cfg(test)]
    pub(crate) fn set_append_hook(&self, hook: AppendHook) {
        *guard(&self.append_hook) = Some(hook);
    }

    fn clock(&self) -> &dyn Fn() -> i64 {
        &*self.now
    }

    fn alive(&self, pid: i64) -> bool {
        (self.is_process_alive)(pid)
    }

    pub fn diagnostics(&self) -> Vec<DagStoreDiagnostic> {
        guard(&self.diagnostic_log).clone()
    }

    pub fn append_event(&self, event: &DagRunEvent) -> Result<(), DagStoreError> {
        assert_safe_segment(&event.run_id, "run id")?;
        let path = self.paths.event(&event.run_id);
        let value = serde_json::to_value(event)?;
        assert_supported_schema(&value, &path, Some(&event.run_id), self.clock())?;
        let tail = self.event_log_tail_seq(&path, &event.run_id)?;
        let expected = tail.as_u64().and_then(|seq| seq.checked_add(1));
        if event.seq > MAX_SAFE_INTEGER || expected != Some(event.seq) {
            let expected_text = expected.map_or_else(
                || (tail.as_f64().unwrap_or(f64::NAN) + 1.0).to_string(),
                |seq| seq.to_string(),
            );
            return Err(DagStoreError::Message(format!(
                "DAG event seq must be strictly increasing: expected {expected_text}, received {}",
                event.seq
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
        file.write_all(format!("{value}\n").as_bytes())?;
        if self.fsync_writes {
            self.fs.fsync(&file)?;
        }
        #[cfg(test)]
        if let Some(hook) = guard(&self.append_hook).clone() {
            hook(event)?;
        }
        Ok(())
    }

    pub fn read_events(
        &self,
        run_id: &str,
        since_seq_exclusive: u64,
        options: &DagEventReadOptions,
    ) -> Result<DagEventPage, DagStoreError> {
        assert_safe_segment(run_id, "run id")?;
        if options.limit == 0 {
            return Err(DagStoreError::Message(
                "event page limit must be a positive integer".to_string(),
            ));
        }
        let path = self.paths.event(run_id);
        if !path.exists() {
            return Ok(DagEventPage {
                events: Vec::new(),
                next_since_seq: since_seq_exclusive,
                head_seq: 0,
                has_more: false,
            });
        }
        self.inspect_event_log(&path, run_id)?;
        let mut events: Vec<DagRunEvent> = Vec::new();
        let mut head_seq = 0;
        let mut has_more = false;
        for_each_json_line(&path, |value| {
            let event = parse_event(value, &path, run_id, self.clock())?;
            head_seq = head_seq.max(event.seq);
            if event.seq <= since_seq_exclusive {
                return Ok(());
            }
            if let Some(through) = options.through_seq
                && event.seq > through
            {
                return Ok(());
            }
            if let Some(lane) = options.lane
                && event.lane != lane
            {
                return Ok(());
            }
            if let Some(types) = &options.types
                && !types.contains(&event.payload.event_type())
            {
                return Ok(());
            }
            if events.len() < options.limit {
                events.push(event);
            } else {
                has_more = true;
            }
            Ok(())
        })?;
        let next_since_seq = events.last().map_or(since_seq_exclusive, |event| event.seq);
        Ok(DagEventPage {
            events,
            next_since_seq,
            head_seq,
            has_more,
        })
    }

    pub fn write_checkpoint<C: Serialize + ?Sized>(
        &self,
        run_id: &str,
        checkpoint: &C,
    ) -> Result<(), DagStoreError> {
        assert_safe_segment(run_id, "run id")?;
        let value = serde_json::to_value(checkpoint)?;
        #[cfg(test)]
        if let Some(hook) = guard(&self.checkpoint_hook).clone() {
            hook(&value)?;
        }
        assert_supported_schema(&value, &self.paths.run(run_id), Some(run_id), self.clock())?;
        self.write_checkpoint_within_session_limit(run_id, &value)
    }

    pub fn read_checkpoint<T: DeserializeOwned>(
        &self,
        run_id: &str,
    ) -> Result<Option<T>, DagStoreError> {
        assert_safe_segment(run_id, "run id")?;
        let path = self.paths.run(run_id);
        let Some(value) = read_json_file(&path, Some(run_id), self.clock())? else {
            return Ok(None);
        };
        assert_supported_schema(&value, &path, Some(run_id), self.clock())?;
        Ok(Some(serde_json::from_value(value)?))
    }

    pub fn write_key(&self, record: &DagKeyRecord) -> Result<PathBuf, DagStoreError> {
        let path = self.paths.key(&record.parent_session_id, &record.run_key);
        let value = serde_json::to_value(record)?;
        assert_supported_schema(&value, &path, Some(&record.run_id), self.clock())?;
        self.write_json_atomic(&path, &value)?;
        Ok(path)
    }

    pub fn read_key(
        &self,
        parent_session_id: &str,
        run_key: &str,
    ) -> Result<Option<DagKeyRecord>, DagStoreError> {
        let path = self.paths.key(parent_session_id, run_key);
        let Some(value) = read_json_file(&path, None, &system_now)? else {
            return Ok(None);
        };
        let run_id = read_optional_string(&value, "runId");
        assert_supported_schema(&value, &path, run_id.as_deref(), self.clock())?;
        Ok(Some(serde_json::from_value(value)?))
    }

    pub fn write_result(
        &self,
        run_id: &str,
        node_id: &str,
        result: &str,
    ) -> Result<PathBuf, DagStoreError> {
        assert_safe_segment(run_id, "run id")?;
        assert_safe_segment(node_id, "node id")?;
        let path = self.paths.result(run_id, node_id);
        self.write_file_atomic(&path, result)?;
        Ok(path)
    }

    pub fn read_result(&self, run_id: &str, node_id: &str) -> Result<Option<String>, DagStoreError> {
        assert_safe_segment(run_id, "run id")?;
        assert_safe_segment(node_id, "node id")?;
        match fs::read(self.paths.result(run_id, node_id)) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn with_run_lock<T>(
        &self,
        run_id: &str,
        operation: impl FnOnce() -> T,
    ) -> Result<T, DagStoreError> {
        assert_safe_segment(run_id, "run id")?;
        self.with_lock(&self.paths.run_lock(run_id), Some(run_id), operation)
    }

    pub fn with_key_lock<T>(
        &self,
        parent_session_id: &str,
        run_key: &str,
        operation: impl FnOnce() -> T,
    ) -> Result<T, DagStoreError> {
        self.with_lock(
            &self.paths.key_lock(parent_session_id, run_key),
            None,
            operation,
        )
    }

    pub fn with_task_owner_lock<T>(
        &self,
        task_owner: &str,
        run_id: &str,
        operation: impl FnOnce() -> T,
    ) -> Result<T, DagStoreError> {
        self.with_lock(&self.paths.task_owner_lock(task_owner), Some(run_id), operation)
    }

    pub fn prune_expired(&self, prune_now: Option<i64>) -> Result<Vec<DagRunId>, DagStoreError> {
        let prune_now = prune_now.unwrap_or_else(|| (self.now)());
        let retention_ms = i64::try_from(self.retention_days)
            .unwrap_or(i64::MAX)
            .saturating_mul(DAY_MS);
        let cutoff = prune_now.saturating_sub(retention_ms);
        let mut pruned = Vec::new();
        for name in list_files(&self.paths.runs, ".json")? {
            let stem = name.strip_suffix(".json").unwrap_or(&name);
            let path = self.paths.runs.join(&name);
            let Some(checkpoint) = read_json_file(&path, Some(stem), self.clock())? else {
                continue;
            };
            let run_id = read_optional_string(&checkpoint, "runId").unwrap_or_else(|| stem.to_string());
            assert_supported_schema(&checkpoint, &path, Some(&run_id), self.clock())?;
            let terminal = read_optional_string(&checkpoint, "status")
                .is_some_and(|status| TERMINAL_STATUSES.contains(&status.as_str()));
            if !terminal {
                continue;
            }
            let terminal_at = read_optional_string(&checkpoint, "completedAt")
                .or_else(|| read_optional_string(&checkpoint, "updatedAt"));
            let Some(terminal_at) = terminal_at else {
                continue;
            };
            // `Date.parse` of an unparsable timestamp is NaN, which never compares greater.
            if let Some(terminal_ms) = parse_date_ms(&terminal_at)
                && terminal_ms > cutoff
            {
                continue;
            }
            self.prune_run_artifacts(&checkpoint, &run_id)?;
            pruned.push(run_id);
        }
        Ok(pruned)
    }

    fn write_checkpoint_within_session_limit(
        &self,
        run_id: &str,
        checkpoint: &Value,
    ) -> Result<(), DagStoreError> {
        let path = self.paths.run(run_id);
        let parent_session_id = match read_optional_string(checkpoint, "parentSessionId") {
            Some(parent) if !path.exists() => parent,
            _ => return self.write_json_atomic(&path, checkpoint),
        };
        let capacity_lock = self
            .paths
            .locks
            .join(format!("session-runs-{}.lock", sha256(&parent_session_id)));
        self.with_lock(&capacity_lock, None, || -> Result<(), DagStoreError> {
            if path.exists() {
                return self.write_json_atomic(&path, checkpoint);
            }
            let mut run_count = 0usize;
            for name in list_files(&self.paths.runs, ".json")? {
                let existing_run_id = name.strip_suffix(".json").unwrap_or(&name);
                let existing_path = self.paths.runs.join(&name);
                let Some(existing) =
                    read_json_file(&existing_path, Some(existing_run_id), self.clock())?
                else {
                    continue;
                };
                assert_supported_schema(&existing, &existing_path, Some(existing_run_id), self.clock())?;
                if read_optional_string(&existing, "parentSessionId").as_deref()
                    == Some(parent_session_id.as_str())
                {
                    run_count += 1;
                }
            }
            if run_count >= self.max_runs_per_session {
                remove_force(&self.paths.root.join("skills").join(format!("{run_id}.json")))?;
                return Err(DagStoreError::Message(format!(
                    "DAG session run limit reached: {}",
                    self.max_runs_per_session
                )));
            }
            self.write_json_atomic(&path, checkpoint)
        })?
    }

    fn write_json_atomic(&self, path: &Path, value: &Value) -> Result<(), DagStoreError> {
        self.write_file_atomic(path, &value.to_string())
    }

    fn write_file_atomic(&self, path: &Path, content: &str) -> Result<(), DagStoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = suffixed(path, &format!(".{}.{}.tmp", host_pid(), random_uuid()));
        let result = self.publish_atomic(&tmp_path, path, content);
        remove_force(&tmp_path)?;
        result
    }

    fn publish_atomic(&self, tmp_path: &Path, path: &Path, content: &str) -> Result<(), DagStoreError> {
        let mut file = self.fs.create_new(tmp_path)?;
        file.write_all(content.as_bytes())?;
        if self.fsync_writes {
            self.fs.fsync(&file)?;
        }
        drop(file);
        self.fs.rename(tmp_path, path)?;
        if self.fsync_writes {
            self.fsync_parent_directory_after_rename(path)?;
        }
        Ok(())
    }

    fn fsync_parent_directory_after_rename(&self, path: &Path) -> Result<(), DagStoreError> {
        // Directory fsync makes the rename durable on POSIX. Windows rejects fsync on directory
        // handles with EPERM, and its MoveFileEx-backed rename uses different durability semantics.
        if self.platform == "win32" {
            return Ok(());
        }
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        let directory = File::open(parent)?;
        self.fs.fsync(&directory)?;
        Ok(())
    }

    fn event_log_tail_seq(&self, path: &Path, run_id: &str) -> Result<serde_json::Number, DagStoreError> {
        let mut tail = serde_json::Number::from(0u64);
        for_each_json_line(path, |value| {
            tail = parse_event_envelope(&value, path, run_id, self.clock())?;
            Ok(())
        })?;
        Ok(tail)
    }

    fn inspect_existing_event_logs(&self) -> Result<(), DagStoreError> {
        for name in list_files(&self.paths.events, ".jsonl")? {
            let run_id = name.strip_suffix(".jsonl").unwrap_or(&name);
            self.inspect_event_log(&self.paths.events.join(&name), run_id)?;
        }
        Ok(())
    }

    fn inspect_event_log(&self, path: &Path, run_id: &str) -> Result<(), DagStoreError> {
        let mut last_complete_offset = 0u64;
        let mut trailing: Option<Vec<u8>> = None;
        for_each_raw_line(path, |line, end_offset, complete| {
            if !complete {
                trailing = Some(line.to_vec());
                return Ok(());
            }
            parse_event_json(&String::from_utf8_lossy(line), path, run_id, self.clock())?;
            last_complete_offset = end_offset;
            Ok(())
        })?;
        let Some(trailing) = trailing.filter(|fragment| !fragment.is_empty()) else {
            return Ok(());
        };
        match parse_event_json(&String::from_utf8_lossy(&trailing), path, run_id, self.clock()) {
            Ok(()) => Ok(()),
            Err(error) if error.diagnostic.message().contains("schemaVersion") => Err(error.into()),
            Err(_) => {
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(last_complete_offset)?;
                if guard(&self.recovered_paths).insert(path.to_path_buf()) {
                    guard(&self.diagnostic_log).push(DagStoreDiagnostic::EventLogRecovered {
                        run_id: run_id.to_string(),
                        path: path_string(path),
                        message: "discarded malformed trailing JSONL fragment".to_string(),
                        at: iso((self.now)()),
                    });
                }
                Ok(())
            }
        }
    }

    fn lock_content(&self, run_id: Option<&str>) -> String {
        let mut content = Map::new();
        content.insert("hostPid".to_string(), Value::from(host_pid()));
        if let Some(run_id) = run_id {
            content.insert("runId".to_string(), Value::from(run_id));
        }
        content.insert("token".to_string(), Value::from(random_uuid()));
        content.insert("createdAt".to_string(), Value::from(iso((self.now)())));
        Value::Object(content).to_string()
    }

    fn with_lock<T>(
        &self,
        path: &Path,
        run_id: Option<&str>,
        operation: impl FnOnce() -> T,
    ) -> Result<T, DagStoreError> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_safe_segment(&name, "lock name")?;
        let started_at = (self.now)();
        let acquired = loop {
            let content = self.lock_content(run_id);
            if self.try_create_lock(path, &content)? {
                break LockHolder {
                    pid: Some(host_pid()),
                    content,
                };
            }
            let Some(observed) = read_lock_holder(path)? else {
                continue;
            };
            if observed.pid.is_none_or(|pid| !self.alive(pid))
                && let Some(reclaimed) = self.reclaim_observed_lock(path, &observed, run_id)?
            {
                break reclaimed;
            }
            if (self.now)() - started_at >= LOCK_WAIT_TIMEOUT_MS {
                return Err(DagStoreError::Message(format!(
                    "Timed out acquiring DAG lock: {}",
                    path.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(LOCK_RETRY_MS));
        };
        let result = operation();
        self.remove_observed_lock(path, Some(&acquired), |_| true)?;
        Ok(result)
    }

    fn try_create_lock(&self, path: &Path, content: &str) -> Result<bool, DagStoreError> {
        let temporary_path = suffixed(path, &format!(".{}.{}.tmp", host_pid(), random_uuid()));
        let result = self.link_new_file(&temporary_path, path, content);
        remove_force(&temporary_path)?;
        match result {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn link_new_file(&self, temporary_path: &Path, path: &Path, content: &str) -> io::Result<()> {
        let mut file = self.fs.create_new(temporary_path)?;
        file.write_all(content.as_bytes())?;
        if self.fsync_writes {
            self.fs.fsync(&file)?;
        }
        drop(file);
        self.fs.hard_link(temporary_path, path)
    }

    fn reclaim_observed_lock(
        &self,
        path: &Path,
        observed: &LockHolder,
        run_id: Option<&str>,
    ) -> Result<Option<LockHolder>, DagStoreError> {
        let reclaim_path = suffixed(path, ".reclaim");
        let Some(reclaim_holder) = self.try_acquire_reclaim_mutex(&reclaim_path)? else {
            return Ok(None);
        };
        let content = self.lock_content(run_id);
        let successor_path =
            suffixed(path, &format!(".{}.{}.successor", host_pid(), random_uuid()));
        let result = self.publish_successor(path, observed, &successor_path, content);
        remove_force(&successor_path)?;
        self.remove_observed_lock(&reclaim_path, Some(&reclaim_holder), |_| true)?;
        result
    }

    fn publish_successor(
        &self,
        path: &Path,
        observed: &LockHolder,
        successor_path: &Path,
        content: String,
    ) -> Result<Option<LockHolder>, DagStoreError> {
        {
            let mut file = self.fs.create_new(successor_path)?;
            file.write_all(content.as_bytes())?;
            if self.fsync_writes {
                self.fs.fsync(&file)?;
            }
        }
        let Some(current) = read_lock_holder(path)? else {
            return Ok(None);
        };
        if &current != observed || current.pid.is_some_and(|pid| self.alive(pid)) {
            return Ok(None);
        }
        self.fs.rename(successor_path, path)?;
        Ok(Some(LockHolder {
            pid: Some(host_pid()),
            content,
        }))
    }

    fn try_acquire_reclaim_mutex(&self, path: &Path) -> Result<Option<LockHolder>, DagStoreError> {
        let content = self.lock_content(None);
        if self.try_create_lock(path, &content)? {
            return Ok(Some(LockHolder {
                pid: Some(host_pid()),
                content,
            }));
        }
        if let Some(observed) = read_lock_holder(path)?
            && observed.pid.is_none_or(|pid| !self.alive(pid))
        {
            self.remove_observed_lock(path, Some(&observed), |moved| {
                moved.is_none_or(|holder| holder.pid.is_none_or(|pid| !self.alive(pid)))
            })?;
        }
        Ok(None)
    }

    fn remove_observed_lock(
        &self,
        path: &Path,
        observed: Option<&LockHolder>,
        can_remove: impl Fn(Option<&LockHolder>) -> bool,
    ) -> Result<bool, DagStoreError> {
        if observed != read_lock_holder(path)?.as_ref() {
            return Ok(false);
        }
        let quarantine_path = suffixed(path, &format!(".{}.{}.stale", host_pid(), random_uuid()));
        match self.fs.rename(path, &quarantine_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        let moved = read_lock_holder(&quarantine_path)?;
        if observed != moved.as_ref() || !can_remove(moved.as_ref()) {
            self.restore_quarantined_lock(path, &quarantine_path)?;
            return Ok(false);
        }
        remove_force(&quarantine_path)?;
        Ok(true)
    }

    fn restore_quarantined_lock(&self, path: &Path, quarantine_path: &Path) -> Result<(), DagStoreError> {
        match self.fs.hard_link(quarantine_path, path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        remove_force(quarantine_path)?;
        Ok(())
    }

    fn prune_run_artifacts(&self, checkpoint: &Value, run_id: &str) -> Result<(), DagStoreError> {
        remove_force(&self.paths.event(run_id))?;
        remove_dir_force(&self.paths.results.join(run_id))?;
        remove_force(&self.paths.root.join("skills").join(format!("{run_id}.json")))?;
        remove_force(&self.paths.run_lock(run_id))?;
        for name in list_files(&self.paths.keys, ".json")? {
            let key_path = self.paths.keys.join(&name);
            let value = read_json_file(&key_path, None, &system_now)?;
            if value
                .as_ref()
                .and_then(|value| value.get("runId"))
                .and_then(Value::as_str)
                != Some(run_id)
            {
                continue;
            }
            remove_force(&key_path)?;
            let stem = name.strip_suffix(".json").unwrap_or(&name);
            remove_force(&self.paths.locks.join(format!("key-{stem}.lock")))?;
        }
        if let Some(nodes) = checkpoint.get("nodes").and_then(Value::as_array) {
            for node in nodes {
                if let Some(task_id) = node.get("taskId").and_then(Value::as_str) {
                    remove_force(&self.paths.task_owner_lock(task_id))?;
                }
            }
        }
        for name in list_files(&self.paths.locks, ".lock")? {
            let lock_path = self.paths.locks.join(&name);
            match fs::read(&lock_path) {
                Ok(bytes) => {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                        && value.get("runId").and_then(Value::as_str) == Some(run_id)
                    {
                        remove_force(&lock_path)?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        remove_force(&self.paths.run(run_id))?;
        Ok(())
    }
}

fn read_lock_holder(path: &Path) -> Result<Option<LockHolder>, DagStoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let content = String::from_utf8_lossy(&bytes).into_owned();
    if let Ok(value) = serde_json::from_str::<Value>(&content)
        && let Some(host_pid) = value.get("hostPid").filter(|pid| pid.is_number())
    {
        let pid = host_pid
            .as_i64()
            .unwrap_or_else(|| host_pid.as_f64().map_or(0, |pid| pid as i64));
        return Ok(Some(LockHolder {
            pid: Some(pid),
            content,
        }));
    }
    let first_line = content.split('\n').next().unwrap_or_default();
    let pid = js_number(first_line)
        .filter(|pid| pid.is_finite() && pid.fract() == 0.0 && *pid > 0.0)
        .map(|pid| pid as i64);
    Ok(Some(LockHolder { pid, content }))
}

/// `Number(text)` for the plain decimal forms a pid line can take.
fn js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok()
}

fn for_each_raw_line(
    path: &Path,
    mut visit: impl FnMut(&[u8], u64, bool) -> Result<(), DagStoreError>,
) -> Result<(), DagStoreError> {
    if !path.exists() {
        return Ok(());
    }
    let mut reader = BufReader::with_capacity(READ_BUFFER_BYTES, File::open(path)?);
    let mut offset = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes_read = reader.read_until(b'\n', &mut line)?;
        if bytes_read == 0 {
            break;
        }
        offset += bytes_read as u64;
        match line.split_last() {
            Some((b'\n', body)) => visit(body, offset, true)?,
            _ => visit(&line, offset, false)?,
        }
    }
    Ok(())
}

fn for_each_json_line(
    path: &Path,
    mut visit: impl FnMut(Value) -> Result<(), DagStoreError>,
) -> Result<(), DagStoreError> {
    for_each_raw_line(path, |line, _end_offset, complete| {
        if !complete && line.is_empty() {
            return Ok(());
        }
        visit(parse_json(&String::from_utf8_lossy(line), path, None, &system_now)?)
    })
}

fn parse_event_json(
    text: &str,
    path: &Path,
    run_id: &str,
    now: &dyn Fn() -> i64,
) -> Result<(), DagJournalCorruptError> {
    let value = parse_json(text, path, Some(run_id), now)?;
    parse_event_envelope(&value, path, run_id, now).map(|_| ())
}

fn parse_event_envelope(
    value: &Value,
    path: &Path,
    run_id: &str,
    now: &dyn Fn() -> i64,
) -> Result<serde_json::Number, DagJournalCorruptError> {
    assert_supported_schema(value, path, Some(run_id), now)?;
    let seq = match value.get("seq") {
        Some(Value::Number(seq)) => Some(seq.clone()),
        _ => None,
    };
    let has_type = value.get("type").is_some_and(Value::is_string);
    match seq {
        Some(seq) if has_type => Ok(seq),
        _ => Err(corrupt(path, Some(run_id), "invalid DAG event envelope", now)),
    }
}

fn parse_event(
    value: Value,
    path: &Path,
    run_id: &str,
    now: &dyn Fn() -> i64,
) -> Result<DagRunEvent, DagJournalCorruptError> {
    parse_event_envelope(&value, path, run_id, now)?;
    serde_json::from_value(value)
        .map_err(|_| corrupt(path, Some(run_id), "invalid DAG event envelope", now))
}

fn parse_json(
    text: &str,
    path: &Path,
    run_id: Option<&str>,
    now: &dyn Fn() -> i64,
) -> Result<Value, DagJournalCorruptError> {
    serde_json::from_str(text)
        .map_err(|error| corrupt(path, run_id, format!("malformed JSON: {error}"), now))
}

fn read_json_file(
    path: &Path,
    run_id: Option<&str>,
    now: &dyn Fn() -> i64,
) -> Result<Option<Value>, DagStoreError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(parse_json(&String::from_utf8_lossy(&bytes), path, run_id, now)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn assert_supported_schema(
    value: &Value,
    path: &Path,
    run_id: Option<&str>,
    now: &dyn Fn() -> i64,
) -> Result<(), DagJournalCorruptError> {
    let Some(version) = value
        .as_object()
        .and_then(|record| record.get("schemaVersion"))
        .filter(|version| version.is_number())
    else {
        return Err(corrupt(path, run_id, "missing schemaVersion", now));
    };
    if version.as_u64() != Some(1) && version.as_f64() != Some(1.0) {
        return Err(corrupt(
            path,
            run_id,
            format!("unsupported schemaVersion {version}"),
            now,
        ));
    }
    Ok(())
}

fn corrupt(
    path: &Path,
    run_id: Option<&str>,
    message: impl Into<String>,
    now: &dyn Fn() -> i64,
) -> DagJournalCorruptError {
    DagJournalCorruptError::new(DagStoreDiagnostic::JournalCorrupt {
        run_id: run_id.map(str::to_string),
        path: path_string(path),
        message: message.into(),
        at: iso(now()),
    })
}

fn assert_safe_segment(value: &str, label: &str) -> Result<(), DagStoreError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(DagStoreError::Message(format!("Invalid {label}")));
    }
    Ok(())
}

fn read_optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|record| record.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Regular-file names in `directory` ending with `suffix`, sorted for deterministic iteration.
fn list_files(directory: &Path, suffix: &str) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(suffix) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn remove_force(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn remove_dir_force(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn host_pid() -> i64 {
    i64::from(std::process::id())
}

fn system_now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `new Date(ms).toISOString()`.
fn iso(ms: i64) -> String {
    chrono::Utc
        .timestamp_millis_opt(ms)
        .single()
        .map_or_else(
            || "1970-01-01T00:00:00.000Z".to_string(),
            |at| at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        )
}

/// `Date.parse` for ISO timestamps; `None` stands for NaN.
fn parse_date_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.timestamp_millis())
}

fn default_platform() -> String {
    match std::env::consts::OS {
        "windows" => "win32".to_string(),
        "macos" => "darwin".to_string(),
        other => other.to_string(),
    }
}

/// Signal-0 liveness probe (`defaultSignaller.isAlive` in `lifecycle/context.ts`), routed
/// through the shared allowlisted killer so this module never invokes `libc::kill` itself.
pub fn default_is_process_alive(pid: i64) -> bool {
    u32::try_from(pid).is_ok_and(utils::process_sweep::default_is_process_alive)
}

fn sha256(value: &str) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A process-unique `randomUUID()`-shaped token.
fn random_uuid() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let digest = sha256(&format!("{}:{nanos}:{count}", std::process::id()));
    format!(
        "{}-{}-{}-{}-{}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
    )
}
