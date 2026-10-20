//! coding-agent/src/core/extensions/builtin/schedule/occurrences.ts.
use super::{Result, ScheduleError, store::*, types::*};
use std::{fs, io::Write, path::Path, time::UNIX_EPOCH};
const TOMBSTONE_RETENTION_MS: u64 = 24 * 60 * 60 * 1000;

pub struct CancelResult {
    pub job: ScheduledJob,
    pub in_flight: bool,
    pub removed_failed: usize,
}
pub fn cancel_scheduled_job(
    dir: &Path,
    id: &str,
    session_id: Option<&str>,
) -> Result<Option<CancelResult>> {
    if !is_scheduled_job_id(id) {
        return Ok(None);
    }
    let listing = list_scheduled_jobs(dir)?;
    let records: Vec<_> = listing
        .jobs
        .iter()
        .filter(|record| record.job.id == id)
        .collect();
    let representative = records
        .iter()
        .find(|record| record.state == ScheduledJobState::Pending)
        .or_else(|| records.first());
    let Some(representative) = representative else {
        return Ok(None);
    };
    if session_id.is_some_and(|session| session != representative.job.session_id) {
        return Ok(None);
    }
    write_atomic(
        &tombstone_path(dir, id),
        &format!("{}\n", chrono::Utc::now().to_rfc3339()),
    )?;
    let in_flight = !occurrence_numbers(dir, "firing", id)?.is_empty();
    unlink_if_present(&dir.join("pending").join(format!("{id}.json")))?;
    let failed: Vec<_> = records
        .iter()
        .filter(|record| record.state == ScheduledJobState::Failed)
        .collect();
    for record in &failed {
        unlink_if_present(&dir.join(&record.file))?;
    }
    Ok(Some(CancelResult {
        job: representative.job.clone(),
        in_flight,
        removed_failed: failed.len(),
    }))
}
pub fn occurrence_file(id: &str, occurrence: u64, owner: &RunnerIdentity) -> String {
    format!(
        "firing/{id}@{occurrence}~{}-{}.json",
        owner.pid, owner.process_started_at_ms
    )
}
pub fn restore_pending(dir: &Path, record: &str, id: &str) -> Result<()> {
    let pending = dir.join("pending").join(format!("{id}.json"));
    match fs::hard_link(dir.join(record), &pending) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    unlink_if_present(&dir.join(record))?;
    if is_cancelled(dir, id)? {
        unlink_if_present(&pending)?;
    }
    Ok(())
}
pub fn claim_occurrence(
    dir: &Path,
    job: &ScheduledJob,
    owner: &RunnerIdentity,
) -> Result<Option<String>> {
    let record = occurrence_file(&job.id, job.fire_count + 1, owner);
    ensure_dir(&dir.join("firing"))?;
    match fs::rename(
        dir.join("pending").join(format!("{}.json", job.id)),
        dir.join(&record),
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    if is_cancelled(dir, &job.id)? {
        unlink_if_present(&dir.join(&record))?;
        return Ok(None);
    }
    let claimed = read_job_file(&dir.join(&record), &job.id)?;
    if claimed.fire_count != job.fire_count {
        restore_pending(dir, &record, &job.id)?;
        return Ok(None);
    }
    Ok(Some(record))
}
pub fn rearm_recurring_job(dir: &Path, next: &ScheduledJob) -> Result<()> {
    if is_cancelled(dir, &next.id)? {
        return Ok(());
    }
    let path = dir.join("pending").join(format!("{}.json", next.id));
    write_atomic(&path, &serialize(next)?)?;
    if is_cancelled(dir, &next.id)? {
        unlink_if_present(&path)?;
    }
    Ok(())
}
fn occurrence_numbers(dir: &Path, state: &str, id: &str) -> Result<Vec<u64>> {
    let prefix = format!("{id}@");
    Ok(names(&dir.join(state))?
        .iter()
        .filter_map(|name| {
            let rest = name.strip_prefix(&prefix)?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect())
}
pub fn restore_abandoned_schedule(
    dir: &Path,
    job: &ScheduledJob,
    occurrence: u64,
    now: u64,
) -> Result<Option<ScheduledJob>> {
    let Some(every) = job.every_ms else {
        return Ok(None);
    };
    if is_cancelled(dir, &job.id)? {
        return Ok(None);
    }
    let pending = dir.join("pending").join(format!("{}.json", job.id));
    if pending.try_exists()? {
        return Ok(None);
    }
    let mut later = occurrence_numbers(dir, "firing", &job.id)?;
    later.extend(occurrence_numbers(dir, "failed", &job.id)?);
    if later.iter().any(|n| *n > occurrence) {
        return Ok(None);
    }
    let next = ScheduledJob {
        fire_count: occurrence,
        due_at: next_recurring_due_at(job.due_at, every, now),
        last_error: None,
        ..job.clone()
    };
    ensure_dir(&dir.join("pending"))?;
    let temp = dir
        .join("pending")
        .join(format!(".schedule-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<bool> {
        private_file(&temp)?.write_all(serialize(&next)?.as_bytes())?;
        match fs::hard_link(&temp, &pending) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.into()),
        }
    })();
    unlink_if_present(&temp)?;
    if !result? {
        return Ok(None);
    }
    if is_cancelled(dir, &job.id)? {
        unlink_if_present(&pending)?;
        return Ok(None);
    }
    Ok(Some(next))
}
pub fn settle_occurrence(
    dir: &Path,
    record: &str,
    job: &ScheduledJob,
    occurrence: u64,
    outcome: std::result::Result<(), String>,
) -> Result<()> {
    if let Err(error) = outcome {
        if !is_cancelled(dir, &job.id)? {
            let failed = dir
                .join("failed")
                .join(format!("{}@{occurrence}.json", job.id));
            write_atomic(
                &failed,
                &serialize(&ScheduledJob {
                    last_error: Some(error),
                    ..job.clone()
                })?,
            )?;
            if is_cancelled(dir, &job.id)? {
                unlink_if_present(&failed)?;
            }
            let mut older = occurrence_numbers(dir, "failed", &job.id)?;
            older.sort_unstable_by(|a, b| b.cmp(a));
            for stale in older.iter().skip(MAX_FAILED_RECORDS_PER_JOB) {
                unlink_if_present(&dir.join("failed").join(format!("{}@{stale}.json", job.id)))?;
            }
        }
    }
    unlink_if_present(&dir.join(record))
}
pub fn prune_tombstones(dir: &Path, listing: &JobListing, now: u64) -> Result<()> {
    for id in names(&dir.join("cancelled"))? {
        if !is_scheduled_job_id(&id)
            || listing
                .jobs
                .iter()
                .any(|record| record.state != ScheduledJobState::Failed && record.job.id == id)
        {
            continue;
        }
        let path = tombstone_path(dir, &id);
        let info = match fs::metadata(&path) {
            Ok(info) => info,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let modified = info
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
        if u128::from(now).saturating_sub(modified.as_millis()) > u128::from(TOMBSTONE_RETENTION_MS)
        {
            unlink_if_present(&path)?;
        }
    }
    Ok(())
}
