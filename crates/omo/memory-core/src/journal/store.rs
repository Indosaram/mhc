//! Durable append-only transcript journal and reflection state storage.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::journal::cursor::{
    ReflectionSnapshot, ReflectionTranscriptState, capture_cursor_snapshot, derive_state,
    finalize_cursor, initial_reflection_state, parse_state,
};
use crate::journal::entries::{
    TranscriptEntry, TranscriptProjection, parse_transcript_entry, project_transcript_entries,
};
use crate::journal::lock::{
    DefaultJournalLock, JournalLock, JournalLockTimeoutError, run_with_lock,
};

/// Typed errors produced by transcript journal operations.
#[derive(Debug)]
pub enum JournalError {
    LockTimeout(JournalLockTimeoutError),
    Aborted,
    InvalidTranscriptJournalRow,
    CorruptState(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockTimeout(err) => write!(f, "{err}"),
            Self::Aborted => write!(f, "The operation was aborted"),
            Self::InvalidTranscriptJournalRow => write!(f, "Invalid transcript journal row"),
            Self::CorruptState(msg) => write!(f, "Corrupt transcript state: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON serialization error: {err}"),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LockTimeout(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for JournalError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for JournalError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<JournalLockTimeoutError> for JournalError {
    fn from(err: JournalLockTimeoutError) -> Self {
        Self::LockTimeout(err)
    }
}

/// Result of an append operation on the transcript journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendResult {
    pub appended: usize,
    pub skipped: usize,
}

/// Configuration options for constructing a [`TranscriptJournal`].
pub struct TranscriptJournalOptions {
    pub journal_dir: PathBuf,
    pub now: Option<Box<dyn Fn() -> String + Send + Sync>>,
    pub lock: Option<Arc<dyn JournalLock>>,
}

impl TranscriptJournalOptions {
    /// Create new options for the given directory.
    pub fn new(journal_dir: impl Into<PathBuf>) -> Self {
        Self {
            journal_dir: journal_dir.into(),
            now: None,
            lock: None,
        }
    }
}

/// Durable append-only journal for conversation transcript entries.
pub struct TranscriptJournal {
    pub transcript_path: PathBuf,
    pub state_path: PathBuf,
    pub lock_path: PathBuf,
    journal_dir: PathBuf,
    now: Box<dyn Fn() -> String + Send + Sync>,
    lock: Arc<dyn JournalLock>,
}

impl TranscriptJournal {
    /// Create a new transcript journal with the provided options.
    pub fn new(options: TranscriptJournalOptions) -> Self {
        let transcript_path = options.journal_dir.join("transcript.jsonl");
        let state_path = options.journal_dir.join("state.json");
        let lock_path = options.journal_dir.join("state.lock");
        let now = options
            .now
            .unwrap_or_else(|| Box::new(crate::support::time::now_iso));
        let lock = options.lock.unwrap_or_else(|| Arc::new(DefaultJournalLock));
        Self {
            transcript_path,
            state_path,
            lock_path,
            journal_dir: options.journal_dir,
            now,
            lock,
        }
    }

    fn locked<T>(
        &self,
        cancel: Option<&dyn Fn() -> bool>,
        task: impl FnOnce() -> Result<T, JournalError>,
    ) -> Result<T, JournalError> {
        if let Some(c) = cancel
            && c()
        {
            return Err(JournalError::Aborted);
        }
        std::fs::create_dir_all(&self.journal_dir).map_err(JournalError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.journal_dir, std::fs::Permissions::from_mode(0o700));
        }
        run_with_lock(&*self.lock, &self.lock_path, task, cancel)
    }

    fn read_entries_unlocked(&self) -> Result<Vec<TranscriptEntry>, JournalError> {
        let raw = match std::fs::read_to_string(&self.transcript_path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.transcript_path)
                    .map_err(JournalError::Io)?;
                return Ok(Vec::new());
            }
            Err(err) => return Err(JournalError::Io(err)),
        };

        let mut entries = Vec::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|_| JournalError::InvalidTranscriptJournalRow)?;
            let entry = parse_transcript_entry(&value)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    fn read_state_unlocked(&self) -> Result<ReflectionTranscriptState, JournalError> {
        match std::fs::read_to_string(&self.state_path) {
            Ok(content) => parse_state(&content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(initial_reflection_state())
            }
            Err(err) => Err(JournalError::Io(err)),
        }
    }

    fn write_state_unlocked(
        &self,
        state: &ReflectionTranscriptState,
        entries: &[TranscriptEntry],
    ) -> Result<(), JournalError> {
        let derived = derive_state(state, entries);
        let temp_filename = format!(
            "{}.tmp-{}",
            self.state_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            crate::support::random::random_uuid()
        );
        let temporary_path = self.journal_dir.join(temp_filename);
        let json = serde_json::to_string_pretty(&derived).map_err(JournalError::Json)?;
        std::fs::write(&temporary_path, format!("{json}\n")).map_err(JournalError::Io)?;
        std::fs::rename(&temporary_path, &self.state_path).map_err(JournalError::Io)?;
        Ok(())
    }

    fn append_unlocked(&self, entries: &[TranscriptEntry]) -> Result<AppendResult, JournalError> {
        let existing = self.read_entries_unlocked()?;
        let mut source_ids: HashSet<String> = existing
            .iter()
            .map(|e| e.source_line_id().to_string())
            .collect();
        let mut fresh = Vec::new();
        let mut skipped = 0;
        for entry in entries {
            let id = entry.source_line_id();
            if source_ids.contains(id) {
                skipped += 1;
            } else {
                source_ids.insert(id.to_string());
                fresh.push(entry.clone());
            }
        }
        if !fresh.is_empty() {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.transcript_path)
                .map_err(JournalError::Io)?;
            for entry in &fresh {
                let json = serde_json::to_string(entry).map_err(JournalError::Json)?;
                writeln!(file, "{json}").map_err(JournalError::Io)?;
            }
            file.flush().map_err(JournalError::Io)?;
        }
        let mut all_entries = existing;
        all_entries.extend(fresh.clone());
        let current_state = self.read_state_unlocked()?;
        self.write_state_unlocked(&current_state, &all_entries)?;
        Ok(AppendResult {
            appended: fresh.len(),
            skipped,
        })
    }

    /// Reconcile structured transcript messages by projecting and appending new rows.
    pub fn reconcile(
        &self,
        messages: &[TranscriptProjection],
    ) -> Result<AppendResult, JournalError> {
        self.locked(None, || {
            let captured_at = (self.now)();
            let entries: Vec<TranscriptEntry> = messages
                .iter()
                .flat_map(|m| project_transcript_entries(m, &captured_at))
                .collect();
            self.append_unlocked(&entries)
        })
    }

    /// Append entries directly to the journal, deduplicating by source line ID.
    pub fn append(&self, entries: &[TranscriptEntry]) -> Result<AppendResult, JournalError> {
        self.locked(None, || self.append_unlocked(entries))
    }

    /// Read all validated transcript entries from the journal file.
    pub fn read_entries(&self) -> Result<Vec<TranscriptEntry>, JournalError> {
        self.locked(None, || self.read_entries_unlocked())
    }

    /// Retrieve the current reflection transcript state, recalculating step counters.
    pub fn get_state(&self) -> Result<ReflectionTranscriptState, JournalError> {
        self.locked(None, || {
            let entries = self.read_entries_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state(&current_state, &entries);
            self.write_state_unlocked(&state, &entries)?;
            Ok(state)
        })
    }

    /// Update the pending compaction flag in the reflection state file.
    pub fn set_pending_compaction(&self, pending: bool) -> Result<(), JournalError> {
        self.locked(None, || {
            let entries = self.read_entries_unlocked()?;
            let mut state = self.read_state_unlocked()?;
            state.pending_compaction = Some(pending);
            self.write_state_unlocked(&state, &entries)
        })
    }

    /// Capture an unreflected snapshot of entries if available.
    pub fn capture_reflection_snapshot(
        &self,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<Option<ReflectionSnapshot>, JournalError> {
        self.locked(cancel, || {
            let entries = self.read_entries_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state(&current_state, &entries);
            let snapshot = capture_cursor_snapshot(&entries, &state);
            let Some(snapshot) = snapshot else {
                return Ok(None);
            };
            if let Some(c) = cancel
                && c()
            {
                return Err(JournalError::Aborted);
            }
            let mut updated_state = state;
            updated_state.last_reflection_started_at = Some((self.now)());
            self.write_state_unlocked(&updated_state, &entries)?;
            Ok(Some(snapshot))
        })
    }

    /// Flush and fsync transcript rows, state file, and journal directory.
    pub fn flush(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<(), JournalError> {
        let is_aborted = || cancel.is_some_and(|c| c());
        self.locked(cancel, || {
            if is_aborted() {
                return Ok(());
            }
            sync_journal_file(&self.transcript_path)?;
            if is_aborted() {
                return Ok(());
            }
            sync_journal_file(&self.state_path)?;
            if is_aborted() {
                return Ok(());
            }
            sync_journal_directory(&self.journal_dir)?;
            Ok(())
        })
    }

    /// Finalize a reflection run using the given snapshot and completion status.
    pub fn finalize_reflection(
        &self,
        snapshot: &ReflectionSnapshot,
        success: bool,
    ) -> Result<(), JournalError> {
        self.locked(None, || {
            let entries = self.read_entries_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state(&current_state, &entries);
            let final_state = finalize_cursor(&state, &entries, snapshot, success, &(self.now)());
            self.write_state_unlocked(&final_state, &entries)
        })
    }
}

fn sync_path(path: &Path) -> Result<(), JournalError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) => match err.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => return Ok(()),
            _ => return Ok(()),
        },
    };
    if let Err(err) = file.sync_all() {
        match err.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => return Ok(()),
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// Fsync a journal file if it exists.
pub fn sync_journal_file(path: &Path) -> Result<(), JournalError> {
    sync_path(path)
}

/// Fsync the journal directory if supported by the platform.
pub fn sync_journal_directory(dir: &Path) -> Result<(), JournalError> {
    sync_path(dir)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
