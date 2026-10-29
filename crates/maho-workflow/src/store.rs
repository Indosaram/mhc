//! The `workflows/` store (spec §6) under the task state directory:
//!
//! - `keys/<sha256(parent session, key)>.json` — key index `{key, parent_session, run_id}`;
//! - `runs/<run id>.json` — the run record, written atomically;
//! - `journal/<run id>.jsonl` — one JSON event per line, appended for every state change;
//! - `results/<run id>/<node id>.txt` — the node child's full final answer, plus
//!   `<node id>.stats.json`;
//! - `locks/<name>.lock` — start locks (create-new files holding the owner pid).
//!
//! Also the retention sweep (§3.8).
use crate::state::WorkflowRun;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEntry {
    pub key: String,
    pub parent_session: String,
    pub run_id: String,
}

/// Stats sidecar of one node result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultStats {
    pub bytes: usize,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub finished_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowStore {
    pub root: PathBuf,
}

/// Writes `bytes` to `path` through a temp file and a rename. On Windows the rename is retried a
/// few times because the target may be briefly locked by a reader.
///
/// # Errors
/// Filesystem errors.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temp, bytes)?;
    let attempts = if cfg!(windows) { 5 } else { 1 };
    let mut last = None;
    for attempt in 0..attempts {
        match std::fs::rename(&temp, path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(25 << attempt));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&temp);
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
}

/// A create-new lock file; removed on drop.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl WorkflowStore {
    /// The store of a task state directory (`<state dir>/workflows`).
    pub fn in_state_dir(state_root: &Path) -> Self {
        Self {
            root: state_root.join("workflows"),
        }
    }

    pub fn key_hash(parent_session: &str, key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(parent_session.as_bytes());
        hasher.update([0]);
        hasher.update(key.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn key_path(&self, parent_session: &str, key: &str) -> PathBuf {
        self.root
            .join("keys")
            .join(format!("{}.json", Self::key_hash(parent_session, key)))
    }
    pub fn record_path(&self, run_id: &str) -> PathBuf {
        self.root.join("runs").join(format!("{run_id}.json"))
    }
    pub fn journal_path(&self, run_id: &str) -> PathBuf {
        self.root.join("journal").join(format!("{run_id}.jsonl"))
    }
    pub fn results_dir(&self, run_id: &str) -> PathBuf {
        self.root.join("results").join(run_id)
    }
    pub fn result_path(&self, run_id: &str, node_id: &str) -> PathBuf {
        self.results_dir(run_id).join(format!("{node_id}.txt"))
    }

    pub fn lookup_key(&self, parent_session: &str, key: &str) -> Option<KeyEntry> {
        let bytes = std::fs::read(self.key_path(parent_session, key)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// # Errors
    /// Filesystem errors.
    pub fn save_key(&self, entry: &KeyEntry) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(entry).map_err(std::io::Error::other)?;
        write_atomic(&self.key_path(&entry.parent_session, &entry.key), &bytes)
    }

    /// # Errors
    /// Filesystem errors.
    pub fn save(&self, run: &WorkflowRun) -> std::io::Result<()> {
        if !valid_component(&run.run_id) {
            return Err(std::io::Error::other("invalid run id"));
        }
        let bytes = serde_json::to_vec_pretty(run).map_err(std::io::Error::other)?;
        write_atomic(&self.record_path(&run.run_id), &bytes)
    }

    pub fn load(&self, run_id: &str) -> Option<WorkflowRun> {
        if !valid_component(run_id) {
            return None;
        }
        let bytes = std::fs::read(self.record_path(run_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Every readable run record.
    pub fn list(&self) -> Vec<WorkflowRun> {
        let Ok(entries) = std::fs::read_dir(self.root.join("runs")) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".json")?;
                self.load(id)
            })
            .collect()
    }

    /// Appends one event line to the run's journal.
    ///
    /// # Errors
    /// Filesystem errors.
    pub fn append(&self, run_id: &str, event: &Value) -> std::io::Result<()> {
        let path = self.journal_path(run_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_vec(event).map_err(std::io::Error::other)?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(&line)
    }

    /// The last `limit` journal events and whether some line could not be parsed.
    pub fn history(&self, run_id: &str, limit: usize) -> (Vec<Value>, bool) {
        let Ok(text) = std::fs::read_to_string(self.journal_path(run_id)) else {
            return (Vec::new(), false);
        };
        let mut corrupt = false;
        let mut events: Vec<Value> = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str(line) {
                Ok(event) => events.push(event),
                Err(_) => corrupt = true,
            }
        }
        let skip = events.len().saturating_sub(limit);
        (events.split_off(skip), corrupt)
    }

    /// Stores a node's full final answer and its stats sidecar.
    ///
    /// # Errors
    /// Filesystem errors or an invalid node id.
    pub fn write_result(&self, run_id: &str, node_id: &str, text: &str, stats: &ResultStats) -> std::io::Result<()> {
        if !valid_component(run_id) || !valid_component(node_id) {
            return Err(std::io::Error::other("invalid run or node id"));
        }
        write_atomic(&self.result_path(run_id, node_id), text.as_bytes())?;
        let bytes = serde_json::to_vec_pretty(stats).map_err(std::io::Error::other)?;
        write_atomic(
            &self.results_dir(run_id).join(format!("{node_id}.stats.json")),
            &bytes,
        )
    }

    pub fn read_result(&self, run_id: &str, node_id: &str) -> Option<String> {
        if !valid_component(run_id) || !valid_component(node_id) {
            return None;
        }
        std::fs::read_to_string(self.result_path(run_id, node_id)).ok()
    }

    /// Takes the lock `name` (create-new file holding this pid). A lock older than `stale_after`
    /// is treated as abandoned and replaced. `None` when it is held.
    pub fn try_lock(&self, name: &str, stale_after: std::time::Duration) -> Option<FileLock> {
        let dir = self.root.join("locks");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{name}.lock"));
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = write!(file, "{}", std::process::id());
                    return Some(FileLock { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > stale_after);
                    if !stale {
                        return None;
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// Deletes one run: record, journal, results and its key entry.
    pub fn remove(&self, run: &WorkflowRun) -> Vec<String> {
        let mut errors = Vec::new();
        let mut attempt = |result: std::io::Result<()>, what: &str| {
            if let Err(error) = result
                && error.kind() != std::io::ErrorKind::NotFound
            {
                errors.push(format!("{what}: {error}"));
            }
        };
        attempt(std::fs::remove_file(self.record_path(&run.run_id)), "record");
        attempt(std::fs::remove_file(self.journal_path(&run.run_id)), "journal");
        attempt(std::fs::remove_dir_all(self.results_dir(&run.run_id)), "results");
        if self
            .lookup_key(&run.parent_session, &run.key)
            .is_some_and(|entry| entry.run_id == run.run_id)
        {
            attempt(
                std::fs::remove_file(self.key_path(&run.parent_session, &run.key)),
                "key",
            );
        }
        errors
    }

    /// Retention sweep: removes terminal runs that finished more than `retention_days` before
    /// `now_ms`. Runs that are not terminal are never removed. Returns the removed run ids and
    /// the failures (to be logged; never fatal).
    pub fn sweep(&self, retention_days: u64, now_ms: u64) -> (Vec<String>, Vec<String>) {
        let horizon = retention_days.saturating_mul(24 * 60 * 60 * 1000);
        let mut removed = Vec::new();
        let mut failures = Vec::new();
        for run in self.list() {
            if !run.status.is_terminal() {
                continue;
            }
            let finished = run.finished_at.unwrap_or(run.updated_at);
            if now_ms.saturating_sub(finished) <= horizon {
                continue;
            }
            let errors = self.remove(&run);
            if errors.is_empty() {
                removed.push(run.run_id.clone());
            } else {
                failures.extend(errors.into_iter().map(|e| format!("{}: {e}", run.run_id)));
            }
        }
        (removed, failures)
    }
}

/// A journal event.
pub fn event(kind: &str, node_id: Option<&str>, data: Value) -> Value {
    let mut value = json!({"at": crate::now_ms(), "event": kind});
    if let Some(node) = node_id {
        value["node_id"] = json!(node);
    }
    if !data.is_null() {
        value["data"] = data;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_and_lock() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = WorkflowStore::in_state_dir(dir.path());
        let path = store.root.join("x/y.json");
        write_atomic(&path, b"1").expect("write");
        write_atomic(&path, b"2").expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"2");
        let lock = store.try_lock("k", std::time::Duration::from_secs(60)).expect("lock");
        assert!(store.try_lock("k", std::time::Duration::from_secs(60)).is_none());
        drop(lock);
        assert!(store.try_lock("k", std::time::Duration::from_secs(60)).is_some());
    }

    #[test]
    fn journal_history_reports_corruption() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = WorkflowStore::in_state_dir(dir.path());
        for i in 0..5 {
            store.append("r", &json!({"n": i})).expect("append");
        }
        let (events, corrupt) = store.history("r", 2);
        assert_eq!(events, [json!({"n": 3}), json!({"n": 4})]);
        assert!(!corrupt);
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.journal_path("r"))
            .and_then(|mut f| f.write_all(b"{broken\n"))
            .expect("corrupt");
        assert!(store.history("r", 10).1);
    }
}
