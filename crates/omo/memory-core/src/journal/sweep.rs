use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::lock::{JournalLockTimeoutError, try_with_local_journal_lock};
use super::store::JournalError;

pub const DEFAULT_MIN_IDLE_MS: u64 = 10 * 60 * 1000;
pub const TRANSCRIPT: &str = "transcript.jsonl";
pub const STATE: &str = "state.json";
pub const LOCK: &str = "state.lock";
const MAX_INSPECTED_BYTES: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EmptyJournalKeepReason { Live, Locked, Recent, TranscriptContent, StateContent, StateUnreadable, OtherContent, ChangedDuringRemoval }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyJournalSweepResult {
    pub removed: Vec<String>,
    pub kept: std::collections::BTreeMap<EmptyJournalKeepReason, usize>,
}

#[derive(Debug)]
pub enum EmptyJournalSweepError { Io(std::io::Error), Lock(JournalError) }

impl std::fmt::Display for EmptyJournalSweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Io(error) => write!(f, "{error}"), Self::Lock(error) => write!(f, "{error}") }
    }
}
impl std::error::Error for EmptyJournalSweepError {}

fn inspect(dir: &Path, cutoff: u64, holding_lock: bool) -> Result<Result<Vec<PathBuf>, EmptyJournalKeepReason>, std::io::Error> {
    let metadata = std::fs::symlink_metadata(dir)?;
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if !holding_lock && modified > cutoff { return Ok(Err(EmptyJournalKeepReason::Recent)); }
    let mut files = Vec::new();
    let mut transcript = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == LOCK { if holding_lock { continue; } return Ok(Err(EmptyJournalKeepReason::Locked)); }
        let path = entry.path();
        let info = std::fs::symlink_metadata(&path)?;
        if !info.file_type().is_file() { return Ok(Err(EmptyJournalKeepReason::OtherContent)); }
        if info.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 > cutoff { return Ok(Err(EmptyJournalKeepReason::Recent)); }
        if name == TRANSCRIPT {
            if info.len() > MAX_INSPECTED_BYTES { return Ok(Err(EmptyJournalKeepReason::TranscriptContent)); }
            let text = std::fs::read_to_string(&path)?;
            if !text.trim().is_empty() { return Ok(Err(EmptyJournalKeepReason::TranscriptContent)); }
            transcript = Some(path);
        } else if name == STATE {
            if info.len() > MAX_INSPECTED_BYTES { return Ok(Err(EmptyJournalKeepReason::StateContent)); }
            let text = std::fs::read_to_string(&path)?;
            if !text.is_empty() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { return Ok(Err(EmptyJournalKeepReason::StateUnreadable)); };
                let Some(object) = value.as_object() else { return Ok(Err(EmptyJournalKeepReason::StateUnreadable)); };
                if object.iter().any(|(key, value)| key != "schema_version" && !value.is_null() && value != 0 && value != false) { return Ok(Err(EmptyJournalKeepReason::StateContent)); }
            }
            files.push(path);
        } else if info.len() > 0 { return Ok(Err(EmptyJournalKeepReason::OtherContent)); }
        else { files.push(path); }
    }
    if let Some(path) = transcript { files.push(path); }
    Ok(Ok(files))
}

pub fn sweep_empty_transcript_journals(
    transcripts_dir: &Path,
    now: u64,
    min_idle_ms: u64,
    is_live: &dyn Fn(&str) -> bool,
) -> Result<EmptyJournalSweepResult, EmptyJournalSweepError> {
    sweep_with_before_lock(transcripts_dir, now, min_idle_ms, is_live, &|_| {})
}

fn sweep_with_before_lock(
    transcripts_dir: &Path,
    now_ms: u64,
    min_idle_ms: u64,
    is_live: &dyn Fn(&str) -> bool,
    before_lock: &dyn Fn(&Path),
) -> Result<EmptyJournalSweepResult, EmptyJournalSweepError> {
    let cutoff = now_ms.saturating_sub(min_idle_ms);
    let mut result = EmptyJournalSweepResult { removed: Vec::new(), kept: std::collections::BTreeMap::new() };
    let entries = match std::fs::read_dir(transcripts_dir) {
        Ok(entries) => entries,
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => return Ok(result),
        Err(error) => return Err(EmptyJournalSweepError::Io(error)),
    };
    for entry in entries {
        let entry = match entry { Ok(entry) => entry, Err(error) => return Err(EmptyJournalSweepError::Io(error)) };
        if !entry.file_type().map_err(EmptyJournalSweepError::Io)?.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_live(&name) { *result.kept.entry(EmptyJournalKeepReason::Live).or_default() += 1; continue; }
        let dir = entry.path();
        match inspect(&dir, cutoff, false) {
            Ok(Ok(_)) => {},
            Ok(Err(reason)) => { *result.kept.entry(reason).or_default() += 1; continue; }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(EmptyJournalSweepError::Io(error)),
        }
        let lock_path = dir.join(LOCK);
        before_lock(&dir);
        let mut verdict = None;
        let locked = try_with_local_journal_lock(&lock_path, || {
            let files = match inspect(&dir, cutoff, true).map_err(JournalError::Io)? {
                Ok(files) => files,
                Err(reason) => { verdict = Some(Err(reason)); return Ok(()); }
            };
            for file in files {
                match std::fs::remove_file(file) { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(JournalError::Io(error)) }
            }
            verdict = Some(Ok(()));
            Ok(())
        }, None);
        if let Err(error) = locked {
            if matches!(error, JournalError::LockTimeout(JournalLockTimeoutError { .. })) { *result.kept.entry(EmptyJournalKeepReason::Locked).or_default() += 1; continue; }
            if matches!(error, JournalError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound) { continue; }
            return Err(EmptyJournalSweepError::Lock(error));
        }
        match verdict { Some(Ok(())) => {}, Some(Err(reason)) => { *result.kept.entry(reason).or_default() += 1; continue; }, None => continue }
        match std::fs::remove_dir(&dir) {
            Ok(()) => result.removed.push(name),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => result.removed.push(name),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::AlreadyExists) => *result.kept.entry(EmptyJournalKeepReason::ChangedDuringRemoval).or_default() += 1,
            Err(error) => return Err(EmptyJournalSweepError::Io(error)),
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod tests;
