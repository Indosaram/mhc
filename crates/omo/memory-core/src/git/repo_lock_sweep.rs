use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, UNIX_EPOCH};

use crate::identity::layout::{AGENTS_DIRNAME, REPO_DIRNAME, RUNTIME_DIRNAME};
use crate::locks::{
    is_lock_owner_proven_dead, memory_writer_lock_path, parse_lock_record, proven_dead_takeover_for,
};

use super::config_lock::{is_git_lock_error, with_git_lock_retry};
use super::errors::GitError;
use super::lock_scans::GitLockScans;

fn env_settle_override() -> Option<u64> {
    let raw = std::env::var("OMO_TEST_GIT_LOCK_SETTLE_MS").ok()?;
    raw.parse::<u64>().ok()
}

pub fn git_lock_settle_ms() -> u64 {
    static SETTLE_MS: OnceLock<u64> = OnceLock::new();
    *SETTLE_MS.get_or_init(|| env_settle_override().unwrap_or(300))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptGitLockReason {
    OwnerNotProvenDead,
    RepoInUse,
    ChangedDuringSettle,
    OpenHolder,
    UnlinkFailed,
}

impl KeptGitLockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnerNotProvenDead => "owner-not-proven-dead",
            Self::RepoInUse => "repo-in-use",
            Self::ChangedDuringSettle => "changed-during-settle",
            Self::OpenHolder => "open-holder",
            Self::UnlinkFailed => "unlink-failed",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::OwnerNotProvenDead => {
                "the process that last held the memory-write lock cannot be proven dead"
            }
            Self::RepoInUse => {
                "a live process is still working in the repository, or that could not be ruled out"
            }
            Self::ChangedDuringSettle => {
                "it changed while it was being checked, so a git process is still using it"
            }
            Self::OpenHolder => {
                "a process holds it open, or open holders could not be determined"
            }
            Self::UnlinkFailed => "it could not be removed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptGitLock {
    pub path: PathBuf,
    pub reason: KeptGitLockReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLockSweepReport {
    pub removed: Vec<PathBuf>,
    pub kept: Vec<KeptGitLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedGitLockError {
    pub kept: Vec<KeptGitLock>,
    pub paths: Vec<PathBuf>,
    pub message: String,
}

impl StrandedGitLockError {
    pub fn new(kept: Vec<KeptGitLock>) -> Self {
        let mut lines = vec!["Stranded git lock file(s) block the memory write:".to_string()];
        for item in &kept {
            lines.push(format!("- {}: {}", item.path.display(), item.reason.description()));
        }
        lines.push(
            "They were kept because a git command or another session may still be using them, and removing a lock that is in use can corrupt the memory repository.".to_string(),
        );
        lines.push(
            "If no git command or other session is working on this memory, re-check and remove each one after confirming:".to_string(),
        );
        for item in &kept {
            lines.push(format!("  /memory unlock {}", item.path.display()));
        }
        let paths = kept.iter().map(|item| item.path.clone()).collect();
        Self {
            kept,
            paths,
            message: lines.join("\n"),
        }
    }
}

impl fmt::Display for StrandedGitLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StrandedGitLockError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockFileSnapshot {
    pub ino: u64,
    pub mtime_ns: i128,
}

fn snapshot_of(meta: &std::fs::Metadata) -> LockFileSnapshot {
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0_u64;

    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0_i128, |d| d.as_nanos() as i128);
    LockFileSnapshot { ino, mtime_ns }
}

fn collect_ref_lock_paths(git_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(names) = crate::fs::read_dir_names(dir) else {
            return;
        };
        for name in names {
            let full = dir.join(&name);
            let Ok(status) = std::fs::symlink_metadata(&full) else {
                continue;
            };
            if status.is_dir() {
                walk(&full, found);
            } else if status.is_file() && name.ends_with(".lock") {
                found.push(full);
            }
        }
    }
    walk(&git_dir.join("refs"), &mut found);
    found
}

pub fn present_lock_files(git_dir: &Path) -> BTreeMap<PathBuf, LockFileSnapshot> {
    let mut candidates = vec![
        git_dir.join("index.lock"),
        git_dir.join("HEAD.lock"),
        git_dir.join("packed-refs.lock"),
        git_dir.join("config.lock"),
    ];
    candidates.extend(collect_ref_lock_paths(git_dir));
    let mut present = BTreeMap::new();
    for candidate in candidates {
        if let Ok(status) = std::fs::symlink_metadata(&candidate)
            && status.is_file()
        {
            present.insert(candidate, snapshot_of(&status));
        }
    }
    present
}

fn settled_verdict(
    candidate: &Path,
    before: LockFileSnapshot,
    scans: &GitLockScans,
) -> Option<KeptGitLockReason> {
    let Ok(status) = std::fs::symlink_metadata(candidate) else {
        return Some(KeptGitLockReason::ChangedDuringSettle);
    };
    if !status.is_file() || snapshot_of(&status) != before {
        return Some(KeptGitLockReason::ChangedDuringSettle);
    }
    let holders = (scans.find_holders)(candidate);
    if holders.as_ref().is_none_or(|list| !list.is_empty()) {
        Some(KeptGitLockReason::OpenHolder)
    } else {
        None
    }
}

pub fn writer_lock_path_for_repo(repo_dir: &Path) -> Option<PathBuf> {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolved = crate::support::paths::resolve_from(&base, repo_dir);
    if resolved.file_name().and_then(|s| s.to_str()) != Some(REPO_DIRNAME) {
        return None;
    }
    let identity_root = resolved.parent()?;
    let agents_dir = identity_root.parent()?;
    if agents_dir.file_name().and_then(|s| s.to_str()) != Some(AGENTS_DIRNAME) {
        return None;
    }
    Some(memory_writer_lock_path(
        &identity_root.join(RUNTIME_DIRNAME).join("locks"),
    ))
}

fn authorized_to_reclaim(repo_dir: &Path) -> bool {
    let Some(writer_lock_path) = writer_lock_path_for_repo(repo_dir) else {
        return false;
    };
    let Ok(raw) = crate::fs::read_to_string(&writer_lock_path) else {
        return false;
    };
    let Some(current) = parse_lock_record(&raw) else {
        return false;
    };
    if current.pid != std::process::id() || current.hostname != crate::support::host::hostname() {
        return false;
    }
    let Some(previous) = proven_dead_takeover_for(&writer_lock_path, &current.nonce) else {
        return false;
    };
    is_lock_owner_proven_dead(&previous)
}

fn keep_all(
    present: &BTreeMap<PathBuf, LockFileSnapshot>,
    reason: KeptGitLockReason,
) -> GitLockSweepReport {
    GitLockSweepReport {
        removed: Vec::new(),
        kept: present
            .keys()
            .map(|path| KeptGitLock {
                path: path.clone(),
                reason,
            })
            .collect(),
    }
}

pub fn sweep_stranded_git_lock_files(
    dir: &Path,
    settle_ms: u64,
    scans: &GitLockScans,
) -> GitLockSweepReport {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolved_dir = crate::support::paths::resolve_from(&base, dir);
    let present = present_lock_files(&resolved_dir.join(".git"));
    if present.is_empty() {
        return GitLockSweepReport {
            removed: Vec::new(),
            kept: Vec::new(),
        };
    }
    if !authorized_to_reclaim(&resolved_dir) {
        return keep_all(&present, KeptGitLockReason::OwnerNotProvenDead);
    }

    std::thread::sleep(Duration::from_millis(settle_ms));
    let users = (scans.find_repo_users)(&resolved_dir);
    if users.as_ref().is_none_or(|list| !list.is_empty()) {
        return keep_all(&present, KeptGitLockReason::RepoInUse);
    }

    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for (candidate, before) in present {
        if let Some(reason) = settled_verdict(&candidate, before, scans) {
            kept.push(KeptGitLock {
                path: candidate,
                reason,
            });
            continue;
        }
        match crate::fs::remove_file(&candidate) {
            Ok(()) => removed.push(candidate),
            Err(_) => kept.push(KeptGitLock {
                path: candidate,
                reason: KeptGitLockReason::UnlinkFailed,
            }),
        }
    }
    GitLockSweepReport { removed, kept }
}

pub fn with_stranded_git_lock_recovery<T, F>(
    dir: &Path,
    mut run: F,
    scans: Option<&GitLockScans>,
) -> Result<T, GitError>
where
    F: FnMut() -> Result<T, GitError>,
{
    match with_git_lock_retry(&mut run) {
        Ok(val) => Ok(val),
        Err(error) => {
            if !is_git_lock_error(&error) {
                return Err(error);
            }
            let default_scans = GitLockScans::production();
            let active_scans = scans.unwrap_or(&default_scans);
            let report = sweep_stranded_git_lock_files(dir, git_lock_settle_ms(), active_scans);
            if report.removed.is_empty() {
                if !report.kept.is_empty() {
                    return Err(GitError::StrandedGitLock(StrandedGitLockError::new(
                        report.kept,
                    )));
                }
                return Err(error);
            }
            run()
        }
    }
}
