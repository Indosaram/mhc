//! Proof-authorized stranded git lock reclamation.
use super::{
    errors::GitError,
    git_activity_lock::{GitActivityProbe, identity_locks_dir_for_repo, probe_git_activity_idle},
    lock_scans::{GitLockScans, ProductionLockScans},
};
use crate::locks::{
    LockRecord, is_lock_owner_proven_dead, memory_writer_lock_path, parse_lock_record,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptGitLock {
    pub path: PathBuf,
    pub reason: &'static str,
}
#[derive(Debug, Default)]
pub struct GitLockSweepReport {
    pub removed: Vec<PathBuf>,
    pub kept: Vec<KeptGitLock>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct LockSnapshot {
    inode: u64,
    modified: Option<SystemTime>,
}
pub fn snapshot(path: &Path) -> Option<LockSnapshot> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    };
    #[cfg(not(unix))]
    let inode = 0;
    Some(LockSnapshot {
        inode,
        modified: meta.modified().ok(),
    })
}
pub fn present_lock_files(git_dir: &Path) -> BTreeMap<PathBuf, LockSnapshot> {
    fn walk(dir: &Path, paths: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, paths);
            } else if path.extension().is_some_and(|ext| ext == "lock") {
                paths.push(path);
            }
        }
    }
    let mut paths: Vec<_> = ["index.lock", "HEAD.lock", "packed-refs.lock", "config.lock"]
        .iter()
        .map(|p| git_dir.join(p))
        .collect();
    walk(&git_dir.join("refs"), &mut paths);
    paths
        .into_iter()
        .filter_map(|p| snapshot(&p).map(|s| (p, s)))
        .collect()
}
pub fn writer_lock_path_for_repo(repo: &Path) -> Option<PathBuf> {
    identity_locks_dir_for_repo(repo).map(|dir| memory_writer_lock_path(&dir))
}
fn authorized_previous_owner(repo: &Path) -> Option<LockRecord> {
    let path = writer_lock_path_for_repo(repo)?;
    let current = parse_lock_record(&std::fs::read_to_string(&path).ok()?)?;
    if current.pid != std::process::id() || current.hostname != crate::support::host::hostname() {
        return None;
    }
    crate::locks::proven_dead_takeovers::proven_dead_takeover_for(&path, &current.nonce)
        .filter(is_lock_owner_proven_dead)
}
fn keep_all(present: &BTreeMap<PathBuf, LockSnapshot>, reason: &'static str) -> GitLockSweepReport {
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
    repo: &Path,
    settle: Duration,
    scans: &dyn GitLockScans,
) -> GitLockSweepReport {
    let present = present_lock_files(&repo.join(".git"));
    if present.is_empty() {
        return GitLockSweepReport::default();
    }
    let Some(previous) = authorized_previous_owner(repo) else {
        return keep_all(&present, "owner-not-proven-dead");
    };
    std::thread::sleep(settle);
    let users = scans.find_repo_users(repo);
    if users.as_ref().is_some_and(|pids| !pids.is_empty()) {
        return keep_all(&present, "repo-in-use");
    }
    let holders: BTreeMap<_, _> = present
        .keys()
        .map(|p| (p.clone(), scans.find_holders(p)))
        .collect();
    let activity = probe_git_activity_idle(repo);
    if matches!(&activity, GitActivityProbe::Busy) {
        return keep_all(&present, "repo-in-use");
    }
    // The exclusive hold is kept alive across every final stat and unlink.
    let vouches = matches!(&activity, GitActivityProbe::Idle { vouches: true, .. })
        && previous.git_activity_lock == Some(true);
    if users.is_none() && !vouches {
        return keep_all(&present, "repo-in-use");
    }
    let mut report = GitLockSweepReport::default();
    for (path, before) in present {
        let holder = holders.get(&path).and_then(|pids| pids.as_ref());
        let reason = if snapshot(&path).as_ref() != Some(&before) {
            Some("changed-during-settle")
        } else if holder.is_some_and(|pids| !pids.is_empty()) || (holder.is_none() && !vouches) {
            Some("open-holder")
        } else if std::fs::remove_file(&path).is_err() {
            Some("unlink-failed")
        } else {
            None
        };
        if let Some(reason) = reason {
            report.kept.push(KeptGitLock { path, reason });
        } else {
            report.removed.push(path);
        }
    }
    drop(activity);
    report
}
/// Explicit, confirmed removal of one inspected lock, never an arbitrary file.
pub fn unlock_stranded_git_lock(
    repo: &Path,
    path: &Path,
    expected: &LockSnapshot,
    scans: &dyn GitLockScans,
) -> Result<(), String> {
    let present = present_lock_files(&repo.join(".git"));
    if present.get(path) != Some(expected) {
        return Err("lock is absent, unsupported, or changed since inspection".into());
    }
    // The inspected owner must be proven dead before its lock may be removed.
    if authorized_previous_owner(repo).is_none() {
        return Err("lock owner is not proven dead".into());
    }
    let activity = probe_git_activity_idle(repo);
    // Mirrors the sweep: an available probe must not report a running git command, and
    // "unavailable" is not evidence of activity, so the scan and snapshot gates below carry
    // the proof. The owner-death gate above is what makes removal safe.
    if matches!(activity, GitActivityProbe::Busy) {
        return Err("repository git activity is still running".into());
    }
    if scans
        .find_repo_users(repo)
        .is_none_or(|pids| !pids.is_empty())
        || scans.find_holders(path).is_none_or(|pids| !pids.is_empty())
    {
        return Err("cannot prove that the repository and lock have no active users".into());
    }
    if snapshot(path).as_ref() != Some(expected) {
        return Err("lock changed since inspection; inspect it again".into());
    }
    let result = std::fs::remove_file(path).map_err(|error| error.to_string());
    drop(activity);
    result
}

pub fn with_stranded_git_lock_recovery<T>(
    repo: &Path,
    mut run: impl FnMut() -> Result<T, GitError>,
) -> Result<T, GitError> {
    match super::config_lock::with_git_lock_retry(&mut run) {
        Ok(value) => Ok(value),
        Err(error) if super::config_lock::is_git_lock_error(&error) => {
            let report = sweep_stranded_git_lock_files(
                repo,
                Duration::from_millis(300),
                &ProductionLockScans,
            );
            if !report.removed.is_empty() {
                return run();
            }
            if report.kept.is_empty() {
                return Err(error);
            }
            Err(GitError::Other(format!(
                "Stranded git locks kept because a git command or session may still be using them: {:?}. Confirm no repository users before /memory unlock.",
                report.kept
            )))
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::locks::{CreateLockRecordOptions, create_lock_record};
    struct Scans {
        users: Option<Vec<u32>>,
        holders: Option<Vec<u32>>,
    }
    impl GitLockScans for Scans {
        fn find_holders(&self, _: &Path) -> Option<Vec<u32>> {
            self.holders.clone()
        }
        fn find_repo_users(&self, _: &Path) -> Option<Vec<u32>> {
            self.users.clone()
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("agents/a/repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/index.lock"), "").unwrap();
        (root, repo)
    }
    fn authorize(repo: &Path, capability: bool) {
        let path = writer_lock_path_for_repo(repo).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let current =
            create_lock_record("memory-write", CreateLockRecordOptions::default()).unwrap();
        let mut previous = current.clone();
        // A joined subprocess supplies an actual dead pid, rather than assuming some pid is free.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        previous.pid = child.id();
        child.wait().unwrap();
        previous.git_activity_lock = capability.then_some(true);
        assert!(is_lock_owner_proven_dead(&previous));
        std::fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
        crate::locks::proven_dead_takeovers::remember_proven_dead_takeover(
            &path,
            &current.nonce,
            previous,
        );
    }
    #[test]
    fn explicit_unlock_removes_only_the_inspected_lock_while_users_remain_proof() {
        let (_root, repo) = fixture();
        // Removal requires a proven-dead owner; without it the lock must stay put.
        let lock = repo.join(".git/index.lock");
        let uninspected = present_lock_files(&repo.join(".git"))
            .remove(&lock)
            .expect("present lock");
        let empty = Scans {
            users: Some(vec![]),
            holders: Some(vec![]),
        };
        assert_eq!(
            unlock_stranded_git_lock(&repo, &lock, &uninspected, &empty),
            Err("lock owner is not proven dead".into())
        );
        assert!(lock.exists(), "an unproven owner must keep the lock");
        authorize(&repo, true);
        let scans = Scans {
            users: Some(vec![]),
            holders: Some(vec![]),
        };
        let inspected = present_lock_files(&repo.join(".git"))
            .remove(&lock)
            .expect("present lock");
        let live = Scans {
            users: Some(vec![123]),
            holders: Some(vec![]),
        };
        assert_eq!(
            unlock_stranded_git_lock(&repo, &lock, &inspected, &live),
            Err("cannot prove that the repository and lock have no active users".into())
        );
        assert!(lock.exists(), "a live user must keep the lock");
        unlock_stranded_git_lock(&repo, &lock, &inspected, &scans).expect("unlock");
        assert!(!lock.exists(), "the inspected lock is removed");
        assert_eq!(
            unlock_stranded_git_lock(&repo, &lock, &inspected, &scans),
            Err("lock is absent, unsupported, or changed since inspection".into())
        );
    }

    #[test]
    fn owner_proof_and_live_users_are_mandatory_gates() {
        let (_root, repo) = fixture();
        let scans = Scans {
            users: Some(vec![]),
            holders: Some(vec![]),
        };
        let report = sweep_stranded_git_lock_files(&repo, Duration::ZERO, &scans);
        assert_eq!(report.kept[0].reason, "owner-not-proven-dead");
        authorize(&repo, true);
        let report = sweep_stranded_git_lock_files(
            &repo,
            Duration::ZERO,
            &Scans {
                users: Some(vec![123]),
                holders: Some(vec![]),
            },
        );
        assert_eq!(report.kept[0].reason, "repo-in-use");
        let hold = super::super::git_activity_lock::hold_git_activity_shared(&repo)
            .unwrap()
            .unwrap();
        let report = sweep_stranded_git_lock_files(&repo, Duration::ZERO, &scans);
        assert_eq!(report.kept[0].reason, "repo-in-use");
        drop(hold);
        assert_eq!(
            sweep_stranded_git_lock_files(&repo, Duration::ZERO, &scans)
                .removed
                .len(),
            1
        );
    }
    #[test]
    fn blind_scans_require_local_kernel_proof_and_previous_capability() {
        let (_root, repo) = fixture();
        let scans = Scans {
            users: None,
            holders: None,
        };
        authorize(&repo, false);
        assert_eq!(
            sweep_stranded_git_lock_files(&repo, Duration::ZERO, &scans).kept[0].reason,
            "repo-in-use"
        );
        authorize(&repo, true);
        let local = super::super::local_filesystem::is_on_local_filesystem(&repo);
        let report = sweep_stranded_git_lock_files(&repo, Duration::ZERO, &scans);
        assert_eq!(report.removed.len(), usize::from(local));
    }
}
