//! Durable store for the facts failure ledger.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::identity::MemoryIdentityPaths;
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, WithLockError,
    create_lock_record, facts_queue_lock_path, with_lock,
};
use crate::support::random::random_uuid;
use crate::support::time::{format_rfc3339_millis, now_millis};

use super::failures_backoff::{
    ApplyFailureInput, FactsFailureFilter, FactsFailureTarget, apply_failure, clear_for_retry,
    clear_on_success,
};
use super::failures_schema::{
    FactsFailureReason, FactsFailureRecord, FactsFailuresCorruptError, FactsFailuresFile,
    empty_failures_file, parse_failures_file, render_failures_file,
};
use super::schema::{FactsQueueLayout, facts_queue_paths};

const LOCK_WAIT_MS: u64 = 2000;

/// Options for configuring a facts failure store.
pub struct FactsFailureStoreOptions {
    pub identity_paths: MemoryIdentityPaths,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub lock_wait_ms: Option<u64>,
}

/// Request payload for recording a batch failure.
#[derive(Debug, Clone)]
pub struct RecordFailureRequest {
    pub targets: Vec<FactsFailureTarget>,
    pub failure_id: String,
    pub reason: FactsFailureReason,
    pub detail: Option<String>,
}

/// Errors returned by the facts failure store.
#[derive(Debug)]
pub enum FactsFailureStoreError {
    Io(io::Error),
    Corrupt(FactsFailuresCorruptError),
    Lock(AcquireLockError),
}

impl std::fmt::Display for FactsFailureStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "facts failure store io error: {err}"),
            Self::Corrupt(err) => write!(f, "{err}"),
            Self::Lock(err) => write!(f, "facts failure store lock error: {err}"),
        }
    }
}

impl std::error::Error for FactsFailureStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Corrupt(err) => Some(err),
            Self::Lock(err) => Some(err),
        }
    }
}

impl From<io::Error> for FactsFailureStoreError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<FactsFailuresCorruptError> for FactsFailureStoreError {
    fn from(err: FactsFailuresCorruptError) -> Self {
        Self::Corrupt(err)
    }
}

impl From<AcquireLockError> for FactsFailureStoreError {
    fn from(err: AcquireLockError) -> Self {
        Self::Lock(err)
    }
}

impl From<WithLockError<FactsFailureStoreError>> for FactsFailureStoreError {
    fn from(err: WithLockError<Self>) -> Self {
        match err {
            WithLockError::Acquire(err) => Self::Lock(err),
            WithLockError::User(err) => err,
        }
    }
}

/// Durable store for persisting failure streaks and backoff state.
pub struct FactsFailureStore {
    layout: FactsQueueLayout,
    lock_path: PathBuf,
    lock_wait_ms: u64,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl FactsFailureStore {
    /// Create a new facts failure store instance.
    pub fn new(options: FactsFailureStoreOptions) -> Self {
        let layout = facts_queue_paths(&options.identity_paths);
        let lock_path = facts_queue_lock_path(&options.identity_paths.locks);
        let lock_wait_ms = options.lock_wait_ms.unwrap_or(LOCK_WAIT_MS);
        let now = options.now.unwrap_or_else(|| Arc::new(now_millis));
        Self {
            layout,
            lock_path,
            lock_wait_ms,
            now,
        }
    }

    /// Read the current failures ledger fail-closed.
    pub fn read_failures(&self) -> Result<FactsFailuresFile, FactsFailureStoreError> {
        self.locked(|| self.read_unlocked())
    }

    /// Record failure targets into the durable ledger.
    pub fn record_failure(
        &self,
        request: RecordFailureRequest,
    ) -> Result<FactsFailuresFile, FactsFailureStoreError> {
        self.mutate(|entries, at_millis| {
            apply_failure(&ApplyFailureInput {
                entries: entries.to_vec(),
                targets: request.targets.clone(),
                failure_id: request.failure_id.clone(),
                reason: request.reason,
                detail: request.detail.clone(),
                now: format_rfc3339_millis(at_millis),
            })
        })
    }

    /// Clear failure records for endpoints that successfully processed.
    pub fn clear_on_success(
        &self,
        targets: &[FactsFailureTarget],
    ) -> Result<FactsFailuresFile, FactsFailureStoreError> {
        self.mutate(|entries, _at_millis| clear_on_success(entries, targets))
    }

    /// Manually clear failure records matching a filter for retry.
    pub fn clear_for_retry(
        &self,
        filter: &FactsFailureFilter,
    ) -> Result<usize, FactsFailureStoreError> {
        let mut removed = 0;
        self.mutate(|entries, _at_millis| {
            let next = clear_for_retry(entries, filter);
            removed = entries.len().saturating_sub(next.len());
            next
        })?;
        Ok(removed)
    }

    fn mutate<F>(&self, change: F) -> Result<FactsFailuresFile, FactsFailureStoreError>
    where
        F: FnOnce(&[FactsFailureRecord], i64) -> Vec<FactsFailureRecord>,
    {
        self.locked(|| {
            let current = self.read_unlocked()?;
            let at_millis = (self.now)();
            let at_iso = format_rfc3339_millis(at_millis);
            let entries = change(&current.entries, at_millis);
            let file = FactsFailuresFile {
                version: current.version,
                updated_at: at_iso.clone(),
                entries,
            };
            let rendered = render_failures_file(&file.entries, &file.updated_at);
            self.write_atomically(&rendered)?;
            Ok(file)
        })
    }

    fn read_unlocked(&self) -> Result<FactsFailuresFile, FactsFailureStoreError> {
        let raw = match fs::read_to_string(&self.layout.failures_path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let at_iso = format_rfc3339_millis((self.now)());
                return Ok(empty_failures_file(at_iso));
            }
            Err(err) => return Err(FactsFailureStoreError::Io(err)),
        };
        parse_failures_file(&raw).map_err(FactsFailureStoreError::Corrupt)
    }

    fn write_atomically(&self, content: &str) -> Result<(), FactsFailureStoreError> {
        let directory = self
            .layout
            .failures_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)?;
        let temp_name = format!(".failures-{}-{}.tmp", std::process::id(), random_uuid());
        let temporary = directory.join(temp_name);

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let write_result = (|| -> Result<(), io::Error> {
            let mut file = options.open(&temporary)?;
            io::Write::write_all(&mut file, content.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.layout.failures_path)?;
            sync_dir(directory)?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        Ok(())
    }

    fn locked<T, F>(&self, task: F) -> Result<T, FactsFailureStoreError>
    where
        F: FnOnce() -> Result<T, FactsFailureStoreError>,
    {
        fs::create_dir_all(&self.layout.queue_dir)?;
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let record = create_lock_record("facts-queue", CreateLockRecordOptions::default())
            .map_err(|err| FactsFailureStoreError::Io(io::Error::other(format!("{err:?}"))))?;
        let options = AcquireLockOptions {
            wait_timeout_ms: Some(self.lock_wait_ms),
            ..Default::default()
        };
        with_lock(&self.lock_path, &record, &options, task).map_err(FactsFailureStoreError::from)
    }
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        if let Ok(file) = fs::File::open(path) {
            let _ = file.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "failures_store_tests.rs"]
mod tests;
