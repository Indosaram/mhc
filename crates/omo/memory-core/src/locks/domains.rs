//! Well-known lock domain definitions and canonical lock-file path resolvers.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::support::sha256::sha256_hex;

/// Canonical strings for the eight well-known lock domains.
pub const LOCK_DOMAINS: [&str; 8] = [
    "memory-write",
    "reflection-scheduler",
    "reflection-finalize",
    "transcript-state",
    "skills-usage",
    "facts-queue",
    "facts-runs",
    "notice",
];

/// The eight well-known lock domains recognised by the memory-core protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LockDomain {
    #[serde(rename = "memory-write")]
    MemoryWrite,
    #[serde(rename = "reflection-scheduler")]
    ReflectionScheduler,
    #[serde(rename = "reflection-finalize")]
    ReflectionFinalize,
    #[serde(rename = "transcript-state")]
    TranscriptState,
    #[serde(rename = "skills-usage")]
    SkillsUsage,
    #[serde(rename = "facts-queue")]
    FactsQueue,
    #[serde(rename = "facts-runs")]
    FactsRuns,
    #[serde(rename = "notice")]
    Notice,
}

impl LockDomain {
    /// Returns the canonical protocol string for this lock domain.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryWrite => "memory-write",
            Self::ReflectionScheduler => "reflection-scheduler",
            Self::ReflectionFinalize => "reflection-finalize",
            Self::TranscriptState => "transcript-state",
            Self::SkillsUsage => "skills-usage",
            Self::FactsQueue => "facts-queue",
            Self::FactsRuns => "facts-runs",
            Self::Notice => "notice",
        }
    }
}

impl fmt::Display for LockDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors raised when constructing domain-specific lock paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockDomainError {
    InvalidRunId(String),
    EmptyTranscriptId,
}

impl fmt::Display for LockDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRunId(msg) => write!(f, "{msg}"),
            Self::EmptyTranscriptId => write!(f, "transcript id must not be empty"),
        }
    }
}

impl std::error::Error for LockDomainError {}

/// Resolves the exclusive lock path for memory-write operations.
pub fn memory_writer_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("memory-write.lock")
}

/// Resolves the exclusive lock path for reflection scheduling.
pub fn reflection_scheduler_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("reflection-scheduler.lock")
}

/// Resolves the exclusive lock path for a specific reflection run finalization.
pub fn run_finalization_lock_path(
    locks_directory: &Path,
    run_id: &str,
) -> Result<PathBuf, LockDomainError> {
    let trimmed = run_id.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err(LockDomainError::InvalidRunId(
            "run id must contain a safe identifier".to_string(),
        ));
    }

    let bytes = trimmed.as_bytes();
    let is_fast_safe = (1..=80).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');

    if is_fast_safe {
        return Ok(locks_directory.join(format!("finalize-{trimmed}.lock")));
    }

    let mut slug = String::with_capacity(trimmed.len());
    let mut last_was_dash = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let trimmed_slug = slug.trim_matches(|c| c == '.' || c == '-');
    let truncated_slug: String = trimmed_slug.chars().take(48).collect();
    let final_slug = if truncated_slug.is_empty() {
        "run"
    } else {
        &truncated_slug
    };

    let digest_hex = sha256_hex(trimmed.as_bytes());
    let digest_16: String = digest_hex.chars().take(16).collect();

    Ok(locks_directory.join(format!("finalize-{final_slug}-{digest_16}.lock")))
}

/// Resolves the exclusive lock path for transcript state tracking.
pub fn transcript_state_lock_path(
    locks_directory: &Path,
    transcript_id: &str,
) -> Result<PathBuf, LockDomainError> {
    if transcript_id.is_empty() {
        return Err(LockDomainError::EmptyTranscriptId);
    }
    let digest_hex = sha256_hex(transcript_id.as_bytes());
    let digest_16: String = digest_hex.chars().take(16).collect();
    Ok(locks_directory.join(format!("transcript-state-{digest_16}.lock")))
}

/// Resolves the exclusive lock path for skills usage aggregation.
pub fn skills_usage_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("skills-usage.lock")
}

/// Resolves the exclusive lock path for facts queue mutation.
pub fn facts_queue_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("facts-queue.lock")
}

/// Resolves the exclusive lock path for facts run namespace serialization.
pub fn facts_runs_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("facts-runs.lock")
}

/// Resolves the exclusive lock path for global notifications.
pub fn notice_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("notice.lock")
}

#[cfg(test)]
#[path = "domains_tests.rs"]
mod tests;
