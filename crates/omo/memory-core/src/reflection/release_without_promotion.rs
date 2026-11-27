//! Releases `active.lock` as a failure WITHOUT promoting `pending.json` (DESIGN.r2 4.1b, #12517).

use std::path::Path;

use crate::journal::store::{TranscriptJournal, TranscriptJournalOptions};
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::domains::reflection_scheduler_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};

use super::machine::{ReflectionTrigger, ReservedRun};

#[derive(Clone)]
pub struct ReleaseWithoutPromotionOptions<'a> {
    pub reflection_dir: &'a Path,
    pub transcripts_dir: &'a Path,
    pub locks_dir: &'a Path,
    pub run_id: &'a str,
    pub now_iso: Option<&'a dyn Fn() -> String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseWithoutPromotionResult {
    pub released: bool,
    pub pending_run_id: Option<String>,
}

fn read_pending_run_id(reflection_dir: &Path) -> std::io::Result<Option<String>> {
    let raw = match std::fs::read_to_string(reflection_dir.join("pending.json")) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(None);
    };
    Ok(value
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

/// Releases `run_id`'s reservation without promoting `pending.json` so a CLI reaper exits cleanly
/// and lets the next live session promote the queued run via `promote_pending`.
pub fn release_reservation_without_promotion(
    options: &ReleaseWithoutPromotionOptions<'_>,
) -> Result<ReleaseWithoutPromotionResult, String> {
    let record = create_lock_record("reflection-scheduler", CreateLockRecordOptions::default())
        .map_err(|e| e.to_string())?;
    let lock_path = reflection_scheduler_lock_path(options.locks_dir);
    let acquire_opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };

    match with_lock(
        &lock_path,
        &record,
        &acquire_opts,
        || -> Result<ReleaseWithoutPromotionResult, String> {
            let active_path = options.reflection_dir.join("active.lock");
            let raw = match std::fs::read_to_string(&active_path) {
                Ok(raw) => raw,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(ReleaseWithoutPromotionResult {
                        released: false,
                        pending_run_id: None,
                    });
                }
                Err(err) => return Err(err.to_string()),
            };
            let active: ReservedRun = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            if active.run_id != options.run_id {
                return Ok(ReleaseWithoutPromotionResult {
                    released: false,
                    pending_run_id: None,
                });
            }
            if active.request.trigger != ReflectionTrigger::Manual
                && active.request.trigger != ReflectionTrigger::Dream
            {
                for conversation_id in &active.request.conversation_ids {
                    let journal_dir = options.transcripts_dir.join(conversation_id);
                    if !journal_dir.is_dir() {
                        continue;
                    }
                    let mut opts = TranscriptJournalOptions::new(&journal_dir);
                    if let Some(now_iso) = options.now_iso {
                        let timestamp = now_iso();
                        opts.now = Some(Box::new(move || timestamp.clone()));
                    }
                    TranscriptJournal::new(opts)
                        .record_reflection_failure()
                        .map_err(|e| e.to_string())?;
                }
            }
            let pending_run_id =
                read_pending_run_id(options.reflection_dir).map_err(|e| e.to_string())?;
            if let Err(err) = std::fs::remove_file(&active_path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err.to_string());
            }
            Ok(ReleaseWithoutPromotionResult {
                released: true,
                pending_run_id,
            })
        },
    ) {
        Ok(result) => Ok(result),
        Err(WithLockError::Acquire(AcquireLockError::Contention(err))) => Err(err.to_string()),
        Err(WithLockError::Acquire(err)) => Err(err.to_string()),
        Err(WithLockError::User(err)) => Err(err),
    }
}
