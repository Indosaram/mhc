//! Transcript journal retention (`transcripts/<sessionId>/`, design 2.3).
//!
//! Classes judged from `state.json`:
//! - `Fully` (J1): unlink `transcript.jsonl` after `retention_days`, stamp `transcript_pruned_at`.
//! - `Never` (J2): unlink `state.json` & `transcript.jsonl` and rmdir after `abandoned_journal_days`.
//! - `Partly` (J3): unlink `transcript.jsonl` after `abandoned_journal_days`, stamp `transcript_pruned_at`.
//!
//! Tolerates corrupt/unreadable journals as `RetentionKeepReason::StateUnreadable` without aborting the pass.

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};

use crate::journal::cursor::{ReflectionTranscriptState, derive_state, initial_reflection_state};
use crate::journal::lock::{JournalLock, with_local_journal_lock};
use crate::journal::store::{JournalError, TranscriptJournal, TranscriptJournalOptions};
use crate::support::random::random_uuid;
use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

use super::budget::{PassBudget, RetentionStore};
use super::guard::LazyRetentionGuard;
use super::policy::{
    RetentionContext, RetentionKeepReason, RetentionStoreResult, StoreTally, cutoff_for_days,
    list_names,
};
use super::reflection_health_signal::UnreflectedRetentionGate;

const TRANSCRIPT: &str = "transcript.jsonl";
const STATE: &str = "state.json";
const LOCK: &str = "state.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalClass {
    Fully,
    Never,
    Partly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalVerdict {
    Prune(JournalClass),
    Keep(RetentionKeepReason),
}

enum StateRead {
    Missing,
    Unreadable,
    Ok(Map<String, Value>),
}

fn read_state(path: &Path) -> Result<StateRead, std::io::Error> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(StateRead::Missing),
        Err(err) => return Err(err),
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return Ok(StateRead::Unreadable);
    };
    Ok(match val.as_object() {
        Some(obj) => StateRead::Ok(obj.clone()),
        None => StateRead::Unreadable,
    })
}

fn count_u64(value: Option<&Value>) -> Option<usize> {
    let Some(val) = value else {
        return Some(0);
    };
    val.as_u64().and_then(|n| usize::try_from(n).ok())
}

fn classify_state(state: Option<&Map<String, Value>>) -> Result<JournalClass, RetentionKeepReason> {
    let Some(state) = state else {
        return Ok(JournalClass::Never);
    };
    let Some(total) = count_u64(state.get("total_completed_steps")) else {
        return Err(RetentionKeepReason::Unclassified);
    };
    let Some(reflected) = count_u64(state.get("reflected_completed_steps")) else {
        return Err(RetentionKeepReason::Unclassified);
    };
    let Some(since) = count_u64(state.get("steps_since_last_successful_reflection")) else {
        return Err(RetentionKeepReason::Unclassified);
    };
    let has_anchor = state
        .get("reflected_through_message_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());

    if reflected == 0 && !has_anchor {
        return Ok(JournalClass::Never);
    }
    if total > 0 && since == 0 && has_anchor {
        if state.get("pending_compaction").and_then(Value::as_bool) == Some(true) {
            return Err(RetentionKeepReason::PendingCompaction);
        }
        return Ok(JournalClass::Fully);
    }
    if reflected > 0 && since > 0 && has_anchor {
        return Ok(JournalClass::Partly);
    }
    Err(RetentionKeepReason::Unclassified)
}

fn newest_file_mtime(dir: &Path, holding_lock: bool) -> Option<i64> {
    let mut newest = if holding_lock {
        0i64
    } else {
        let meta = std::fs::symlink_metadata(dir).ok()?;
        let mtime = meta.modified().ok()?;
        i64::try_from(mtime.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?
    };
    let names = list_names(dir);
    for name in names {
        if holding_lock && name == LOCK {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(dir.join(name)) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if let Ok(ms) = i64::try_from(mtime.duration_since(UNIX_EPOCH).ok()?.as_millis()) {
            newest = newest.max(ms);
        }
    }
    Some(newest)
}

fn transcript_matches_state(dir: &Path, state: &Map<String, Value>) -> Result<bool, JournalError> {
    let transcript_path = dir.join(TRANSCRIPT);
    let raw = match std::fs::read_to_string(&transcript_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(JournalError::Io(err)),
    };
    let non_empty_lines = raw.lines().filter(|line| !line.trim().is_empty()).count();
    let entries =
        TranscriptJournal::new(TranscriptJournalOptions::new(dir)).read_entries_snapshot()?;
    if entries.len() != non_empty_lines {
        return Ok(false);
    }
    let anchor = state
        .get("reflected_through_message_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let reflected_completed_steps = count_u64(state.get("reflected_completed_steps")).unwrap_or(0);
    let base = ReflectionTranscriptState {
        reflected_through_message_id: anchor,
        reflected_completed_steps,
        ..initial_reflection_state()
    };
    let derived = derive_state(&base, &entries);
    if Some(derived.total_completed_steps) != count_u64(state.get("total_completed_steps")) {
        return Ok(false);
    }
    let state_unreflected = state.get("unreflected_bytes");
    if state_unreflected.is_none() {
        return Ok(true);
    }
    Ok(count_u64(state_unreflected) == derived.unreflected_bytes)
}

fn judge(
    context: &RetentionContext<'_>,
    dir: &Path,
    holding_lock: bool,
    gate: Option<&UnreflectedRetentionGate>,
) -> Result<JournalVerdict, String> {
    let state_opt = match read_state(&dir.join(STATE)).map_err(|e| e.to_string())? {
        StateRead::Missing => None,
        StateRead::Unreadable => {
            return Ok(JournalVerdict::Keep(RetentionKeepReason::StateUnreadable));
        }
        StateRead::Ok(map) => Some(map),
    };
    let kind = match classify_state(state_opt.as_ref()) {
        Ok(k) => k,
        Err(reason) => return Ok(JournalVerdict::Keep(reason)),
    };
    if kind != JournalClass::Never && !dir.join(TRANSCRIPT).exists() {
        return Ok(JournalVerdict::Keep(RetentionKeepReason::Pruned));
    }
    if kind != JournalClass::Fully && !context.policy.discard_unreflected_journals {
        return Ok(JournalVerdict::Keep(
            RetentionKeepReason::UnreflectedRetentionOff,
        ));
    }
    if kind != JournalClass::Fully && !gate.is_some_and(|g| g.open) {
        return Ok(JournalVerdict::Keep(
            RetentionKeepReason::ReflectionUnhealthy,
        ));
    }
    let days = if kind == JournalClass::Fully {
        context.policy.retention_days
    } else {
        context.policy.abandoned_journal_days
    };
    let cutoff = cutoff_for_days(context.now(), days);
    let newest = newest_file_mtime(dir, holding_lock);
    if newest.is_none_or(|t| t > cutoff) {
        return Ok(JournalVerdict::Keep(RetentionKeepReason::Recent));
    }
    if let Some(ref state) = state_opt
        && let Some(started) = state
            .get("last_reflection_started_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339)
        && started > cutoff
    {
        return Ok(JournalVerdict::Keep(RetentionKeepReason::Recent));
    }
    if let Some(ref state) = state_opt
        && !transcript_matches_state(dir, state).map_err(|e| e.to_string())?
    {
        return Ok(JournalVerdict::Keep(
            RetentionKeepReason::StateBehindTranscript,
        ));
    }
    Ok(JournalVerdict::Prune(kind))
}

/// Lock-free verdict for one journal directory; unreadable/broken journals become `StateUnreadable`.
pub fn classify_journal_entry(
    context: &RetentionContext<'_>,
    dir: &Path,
    gate: Option<&UnreflectedRetentionGate>,
) -> Option<JournalVerdict> {
    if !dir.is_dir() {
        return None;
    }
    Some(
        judge(context, dir, false, gate)
            .unwrap_or(JournalVerdict::Keep(RetentionKeepReason::StateUnreadable)),
    )
}

fn stamp_pruned(state_path: &Path, now_ms: i64) -> std::io::Result<()> {
    let state = match read_state(state_path)? {
        StateRead::Ok(map) => map,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "journal state vanished under its lock: {}",
                    state_path.display()
                ),
            ));
        }
    };
    let mut updated = state;
    updated.remove("reflected_through_byte_offset");
    updated.insert(
        "transcript_pruned_at".to_owned(),
        Value::String(format_rfc3339_millis(now_ms)),
    );
    let temp = state_path.with_file_name(format!("{STATE}.tmp-{}", random_uuid()));
    let json = serde_json::to_string_pretty(&Value::Object(updated))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Err(err) = std::fs::write(&temp, format!("{json}\n")) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, state_path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

fn unlink_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub fn remove_transcript_journal_under_lock(
    context: &RetentionContext<'_>,
    dir: &Path,
    gate: Option<&UnreflectedRetentionGate>,
    custom_lock: Option<&dyn JournalLock>,
) -> Result<Result<(), RetentionKeepReason>, String> {
    let lock_path = dir.join(LOCK);
    let mut verdict_out = JournalVerdict::Keep(RetentionKeepReason::StateUnreadable);
    let run_result: Result<(), JournalError> = {
        let mut inner = || -> Result<(), JournalError> {
            let current = judge(context, dir, true, gate)
                .map_err(|msg| JournalError::Io(std::io::Error::other(msg)))?;
            verdict_out = current;
            if let JournalVerdict::Prune(kind) = current {
                if kind == JournalClass::Never {
                    unlink_if_present(&dir.join(STATE)).map_err(JournalError::Io)?;
                    unlink_if_present(&dir.join(TRANSCRIPT)).map_err(JournalError::Io)?;
                } else {
                    stamp_pruned(&dir.join(STATE), context.now()).map_err(JournalError::Io)?;
                    unlink_if_present(&dir.join(TRANSCRIPT)).map_err(JournalError::Io)?;
                }
            }
            Ok(())
        };
        match custom_lock {
            Some(lock) => lock.with_lock(&lock_path, &mut inner, context.aborted),
            None => with_local_journal_lock(&lock_path, inner, context.aborted),
        }
    };

    match run_result {
        Ok(()) => {}
        Err(JournalError::LockTimeout(_)) => {
            return Ok(Err(RetentionKeepReason::Locked));
        }
        Err(err) => return Err(err.to_string()),
    }

    match verdict_out {
        JournalVerdict::Keep(reason) => Ok(Err(reason)),
        JournalVerdict::Prune(kind) if kind != JournalClass::Never => Ok(Ok(())),
        JournalVerdict::Prune(_) => match std::fs::remove_dir(dir) {
            Ok(()) => Ok(Ok(())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Ok(())),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::AlreadyExists
                ) =>
            {
                Ok(Err(RetentionKeepReason::ChangedDuringRemoval))
            }
            Err(err) => Err(err.to_string()),
        },
    }
}

fn visit_journal(
    context: &RetentionContext<'_>,
    lazy_guard: &mut LazyRetentionGuard<'_>,
    gate: Option<&UnreflectedRetentionGate>,
    custom_lock: Option<&dyn JournalLock>,
    session_id: &str,
) -> Result<Option<RetentionKeepReason>, String> {
    let dir = context.paths.transcripts.join(session_id);
    let Ok(meta) = std::fs::symlink_metadata(&dir) else {
        return Ok(None);
    };
    if !meta.is_dir() {
        return Ok(None);
    }
    if context.is_live_session.is_some_and(|live| live(session_id)) {
        return Ok(Some(RetentionKeepReason::Live));
    }
    if dir.join(LOCK).exists() {
        return Ok(Some(RetentionKeepReason::Locked));
    }
    match judge(context, &dir, false, gate)? {
        JournalVerdict::Keep(reason) => return Ok(Some(reason)),
        JournalVerdict::Prune(_) => {}
    }
    let Some(guard) = lazy_guard.get()? else {
        return Ok(Some(RetentionKeepReason::SchedulerBusy));
    };
    if guard.reserved_conversation_ids.contains(session_id) {
        return Ok(Some(RetentionKeepReason::Reserved));
    }
    if let Some(before) = context.before_journal_claim {
        before(session_id);
    }
    match remove_transcript_journal_under_lock(context, &dir, gate, custom_lock)? {
        Ok(()) => Ok(None),
        Err(reason) => Ok(Some(reason)),
    }
}

pub fn prune_transcript_journals(
    context: &RetentionContext<'_>,
    lazy_guard: &mut LazyRetentionGuard<'_>,
    gate: Option<&UnreflectedRetentionGate>,
    budget: &mut PassBudget<'_>,
    custom_lock: Option<&dyn JournalLock>,
) -> RetentionStoreResult {
    let mut tally = StoreTally::default();
    for session_id in list_names(&context.paths.transcripts) {
        if budget.skip(RetentionStore::Journals, &session_id) {
            continue;
        }
        if !budget.admit(RetentionStore::Journals) {
            break;
        }
        // Check first if `transcripts/<session_id>` is actually a directory; stray files are ignored.
        let dir = context.paths.transcripts.join(&session_id);
        let is_dir = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
        if is_dir {
            match visit_journal(context, lazy_guard, gate, custom_lock, &session_id) {
                Ok(None) => tally.removed.push(session_id.clone()),
                Ok(Some(reason)) => tally.keep(session_id.clone(), reason),
                Err(_) => {
                    if !context.is_aborted() {
                        tally.keep(session_id.clone(), RetentionKeepReason::StateUnreadable);
                    }
                }
            }
        }
        budget.done(RetentionStore::Journals, &session_id);
    }
    tally.result()
}
