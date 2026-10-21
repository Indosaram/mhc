//! coding-agent/src/core/extensions/builtin/schedule/types.ts.
use super::{Result, ScheduleError};
use serde::{Deserialize, Serialize};

pub const SCHEDULED_JOB_VERSION: u32 = 1;
pub const MIN_EVERY_SECONDS: u64 = 60;
pub const MAX_SCHEDULE_AHEAD_MS: u64 = 366 * 24 * 60 * 60 * 1000;
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_JOB_FILE_BYTES: u64 = 64 * 1024;
pub const MAX_PENDING_JOBS_PER_SESSION: usize = 50;
pub const MAX_PENDING_JOBS_TOTAL: usize = 1000;
pub const MAX_FAILED_RECORDS_PER_JOB: usize = 10;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJob {
    pub version: u32,
    pub id: String,
    pub session_id: String,
    pub session_file: Option<String>,
    pub cwd: String,
    pub prompt: String,
    pub created_at: u64,
    pub due_at: u64,
    pub every_ms: Option<u64>,
    pub fire_count: u64,
    pub last_fired_at: Option<u64>,
    pub last_error: Option<String>,
}

pub fn is_scheduled_job_id(id: &str) -> bool {
    id.len() == 16
        && id.starts_with("sch_")
        && id.as_bytes()[4..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

pub fn parse_scheduled_job(raw: serde_json::Value) -> Result<ScheduledJob> {
    let job: ScheduledJob = serde_json::from_value(raw)?;
    if job.version != SCHEDULED_JOB_VERSION {
        return Err(ScheduleError::Invalid(format!(
            "unsupported scheduled job version: {}",
            job.version
        )));
    }
    if !is_scheduled_job_id(&job.id) {
        return Err(ScheduleError::Invalid(format!(
            "invalid scheduled job id: {}",
            job.id
        )));
    }
    for (key, value) in [
        ("sessionId", &job.session_id),
        ("cwd", &job.cwd),
        ("prompt", &job.prompt),
    ] {
        if value.is_empty() {
            return Err(ScheduleError::Invalid(format!(
                "scheduled job field \"{key}\" must be a non-empty string"
            )));
        }
    }
    if job.prompt.len() > MAX_PROMPT_BYTES {
        return Err(ScheduleError::Invalid(format!(
            "scheduled job prompt exceeds {MAX_PROMPT_BYTES} bytes"
        )));
    }
    for (key, value) in [
        ("createdAt", Some(job.created_at)),
        ("dueAt", Some(job.due_at)),
        ("fireCount", Some(job.fire_count)),
        ("lastFiredAt", job.last_fired_at),
        ("everyMs", job.every_ms),
    ] {
        if value.is_some_and(|value| value > MAX_SAFE_INTEGER) {
            return Err(ScheduleError::Invalid(format!(
                "scheduled job field \"{key}\" must be a non-negative integer"
            )));
        }
    }
    if let Some(every) = job.every_ms {
        if every < MIN_EVERY_SECONDS * 1000 {
            return Err(ScheduleError::Invalid(format!(
                "scheduled job recurrence must be at least {MIN_EVERY_SECONDS}s"
            )));
        }
        if every > MAX_SCHEDULE_AHEAD_MS {
            return Err(ScheduleError::Invalid(
                "scheduled job recurrence must be at most 366 days".into(),
            ));
        }
    }
    Ok(job)
}

pub fn next_recurring_due_at(due_at: u64, every_ms: u64, now: u64) -> u64 {
    if due_at > now {
        due_at
    } else {
        due_at + ((now - due_at) / every_ms + 1) * every_ms
    }
}

pub fn iso_time(ms: u64) -> Result<String> {
    let timestamp = i64::try_from(ms).map_err(|error| ScheduleError::Invalid(error.to_string()))?;
    chrono::DateTime::from_timestamp_millis(timestamp)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .ok_or_else(|| ScheduleError::Invalid("timestamp outside date range".into()))
}

pub fn format_scheduled_message(job: &ScheduledJob, fired_at: u64) -> Result<String> {
    let recurrence = job.every_ms.map_or_else(String::new, |every| {
        format!(", repeats every {}s", (every + 500) / 1000)
    });
    Ok(format!(
        "[Scheduled prompt {}: created {}, due {}, fired {}{}]\n{}",
        job.id,
        iso_time(job.created_at)?,
        iso_time(job.due_at)?,
        iso_time(fired_at)?,
        recurrence,
        job.prompt
    ))
}
