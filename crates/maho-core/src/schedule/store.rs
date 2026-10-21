//! coding-agent/src/core/extensions/builtin/schedule/store.ts.
pub use super::types::is_scheduled_job_id;
use super::{Result, ScheduleError, types::*};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduledJobState {
    Pending,
    Firing,
    Failed,
}
impl ScheduledJobState {
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Firing => "firing",
            Self::Failed => "failed",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunnerIdentity {
    pub pid: u32,
    pub process_started_at_ms: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct JobRecord {
    pub state: ScheduledJobState,
    pub job: ScheduledJob,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<RunnerIdentity>,
}
#[derive(Clone, Debug, Serialize)]
pub struct InvalidJobFile {
    pub state: ScheduledJobState,
    pub file: String,
    pub error: String,
}
#[derive(Default, Debug, Serialize)]
pub struct JobListing {
    pub jobs: Vec<JobRecord>,
    pub invalid: Vec<InvalidJobFile>,
}

pub fn schedule_dir(agent_dir: &Path) -> PathBuf {
    agent_dir.join("schedule")
}
pub fn ensure_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    Ok(())
}
pub fn private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
pub fn unlink_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn names(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        result.push(entry?.file_name().to_string_lossy().into_owned());
    }
    result.sort();
    Ok(result)
}
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ScheduleError::Invalid("job file has no parent".into()))?;
    ensure_dir(parent)?;
    let temp = parent.join(format!(".schedule-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        private_file(&temp)?.write_all(contents.as_bytes())?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        unlink_if_present(&temp)?;
    }
    result
}
pub fn serialize(job: &ScheduledJob) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(job)?))
}
pub fn read_job_file(path: &Path, expected_id: &str) -> Result<ScheduledJob> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > MAX_JOB_FILE_BYTES {
        return Err(ScheduleError::Invalid(format!(
            "scheduled job file exceeds {MAX_JOB_FILE_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOB_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).is_ok_and(|len| len > MAX_JOB_FILE_BYTES) {
        return Err(ScheduleError::Invalid(format!(
            "scheduled job file exceeds {MAX_JOB_FILE_BYTES} bytes"
        )));
    }
    let raw = serde_json::from_slice(&bytes)
        .map_err(|_| ScheduleError::Invalid("scheduled job file is not valid JSON".into()))?;
    let job = parse_scheduled_job(raw)?;
    if job.id != expected_id {
        return Err(ScheduleError::Invalid(format!(
            "scheduled job id {} does not match its file",
            job.id
        )));
    }
    Ok(job)
}

pub fn list_scheduled_jobs(dir: &Path) -> Result<JobListing> {
    let mut listing = JobListing::default();
    let pattern = regex::Regex::new(r"^(sch_[a-z0-9]{12})(?:@(\d+)(?:~(\d+)-(\d+))?)?\.json$")
        .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
    for state in [
        ScheduledJobState::Pending,
        ScheduledJobState::Firing,
        ScheduledJobState::Failed,
    ] {
        for name in names(&dir.join(state.directory()))? {
            let Some(captures) = pattern.captures(&name) else {
                continue;
            };
            let file = format!("{}/{name}", state.directory());
            let shape_ok = match state {
                ScheduledJobState::Pending => captures.get(2).is_none(),
                ScheduledJobState::Firing => captures.get(3).is_some(),
                ScheduledJobState::Failed => captures.get(2).is_some() && captures.get(3).is_none(),
            };
            if !shape_ok {
                listing.invalid.push(InvalidJobFile {
                    state,
                    file,
                    error: format!("unexpected record name for {}/", state.directory()),
                });
                continue;
            }
            let Some(id) = captures.get(1) else {
                continue;
            };
            let result = (|| -> Result<JobRecord> {
                let occurrence = captures
                    .get(2)
                    .map(|n| n.as_str().parse::<u64>())
                    .transpose()
                    .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
                let owner = match (captures.get(3), captures.get(4)) {
                    (Some(pid), Some(start)) => Some(RunnerIdentity {
                        pid: pid
                            .as_str()
                            .parse()
                            .map_err(|error: std::num::ParseIntError| {
                                ScheduleError::Invalid(error.to_string())
                            })?,
                        process_started_at_ms: start.as_str().parse().map_err(
                            |error: std::num::ParseIntError| {
                                ScheduleError::Invalid(error.to_string())
                            },
                        )?,
                    }),
                    _ => None,
                };
                Ok(JobRecord {
                    state,
                    job: read_job_file(&dir.join(&file), id.as_str())?,
                    file: file.clone(),
                    occurrence,
                    owner,
                })
            })();
            match result {
                Ok(record) => listing.jobs.push(record),
                Err(ScheduleError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => listing.invalid.push(InvalidJobFile {
                    state,
                    file,
                    error: error.to_string(),
                }),
            }
        }
    }
    listing.jobs.sort_by_key(|record| record.job.due_at);
    Ok(listing)
}

#[derive(Clone, Debug)]
pub struct NewScheduledJob {
    pub session_id: String,
    pub session_file: Option<String>,
    pub cwd: String,
    pub prompt: String,
    pub due_at: u64,
    pub every_ms: Option<u64>,
}
pub fn create_scheduled_job(dir: &Path, input: NewScheduledJob, now: u64) -> Result<ScheduledJob> {
    let job = ScheduledJob {
        version: SCHEDULED_JOB_VERSION,
        id: format!("sch_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        session_id: input.session_id,
        session_file: input.session_file,
        cwd: input.cwd,
        prompt: input.prompt,
        created_at: now,
        due_at: input.due_at,
        every_ms: input.every_ms,
        fire_count: 0,
        last_fired_at: None,
        last_error: None,
    };
    let job = parse_scheduled_job(serde_json::to_value(job)?)?;
    write_atomic(
        &dir.join("pending").join(format!("{}.json", job.id)),
        &serialize(&job)?,
    )?;
    Ok(job)
}
pub fn tombstone_path(dir: &Path, id: &str) -> PathBuf {
    dir.join("cancelled").join(id)
}
pub fn is_cancelled(dir: &Path, id: &str) -> Result<bool> {
    match fs::metadata(tombstone_path(dir, id)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
pub fn runners_dir(dir: &Path) -> PathBuf {
    dir.join("runners")
}
