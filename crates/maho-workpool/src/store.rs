//! The `workpools/` directory of a task state dir (spec §8):
//!
//! - `workpools/<pool_id>.json` — one record per pool, written with temp-file + rename;
//! - `workpools/logs/<task_id>.jsonl` — each worker task's yield event log (append-only).
//!
//! Records are read from disk on every operation, so a corrupt record is reported as
//! `store_corrupt` the next time it is touched and never silently dropped.
use crate::model::{PoolError, PoolRecord, codes, validate_pool_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One accepted yield, as appended to the worker's event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YieldEvent {
    pub pool_id: String,
    pub generation: u64,
    pub task_id: String,
    pub run_epoch: u64,
    pub item_id: String,
    pub key: String,
    /// `{data}` or `{error: {code, message}}`.
    pub result: Value,
}

#[derive(Debug, Clone)]
pub struct PoolStore {
    dir: PathBuf,
}

fn corrupt(error: impl std::fmt::Display) -> PoolError {
    PoolError::new(codes::STORE_CORRUPT, error.to_string())
}

/// Task ids are file-name safe (`maho_task::ids::sanitize`); anything else is mapped to `_`.
fn file_safe(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

impl PoolStore {
    /// The store of a task state dir (`<state dir>/workpools`).
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self { dir: state_dir.join("workpools") }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn record_path(&self, pool_id: &str) -> PathBuf {
        self.dir.join(format!("{pool_id}.json"))
    }

    pub fn log_path(&self, task_id: &str) -> PathBuf {
        self.dir.join("logs").join(format!("{}.jsonl", file_safe(task_id)))
    }

    /// The record of `pool_id`; `Ok(None)` when no record exists.
    ///
    /// # Errors
    /// `invalid_pool_id` for a malformed id, `store_corrupt` for an unreadable or invalid record.
    pub fn load(&self, pool_id: &str) -> Result<Option<PoolRecord>, PoolError> {
        validate_pool_id(pool_id)?;
        let path = self.record_path(pool_id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(corrupt(format!("cannot read {}: {error}", path.display()))),
        };
        let record: PoolRecord = serde_json::from_slice(&bytes)
            .map_err(|error| corrupt(format!("invalid pool record {}: {error}", path.display())))?;
        if record.pool_id != pool_id {
            return Err(corrupt(format!("record {} names pool {}", path.display(), record.pool_id)));
        }
        Ok(Some(record))
    }

    /// Ids of every record file (well-formed names only).
    pub fn ids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut ids: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".json").map(str::to_owned)
            })
            .filter(|id| validate_pool_id(id).is_ok())
            .collect();
        ids.sort();
        ids
    }

    /// Every readable record; corrupt ones are returned as errors with their id.
    pub fn load_all(&self) -> Vec<(String, Result<PoolRecord, PoolError>)> {
        self.ids()
            .into_iter()
            .filter_map(|id| match self.load(&id) {
                Ok(Some(record)) => Some((id, Ok(record))),
                Ok(None) => None,
                Err(error) => Some((id, Err(error))),
            })
            .collect()
    }

    /// Writes `record` atomically (temp file + rename).
    ///
    /// # Errors
    /// Filesystem failures.
    pub fn save(&self, record: &PoolRecord) -> Result<(), PoolError> {
        std::fs::create_dir_all(&self.dir).map_err(corrupt)?;
        let path = self.record_path(&record.pool_id);
        let tmp = self.dir.join(format!(".{}.{}.tmp", record.pool_id, uuid::Uuid::new_v4().simple()));
        let bytes = serde_json::to_vec_pretty(record).map_err(corrupt)?;
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        });
        if let Err(error) = written.and_then(|()| std::fs::rename(&tmp, &path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(corrupt(format!("cannot write {}: {error}", path.display())));
        }
        Ok(())
    }

    /// Appends `event` to its worker's log and syncs it.
    ///
    /// # Errors
    /// Filesystem failures.
    pub fn append_event(&self, event: &YieldEvent) -> Result<(), PoolError> {
        let path = self.log_path(&event.task_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(corrupt)?;
        }
        let mut line = serde_json::to_vec(event).map_err(corrupt)?;
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| {
                file.write_all(&line)?;
                file.sync_all()
            })
            .map_err(|error| corrupt(format!("cannot append to {}: {error}", path.display())))
    }

    /// The events of `task_id`'s log; a torn last line (crash mid-append) is skipped.
    pub fn events(&self, task_id: &str) -> Vec<YieldEvent> {
        let Ok(text) = std::fs::read_to_string(self.log_path(task_id)) else { return Vec::new() };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}
