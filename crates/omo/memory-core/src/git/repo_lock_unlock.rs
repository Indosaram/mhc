use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, ProcessLiveness, acquire_lock,
    create_lock_record, get_pid_liveness, parse_lock_record, release_lock,
};

use super::lock_scans::GitLockScans;
use super::repo_lock_sweep::{present_lock_files, writer_lock_path_for_repo};

pub type UnlockScans = GitLockScans;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterState {
    None,
    Alive { pid: u32 },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockVerdict {
    Refuse,
    Confirm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockFindings {
    pub lock_path: PathBuf,
    pub identity: String,
    pub holders: Option<Vec<u32>>,
    pub repo_users: Option<Vec<u32>>,
    pub writer: WriterState,
    pub writer_lock_path: Option<PathBuf>,
    pub verdict: UnlockVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockInspection {
    NotAGitLock,
    Missing,
    Found(UnlockFindings),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockOutcome {
    Removed { lock_path: PathBuf },
    Refused { inspection: UnlockFindings },
    Changed { lock_path: PathBuf },
    Missing,
    NotAGitLock,
    UnlinkFailed { lock_path: PathBuf, message: String },
}

fn memory_writer_state(repo_dir: &Path, own_nonce: Option<&str>) -> WriterState {
    let Some(writer_lock_path) = writer_lock_path_for_repo(repo_dir) else {
        return WriterState::None;
    };
    let raw = match crate::fs::read_to_string(&writer_lock_path) {
        Ok(raw) => raw,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                WriterState::None
            } else {
                WriterState::Unknown
            };
        }
    };
    let Some(record) = parse_lock_record(&raw) else {
        return WriterState::Unknown;
    };
    if record.hostname != crate::support::host::hostname() {
        return WriterState::Unknown;
    }
    if own_nonce.is_some_and(|nonce| record.nonce == nonce) {
        return WriterState::None;
    }
    match get_pid_liveness(record.pid) {
        ProcessLiveness::Alive => WriterState::Alive { pid: record.pid },
        ProcessLiveness::Dead => WriterState::None,
        ProcessLiveness::Unknown => WriterState::Unknown,
    }
}

fn file_identity(status: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    let (dev, ino, size, mtime_ns, ctime_ns) = {
        use std::os::unix::fs::MetadataExt;
        (
            status.dev(),
            status.ino(),
            status.size(),
            status.mtime() as i128 * 1_000_000_000 + status.mtime_nsec() as i128,
            status.ctime() as i128 * 1_000_000_000 + status.ctime_nsec() as i128,
        )
    };
    #[cfg(not(unix))]
    let (dev, ino, size, mtime_ns, ctime_ns) = {
        let mtime_ns = status
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0_i128, |d| d.as_nanos() as i128);
        (0_u64, 0_u64, status.len(), mtime_ns, mtime_ns)
    };
    let birthtime_ns = status
        .created()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0_i128, |d| d.as_nanos() as i128);
    format!("{dev}:{ino}:{size}:{mtime_ns}:{ctime_ns}:{birthtime_ns}")
}

pub fn inspect_stranded_git_lock(
    repo_dir: &Path,
    lock_path: &Path,
    scans: Option<&UnlockScans>,
    own_writer_nonce: Option<&str>,
) -> UnlockInspection {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let target = crate::support::paths::resolve_from(&base, lock_path);
    let resolved_repo = crate::support::paths::resolve_from(&base, repo_dir);
    let git_dir = resolved_repo.join(".git");
    let present = present_lock_files(&git_dir);
    if !present.contains_key(&target) {
        let status = std::fs::symlink_metadata(&target).ok();
        return if status.is_none()
            && target != git_dir
            && target.starts_with(&git_dir)
            && target.to_string_lossy().ends_with(".lock")
        {
            UnlockInspection::Missing
        } else {
            UnlockInspection::NotAGitLock
        };
    }
    let Ok(status) = std::fs::symlink_metadata(&target) else {
        return UnlockInspection::Missing;
    };
    let default_scans = UnlockScans::production();
    let active_scans = scans.unwrap_or(&default_scans);
    let holders = (active_scans.find_holders)(&target);
    let repo_users = (active_scans.find_repo_users)(repo_dir);
    let writer = memory_writer_state(repo_dir, own_writer_nonce);
    let visibly_live = holders.as_ref().is_some_and(|list| !list.is_empty())
        || repo_users.as_ref().is_some_and(|list| !list.is_empty())
        || writer != WriterState::None;
    UnlockInspection::Found(UnlockFindings {
        lock_path: target,
        identity: file_identity(&status),
        holders,
        repo_users,
        writer,
        writer_lock_path: writer_lock_path_for_repo(repo_dir),
        verdict: if visibly_live {
            UnlockVerdict::Refuse
        } else {
            UnlockVerdict::Confirm
        },
    })
}

pub fn unlock_stranded_git_lock(
    repo_dir: &Path,
    confirmed: &UnlockFindings,
    scans: Option<&UnlockScans>,
) -> Result<UnlockOutcome, AcquireLockError> {
    let Some(writer_lock_path) = writer_lock_path_for_repo(repo_dir) else {
        return Ok(remove_if_unchanged(repo_dir, confirmed, scans, None));
    };
    let record = create_lock_record("memory-write", CreateLockRecordOptions::default())
        .map_err(|err| AcquireLockError::InvalidOptions(err.to_string()))?;
    match acquire_lock(&writer_lock_path, &record, &AcquireLockOptions::default()) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(err)) => {
            let writer = match err.owner {
                Some(owner) => WriterState::Alive { pid: owner.pid },
                None => WriterState::Unknown,
            };
            return Ok(UnlockOutcome::Refused {
                inspection: UnlockFindings {
                    writer,
                    verdict: UnlockVerdict::Refuse,
                    ..confirmed.clone()
                },
            });
        }
        Err(other) => return Err(other),
    }
    let outcome = remove_if_unchanged(repo_dir, confirmed, scans, Some(&record.nonce));
    let _ = release_lock(&writer_lock_path, &record);
    Ok(outcome)
}

fn remove_if_unchanged(
    repo_dir: &Path,
    confirmed: &UnlockFindings,
    scans: Option<&UnlockScans>,
    own_writer_nonce: Option<&str>,
) -> UnlockOutcome {
    let fresh = inspect_stranded_git_lock(repo_dir, &confirmed.lock_path, scans, own_writer_nonce);
    let UnlockInspection::Found(fresh) = fresh else {
        return match fresh {
            UnlockInspection::Missing => UnlockOutcome::Missing,
            UnlockInspection::NotAGitLock => UnlockOutcome::NotAGitLock,
            UnlockInspection::Found(_) => UnlockOutcome::NotAGitLock,
        };
    };
    if fresh.identity != confirmed.identity {
        return UnlockOutcome::Changed {
            lock_path: fresh.lock_path,
        };
    }
    if fresh.verdict == UnlockVerdict::Refuse {
        return UnlockOutcome::Refused { inspection: fresh };
    }
    match crate::fs::remove_file(&fresh.lock_path) {
        Ok(()) => UnlockOutcome::Removed {
            lock_path: fresh.lock_path,
        },
        Err(error) => UnlockOutcome::UnlinkFailed {
            lock_path: fresh.lock_path,
            message: error.to_string(),
        },
    }
}
