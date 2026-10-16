//! Durable append-only transcript journal and reflection state storage.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::journal::cursor::{
    ReflectionSnapshot, ReflectionTranscriptState, capture_cursor_snapshot_with_max_bytes, count_completed_steps,
    derive_state, derive_state_with_total, finalize_cursor, initial_reflection_state, parse_state,
};
use crate::journal::entries::{
    TranscriptEntry, TranscriptProjection, parse_transcript_entry, project_transcript_entries,
};
use crate::journal::lock::{DefaultJournalLock, JournalLock, JournalLockTimeoutError, run_with_lock};

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
    pub snapshot_max_bytes: Option<u64>,
}

impl TranscriptJournalOptions {
    /// Create new options for the given directory.
    pub fn new(journal_dir: impl Into<PathBuf>) -> Self {
        Self {
            journal_dir: journal_dir.into(),
            now: None,
            lock: None,
            snapshot_max_bytes: None,
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
    snapshot_max_bytes: Option<u64>,
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
            snapshot_max_bytes: options.snapshot_max_bytes,
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
            let entry = parse_transcript_entry(value)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Raw transcript text; creates the file when missing, like [`Self::read_entries_unlocked`].
    fn read_raw_unlocked(&self) -> Result<String, JournalError> {
        match std::fs::read_to_string(&self.transcript_path) {
            Ok(content) => Ok(content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.transcript_path)
                    .map_err(JournalError::Io)?;
                Ok(String::new())
            }
            Err(err) => Err(JournalError::Io(err)),
        }
    }

    fn read_entries_and_raw_unlocked(&self) -> Result<(Vec<TranscriptEntry>, String), JournalError> {
        let raw = self.read_raw_unlocked()?;
        let entries = raw.lines().filter(|line| !line.trim().is_empty()).map(|line| {
            let value = serde_json::from_str(line).map_err(|_| JournalError::InvalidTranscriptJournalRow)?;
            parse_transcript_entry(value)
        }).collect::<Result<Vec<_>, _>>()?;
        Ok((entries, raw))
    }

    /// Validate every row without building entries; returns the completed-step count.
    fn count_completed_rows_unlocked(&self) -> Result<usize, JournalError> {
        let raw = self.read_raw_unlocked()?;
        let mut completed = 0;
        for line in raw.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && parse_row_probe(trimmed)?.is_completed_step() {
                completed += 1;
            }
        }
        Ok(completed)
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
        total_completed_steps: usize,
    ) -> Result<(), JournalError> {
        let mut derived = derive_state_with_total(state, total_completed_steps);
        let (entries, raw) = self.read_entries_and_raw_unlocked()?;
        derived.unreflected_bytes = Some(raw.len().saturating_sub(derived.reflected_through_byte_offset.unwrap_or(0)));
        if derived.reflected_through_byte_offset.is_none() {
            derived.reflected_through_byte_offset = derived.reflected_through_message_id.as_deref().map(|id| crate::journal::cursor::reflected_through_byte_offset(&entries, id));
        }
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
        let raw = self.read_raw_unlocked()?;
        let mut existing_completed = 0;
        let mut source_ids: HashSet<std::borrow::Cow<'_, str>> = HashSet::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let row = parse_row_probe(trimmed)?;
            if row.is_completed_step() {
                existing_completed += 1;
            }
            source_ids.insert(row.source_line_id);
        }
        let mut fresh = Vec::new();
        let mut skipped = 0;
        for entry in entries {
            let id = entry.source_line_id();
            if source_ids.contains(id) {
                skipped += 1;
            } else {
                source_ids.insert(std::borrow::Cow::Borrowed(id));
                fresh.push(entry);
            }
        }
        if !fresh.is_empty() {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.transcript_path)
                .map_err(JournalError::Io)?;
            for entry in fresh.iter().copied() {
                let json = serde_json::to_string(entry).map_err(JournalError::Json)?;
                writeln!(file, "{json}").map_err(JournalError::Io)?;
            }
            file.flush().map_err(JournalError::Io)?;
        }
        let appended = fresh.len();
        let fresh_completed = fresh
            .iter()
            .filter(|entry| count_completed_steps(std::slice::from_ref(*entry)) == 1)
            .count();
        let current_state = self.read_state_unlocked()?;
        self.write_state_unlocked(&current_state, existing_completed + fresh_completed)?;
        Ok(AppendResult { appended, skipped })
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

    pub fn read_entries_snapshot(&self) -> Result<Vec<TranscriptEntry>, JournalError> {
        let raw = match std::fs::read_to_string(&self.transcript_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(JournalError::Io(error)),
        };
        Ok(raw.lines().filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let row = value.as_object()?;
            if !matches!(row.get("kind").and_then(serde_json::Value::as_str), Some("user" | "assistant" | "reasoning" | "error" | "tool_call"))
                || !row.get("captured_at").is_some_and(serde_json::Value::is_string)
                || !row.get("source_line_id").is_some_and(serde_json::Value::is_string)
                || !row.get("source_message_id").is_some_and(serde_json::Value::is_string) { return None; }
            parse_transcript_entry(value).ok()
        }).collect())
    }

    pub fn record_reflection_failure_at(&self, now_millis: i64) -> Result<(), JournalError> {
        self.locked(None, || {
            let entries = self.read_entries_unlocked()?;
            let mut state = self.read_state_unlocked()?;
            let failures = state.consecutive_failures.unwrap_or(0).saturating_add(1);
            let exponent = failures.saturating_sub(1).min(6);
            let delay_seconds = (5u64.saturating_mul(1u64 << exponent)).min(300);
            let next = now_millis.saturating_add((delay_seconds * 1000) as i64);
            state.consecutive_failures = Some(failures);
            state.next_eligible_at = Some(crate::support::time::format_rfc3339_millis(next));
            self.write_state_unlocked(&state, count_completed_steps(&entries))
        })
    }

    pub fn backfill_reflection_byte_offset(&self, entries: &[TranscriptEntry]) -> Result<ReflectionTranscriptState, JournalError> {
        self.locked(None, || {
            let mut state = self.read_state_unlocked()?;
            if state.reflected_through_byte_offset.is_none() {
                state.reflected_through_byte_offset = Some(state.reflected_through_message_id.as_deref().map_or(0, |id| crate::journal::cursor::reflected_through_byte_offset(entries, id)));
            }
            self.write_state_unlocked(&state, count_completed_steps(entries))?;
            self.read_state_unlocked()
        })
    }

    pub fn record_reflection_failure(&self) -> Result<(), JournalError> {
        self.record_reflection_failure_at(crate::support::time::now_millis())
    }

    /// Retrieve the current reflection transcript state, recalculating step counters.
    pub fn get_state(&self) -> Result<ReflectionTranscriptState, JournalError> {
        self.locked(None, || {
            let completed = self.count_completed_rows_unlocked()?;
            let current_state = self.read_state_unlocked()?;
            let state = derive_state_with_total(&current_state, completed);
            self.write_state_unlocked(&state, completed)?;
            Ok(state)
        })
    }

    /// Update the pending compaction flag in the reflection state file.
    pub fn set_pending_compaction(&self, pending: bool) -> Result<(), JournalError> {
        self.locked(None, || {
            let completed = self.count_completed_rows_unlocked()?;
            let mut state = self.read_state_unlocked()?;
            state.pending_compaction = Some(pending);
            self.write_state_unlocked(&state, completed)
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
            let snapshot = capture_cursor_snapshot_with_max_bytes(&entries, &state, self.snapshot_max_bytes);
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
            if current_state.reflected_through_byte_offset.is_none() {
                updated_state.reflected_through_byte_offset = Some(current_state.reflected_through_message_id.as_deref().map_or(0, |id| crate::journal::cursor::reflected_through_byte_offset(&entries, id)));
            }
            self.write_state_unlocked(&updated_state, count_completed_steps(&entries))?;
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
            self.write_state_unlocked(&final_state, count_completed_steps(&entries))
        })
    }
}

/// Shape of one optional JSON field, read without allocating its contents.
enum FieldProbe {
    Str { empty: bool, blank: bool },
    Bool,
    Other,
}

impl<'de> serde::Deserialize<'de> for FieldProbe {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ProbeVisitor;
        impl<'v> serde::de::Visitor<'v> for ProbeVisitor {
            type Value = FieldProbe;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Str {
                    empty: v.is_empty(),
                    blank: v.trim().is_empty(),
                })
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Bool)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'v>>(
                self,
                mut seq: A,
            ) -> Result<FieldProbe, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(FieldProbe::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'v>>(
                self,
                mut map: A,
            ) -> Result<FieldProbe, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(FieldProbe::Other)
            }
        }
        deserializer.deserialize_any(ProbeVisitor)
    }
}

/// Borrowed view of one journal row; validated like [`parse_transcript_entry`].
#[derive(serde::Deserialize)]
struct RowProbe<'a> {
    #[serde(borrow)]
    kind: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    source_line_id: std::borrow::Cow<'a, str>,
    #[serde(default)]
    captured_at: Option<FieldProbe>,
    #[serde(default)]
    source_message_id: Option<FieldProbe>,
    #[serde(default)]
    text: Option<FieldProbe>,
    #[serde(default)]
    name: Option<FieldProbe>,
    #[serde(rename = "argsText", default)]
    args_text: Option<FieldProbe>,
    #[serde(rename = "resultText", default)]
    result_text: Option<FieldProbe>,
    #[serde(rename = "resultOk", default)]
    result_ok: Option<FieldProbe>,
}

impl RowProbe<'_> {
    fn is_completed_step(&self) -> bool {
        self.kind == "assistant"
            && matches!(self.source_message_id, Some(FieldProbe::Str { empty: false, .. }))
            && matches!(self.text, Some(FieldProbe::Str { blank: false, .. }))
    }
}

fn parse_row_probe(line: &str) -> Result<RowProbe<'_>, JournalError> {
    // Structs also accept JSON arrays; journal rows must be objects.
    if !line.starts_with('{') {
        return Err(JournalError::InvalidTranscriptJournalRow);
    }
    let row: RowProbe<'_> =
        serde_json::from_str(line).map_err(|_| JournalError::InvalidTranscriptJournalRow)?;
    let is_str = |field: &Option<FieldProbe>| matches!(field, Some(FieldProbe::Str { .. }));
    let opt_str = |field: &Option<FieldProbe>| matches!(field, None | Some(FieldProbe::Str { .. }));
    if !is_str(&row.captured_at) || !is_str(&row.source_message_id) {
        return Err(JournalError::InvalidTranscriptJournalRow);
    }
    let valid = if row.kind == "tool_call" {
        opt_str(&row.name)
            && opt_str(&row.args_text)
            && opt_str(&row.result_text)
            && matches!(row.result_ok, None | Some(FieldProbe::Bool))
    } else {
        matches!(&*row.kind, "user" | "assistant" | "reasoning" | "error") && is_str(&row.text)
    };
    if valid {
        Ok(row)
    } else {
        Err(JournalError::InvalidTranscriptJournalRow)
    }
}

/// Kinds Windows raises for a directory handle, plus the absent-file case
/// (upstream `SKIPPABLE_SYNC_CODES`: ENOENT, EISDIR, EPERM, EACCES, EINVAL).
fn is_skippable_sync_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::IsADirectory
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::InvalidInput
    )
}

fn sync_path(path: &Path) -> Result<(), JournalError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) if is_skippable_sync_error(err.kind()) => return Ok(()),
        Err(err) => return Err(JournalError::Io(err)),
    };
    match file.sync_all() {
        Ok(()) => Ok(()),
        Err(err) if is_skippable_sync_error(err.kind()) => Ok(()),
        Err(err) => Err(JournalError::Io(err)),
    }
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
