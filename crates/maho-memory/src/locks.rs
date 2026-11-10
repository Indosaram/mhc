//! Lock domains: the named locks the memory store serializes its writers with, and their file paths.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-locks.md`. Two writers holding what they each believe
//! is the same lock is how a memory file ends up with one write silently replacing another, so these
//! paths have to be canonical: the same logical lock must always land on the same file, and two
//! different locks must never collide.
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::sha256_hex16;

const FIXED_DOMAINS: [(&str, &str); 7] = [
    ("memory-write", "memory-write.lock"),
    ("reflection-scheduler", "reflection-scheduler.lock"),
    ("skills-usage", "skills-usage.lock"),
    ("facts-queue", "facts-queue.lock"),
    ("facts-runs", "facts-runs.lock"),
    ("notice", "notice.lock"),
    ("receipts", "receipts.lock"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    #[serde(rename = "receipts")]
    Receipts,
}

impl LockDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemoryWrite => "memory-write",
            Self::ReflectionScheduler => "reflection-scheduler",
            Self::ReflectionFinalize => "reflection-finalize",
            Self::TranscriptState => "transcript-state",
            Self::SkillsUsage => "skills-usage",
            Self::FactsQueue => "facts-queue",
            Self::FactsRuns => "facts-runs",
            Self::Notice => "notice",
            Self::Receipts => "receipts",
        }
    }
}

impl fmt::Display for LockDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockDomainError {
    /// A run id that would escape the lock directory or produce no usable name.
    InvalidRunId(String),
    /// A transcript id of zero length, which has no distinct lock.
    EmptyTranscriptId,
}

impl fmt::Display for LockDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRunId(message) => f.write_str(message),
            Self::EmptyTranscriptId => f.write_str("transcript id must not be empty"),
        }
    }
}

impl std::error::Error for LockDomainError {}

/// Whether a run id can be used verbatim in a file name without escaping the lock directory.
///
/// Leading `.` and `..` are rejected outright rather than sanitized: a run id that only becomes safe
/// after rewriting is a caller bug, and silently renaming it would produce a lock file the caller
/// cannot find again.
fn is_bare_safe(run_id: &str) -> bool {
    let bytes = run_id.as_bytes();
    if bytes.is_empty() || bytes.len() > 80 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn slugify(run_id: &str) -> String {
    let mut slug = String::with_capacity(run_id.len());
    let mut last_was_dash = false;
    for ch in run_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = slug.trim_matches(|ch| ch == '.' || ch == '-');
    let truncated: String = trimmed.chars().take(48).collect();
    if truncated.is_empty() {
        "run".to_owned()
    } else {
        truncated
    }
}

pub fn memory_writer_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("memory-write.lock")
}

pub fn reflection_scheduler_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("reflection-scheduler.lock")
}

pub fn skills_usage_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("skills-usage.lock")
}

pub fn facts_queue_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("facts-queue.lock")
}

pub fn facts_runs_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("facts-runs.lock")
}

pub fn notice_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("notice.lock")
}

pub fn receipts_lock_path(locks_directory: &Path) -> PathBuf {
    locks_directory.join("receipts.lock")
}

/// The lock file guarding one transcript's state.
///
/// Hashed rather than named: a transcript id is a session identifier that can be arbitrarily long and
/// contain separators, and a 16-hex digest is fixed width and filesystem-safe on every platform.
pub fn transcript_state_lock_path(
    locks_directory: &Path,
    transcript_id: &str,
) -> Result<PathBuf, LockDomainError> {
    if transcript_id.is_empty() {
        return Err(LockDomainError::EmptyTranscriptId);
    }
    Ok(locks_directory.join(format!(
        "transcript-state-{}.lock",
        sha256_hex16(transcript_id)
    )))
}

/// The lock file guarding one reflection run's finalization.
///
/// A name that is already safe is used verbatim so a human reading the lock directory can tell which
/// run holds it. Anything else is slugged for readability and suffixed with a digest, because two
/// different run ids must never slug to the same file.
pub fn run_finalization_lock_path(
    locks_directory: &Path,
    run_id: &str,
) -> Result<PathBuf, LockDomainError> {
    let trimmed = run_id.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err(LockDomainError::InvalidRunId(
            "run id must contain a safe identifier".to_owned(),
        ));
    }
    if is_bare_safe(trimmed) {
        return Ok(locks_directory.join(format!("finalize-{trimmed}.lock")));
    }
    Ok(locks_directory.join(format!(
        "finalize-{}-{}.lock",
        slugify(trimmed),
        sha256_hex16(trimmed)
    )))
}

pub fn fixed_lock_paths(locks_directory: &Path) -> Vec<(&'static str, PathBuf)> {
    FIXED_DOMAINS
        .iter()
        .map(|(name, file)| (*name, locks_directory.join(file)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locks() -> PathBuf {
        PathBuf::from("/locks")
    }

    #[test]
    fn the_nine_domains_keep_their_protocol_strings() {
        let names: Vec<&str> = [
            LockDomain::MemoryWrite,
            LockDomain::ReflectionScheduler,
            LockDomain::ReflectionFinalize,
            LockDomain::TranscriptState,
            LockDomain::SkillsUsage,
            LockDomain::FactsQueue,
            LockDomain::FactsRuns,
            LockDomain::Notice,
            LockDomain::Receipts,
        ]
        .iter()
        .map(|domain| domain.as_str())
        .collect();
        assert_eq!(
            names,
            [
                "memory-write",
                "reflection-scheduler",
                "reflection-finalize",
                "transcript-state",
                "skills-usage",
                "facts-queue",
                "facts-runs",
                "notice",
                "receipts",
            ]
        );
    }

    #[test]
    fn a_domain_round_trips_through_json() {
        for domain in [
            LockDomain::MemoryWrite,
            LockDomain::ReflectionFinalize,
            LockDomain::Receipts,
        ] {
            let json = serde_json::to_string(&domain).expect("serialize");
            assert_eq!(json, format!("\"{}\"", domain.as_str()));
            let parsed: LockDomain = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, domain);
        }
    }

    #[test]
    fn a_domain_displays_as_its_protocol_string() {
        assert_eq!(LockDomain::FactsQueue.to_string(), "facts-queue");
    }

    #[test]
    fn fixed_name_lock_paths_are_the_domain_name_with_a_lock_suffix() {
        assert_eq!(
            memory_writer_lock_path(&locks()),
            PathBuf::from("/locks/memory-write.lock")
        );
        assert_eq!(
            receipts_lock_path(&locks()),
            PathBuf::from("/locks/receipts.lock")
        );
        assert_eq!(
            facts_runs_lock_path(&locks()),
            PathBuf::from("/locks/facts-runs.lock")
        );
    }

    #[test]
    fn every_fixed_domain_has_a_distinct_file() {
        let paths = fixed_lock_paths(&locks());
        assert_eq!(paths.len(), 7);
        let mut names: Vec<PathBuf> = paths.iter().map(|(_, path)| path.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn a_transcript_lock_is_a_fixed_width_digest() {
        let path = transcript_state_lock_path(&locks(), "session-abc").expect("path");
        let file = path.file_name().expect("file").to_string_lossy();
        assert!(file.starts_with("transcript-state-"));
        assert!(file.ends_with(".lock"));
        assert_eq!(file.len(), "transcript-state-".len() + 16 + ".lock".len());
    }

    #[test]
    fn two_transcript_ids_never_share_a_lock_file() {
        let first = transcript_state_lock_path(&locks(), "session-a").expect("path");
        let second = transcript_state_lock_path(&locks(), "session-b").expect("path");
        assert_ne!(first, second);
    }

    #[test]
    fn a_transcript_id_with_a_separator_stays_inside_the_lock_directory() {
        let path = transcript_state_lock_path(&locks(), "../../etc/passwd").expect("path");
        assert_eq!(path.parent(), Some(locks().as_path()));
    }

    #[test]
    fn an_empty_transcript_id_is_rejected() {
        assert_eq!(
            transcript_state_lock_path(&locks(), "").unwrap_err(),
            LockDomainError::EmptyTranscriptId
        );
    }

    #[test]
    fn a_safe_run_id_is_used_verbatim() {
        let path = run_finalization_lock_path(&locks(), "run-2026-11-10").expect("path");
        assert_eq!(path, PathBuf::from("/locks/finalize-run-2026-11-10.lock"));
    }

    #[test]
    fn a_run_id_containing_a_separator_is_slugged_and_digested() {
        let path = run_finalization_lock_path(&locks(), "run/with/slashes").expect("path");
        let file = path.file_name().expect("file").to_string_lossy();
        assert!(file.starts_with("finalize-run-with-slashes-"));
        assert_eq!(path.parent(), Some(locks().as_path()));
    }

    #[test]
    fn two_run_ids_that_slug_identically_get_different_files() {
        let first = run_finalization_lock_path(&locks(), "run a").expect("path");
        let second = run_finalization_lock_path(&locks(), "run-a").expect("path");
        assert_ne!(first, second);
    }

    #[test]
    fn a_run_id_of_dots_only_is_rejected() {
        for run_id in ["", "   ", ".", ".."] {
            assert!(
                run_finalization_lock_path(&locks(), run_id).is_err(),
                "expected rejection for {run_id:?}"
            );
        }
    }

    #[test]
    fn a_run_id_with_a_leading_dot_is_not_used_verbatim() {
        let path = run_finalization_lock_path(&locks(), ".hidden").expect("path");
        let file = path.file_name().expect("file").to_string_lossy();
        assert!(file.starts_with("finalize-hidden-"), "got {file}");
    }

    #[test]
    fn a_run_id_longer_than_the_limit_is_hashed_rather_than_truncated_to_a_collision() {
        let long = "a".repeat(81);
        let path = run_finalization_lock_path(&locks(), &long).expect("path");
        let file = path.file_name().expect("file").to_string_lossy();
        assert!(file.contains('-'), "expected a digest suffix, got {file}");
    }

    #[test]
    fn a_run_id_of_only_punctuation_still_produces_a_named_lock() {
        let path = run_finalization_lock_path(&locks(), "!!!").expect("path");
        let file = path.file_name().expect("file").to_string_lossy();
        assert!(file.starts_with("finalize-run-"), "got {file}");
    }

    #[test]
    fn a_korean_run_id_does_not_escape_the_lock_directory() {
        let path = run_finalization_lock_path(&locks(), "실행-2026").expect("path");
        assert_eq!(path.parent(), Some(locks().as_path()));
    }

    #[test]
    fn an_invalid_run_id_message_names_the_requirement() {
        let error = run_finalization_lock_path(&locks(), "").unwrap_err();
        assert!(error.to_string().contains("safe identifier"));
    }
}
