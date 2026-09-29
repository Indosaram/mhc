//! Watermark tracking and single-advancement delta notice consumption for soul files.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::{GitLogOptions, GitMemoryRepo};
use crate::locks::acquire::{AcquireLockError, AcquireLockOptions, WithLockError, with_lock};
use crate::locks::domains::notice_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};

use super::paths::SOUL_PATHS;

/// Canonical filename for the soul head watermark record.
pub const SOUL_NOTICE_WATERMARK_FILENAME: &str = "soul-head.json";

/// Notice emitted when an out-of-band change to a soul file advances HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoulNotice {
    pub sha: String,
    pub subject: String,
}

/// Options controlling soul notice delta consumption and lock timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumeSoulNoticeOptions {
    pub notices_dir: PathBuf,
    pub locks_dir: PathBuf,
    pub wait_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SoulHeadWatermark {
    version: u32,
    #[serde(rename = "lastNotifiedHead")]
    last_notified_head: String,
}

/// Errors raised during soul notice delta evaluation and persistence.
#[derive(Debug)]
pub enum WatermarkError {
    Lock(String),
    Io(std::io::Error),
    Git(String),
}

impl fmt::Display for WatermarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(msg) => write!(formatter, "lock error in soul watermark: {msg}"),
            Self::Io(err) => write!(formatter, "io error in soul watermark: {err}"),
            Self::Git(msg) => write!(formatter, "git error in soul watermark: {msg}"),
        }
    }
}

impl std::error::Error for WatermarkError {}

impl From<std::io::Error> for WatermarkError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Consumes new out-of-band soul modifications since the last established watermark.
pub fn consume_soul_notice_delta(
    repo: &GitMemoryRepo,
    options: &ConsumeSoulNoticeOptions,
) -> Result<Option<SoulNotice>, WatermarkError> {
    let record = create_lock_record("soul notice watermark", CreateLockRecordOptions::default())
        .map_err(|e| WatermarkError::Lock(e.to_string()))?;
    let lock_file = notice_lock_path(&options.locks_dir);
    let acquire_options = AcquireLockOptions {
        wait_timeout_ms: Some(options.wait_timeout_ms.unwrap_or(2_000)),
        ..Default::default()
    };

    let result = with_lock(&lock_file, &record, &acquire_options, || {
        consume_under_lock(repo, &options.notices_dir)
    });

    match result {
        Ok(notice) => Ok(notice),
        Err(WithLockError::Acquire(AcquireLockError::Contention(_))) => Ok(None),
        Err(WithLockError::Acquire(err)) => Err(WatermarkError::Lock(err.to_string())),
        Err(WithLockError::User(err)) => Err(err),
    }
}

fn consume_under_lock(
    repo: &GitMemoryRepo,
    notices_dir: &Path,
) -> Result<Option<SoulNotice>, WatermarkError> {
    let head = repo
        .head()
        .map_err(|e| WatermarkError::Git(e.to_string()))?;
    let head = match head {
        Some(h) => h,
        None => return Ok(None),
    };

    let watermark = read_watermark(notices_dir)?;
    let watermark = match watermark {
        Some(w) => w,
        None => {
            write_watermark(notices_dir, &head)?;
            return Ok(None);
        }
    };

    if watermark.last_notified_head == head {
        return Ok(None);
    }

    let range = format!("{}..HEAD", watermark.last_notified_head);
    let log_options = GitLogOptions {
        range: Some(range),
        paths: Some(SOUL_PATHS.iter().map(|s| s.to_string()).collect()),
        ..Default::default()
    };

    let commits = match repo.log(Some(&log_options)) {
        Ok(c) => c,
        Err(_) => {
            write_watermark(notices_dir, &head)?;
            return Ok(None);
        }
    };

    let newest = commits
        .into_iter()
        .find(|c| c.trailers.get("Omo-Writer").map(String::as_str) != Some("memory-tool"));

    match newest {
        Some(commit) => {
            write_watermark(notices_dir, &head)?;
            Ok(Some(SoulNotice {
                sha: commit.sha,
                subject: commit.subject,
            }))
        }
        None => Ok(None),
    }
}

fn read_watermark(notices_dir: &Path) -> Result<Option<SoulHeadWatermark>, WatermarkError> {
    let path = notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(WatermarkError::Io(e)),
    };

    let parsed: Result<SoulHeadWatermark, _> = serde_json::from_str(&raw);
    match parsed {
        Ok(w) if w.version == 1 && is_valid_hex_sha(&w.last_notified_head) => Ok(Some(w)),
        _ => Ok(None),
    }
}

fn write_watermark(notices_dir: &Path, head: &str) -> Result<(), WatermarkError> {
    std::fs::create_dir_all(notices_dir)?;
    let target = notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME);
    let temp_name = format!(
        "{}.tmp-{}",
        SOUL_NOTICE_WATERMARK_FILENAME,
        crate::support::random::random_id()
    );
    let temporary = notices_dir.join(temp_name);

    let watermark = SoulHeadWatermark {
        version: 1,
        last_notified_head: head.to_string(),
    };
    let json = serde_json::to_string(&watermark)
        .map_err(|e| WatermarkError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

    std::fs::write(&temporary, format!("{json}\n"))?;
    std::fs::rename(&temporary, &target)?;
    Ok(())
}

fn is_valid_hex_sha(input: &str) -> bool {
    input.len() == 40 && input.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "watermark_tests.rs"]
mod tests;
