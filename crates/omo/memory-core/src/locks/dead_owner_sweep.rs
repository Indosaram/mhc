//! Reclaims `*.lock` files whose recorded owner's pid is proven dead on this host.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::locks::acquire::{
    AcquireLockError, AcquireLockOptions, acquire_without_directory_sweep,
    is_lock_owner_proven_dead, release_lock,
};
use crate::locks::lock_record::{LockRecord, create_lock_record, parse_lock_record};
use crate::locks::process_identity::{ProcessLiveness, get_pid_liveness};
use crate::support::host::hostname;

const LOCK_FILE_SUFFIX: &str = ".lock";

pub type DeadOwnerFailureCallback = Arc<dyn Fn(&Path, &str) + Send + Sync>;
pub type DeadOwnerReclaimedCallback = Arc<dyn Fn(&Path, &LockRecord) + Send + Sync>;

/// Callbacks invoked while sweeping dead-owner locks in a lock directory.
#[derive(Default, Clone)]
pub struct DeadOwnerSweepOptions {
    /// Called for each lock (or the directory) the sweep could not inspect or reclaim; the sweep continues.
    pub on_failure: Option<DeadOwnerFailureCallback>,
    /// Called with the proven-dead owner of each lock the sweep reclaimed.
    pub on_reclaimed: Option<DeadOwnerReclaimedCallback>,
}

fn proven_dead_owner(lock_path: &Path) -> std::io::Result<Option<LockRecord>> {
    let raw = match std::fs::read_to_string(lock_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let Some(owner) = parse_lock_record(&raw) else {
        return Ok(None);
    };
    // A sweep checks only owners whose pid is already gone; a live pid would cost a start-identity
    // probe (a process spawn) per lock on every sweep. A recycled pid is caught by the next contender.
    if owner.hostname != hostname() || get_pid_liveness(owner.pid) != ProcessLiveness::Dead {
        return Ok(None);
    }
    if is_lock_owner_proven_dead(&owner) {
        Ok(Some(owner))
    } else {
        Ok(None)
    }
}

fn reclaim_if_dead_owner(
    lock_path: &Path,
    options: &DeadOwnerSweepOptions,
) -> Result<bool, String> {
    let Ok(meta) = std::fs::symlink_metadata(lock_path) else {
        return Ok(false);
    };
    if !meta.is_file() {
        return Ok(false);
    }
    let Some(previous_owner) = proven_dead_owner(lock_path).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let sweeper =
        create_lock_record("dead-owner-sweep", Default::default()).map_err(|e| e.to_string())?;
    let acquire_opts = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };
    match acquire_without_directory_sweep(lock_path, &sweeper, &acquire_opts) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => return Ok(false),
        Err(err) => return Err(err.to_string()),
    }
    let _ = release_lock(lock_path, &sweeper);
    if let Some(on_reclaimed) = &options.on_reclaimed {
        on_reclaimed(lock_path, &previous_owner);
    }
    Ok(true)
}

/// Reclaims every `*.lock` in `lock_directory` whose recorded owner's pid is gone on this host
/// (using the same proof every contender applies, never age). Returns how many were reclaimed.
pub fn sweep_dead_owner_locks(lock_directory: &Path, options: &DeadOwnerSweepOptions) -> usize {
    let entries = match std::fs::read_dir(lock_directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            if let Some(on_failure) = &options.on_failure {
                on_failure(lock_directory, &err.to_string());
            }
            return 0;
        }
    };
    let mut lock_paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(LOCK_FILE_SUFFIX))
        })
        .collect();
    lock_paths.sort();

    let mut reclaimed = 0usize;
    for lock_path in lock_paths {
        match reclaim_if_dead_owner(&lock_path, options) {
            Ok(true) => reclaimed += 1,
            Ok(false) => {}
            Err(err) => {
                if let Some(on_failure) = &options.on_failure {
                    on_failure(&lock_path, &err);
                }
            }
        }
    }
    reclaimed
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::locks::lock_record::CreateLockRecordOptions;

    use super::*;

    #[test]
    fn reclaims_dead_pid_lock_and_leaves_live_lock() {
        let dir = tempfile::tempdir().unwrap();
        let dead_lock = dir.path().join("dead.lock");
        let live_lock = dir.path().join("live.lock");
        let dead_record = LockRecord {
            pid: 999_999_991,
            nonce: "dead-nonce".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            hostname: crate::support::host::hostname(),
            process_start: "1234".to_owned(),
            purpose: "test-dead".to_owned(),
            run_id: None,
            git_activity_lock: None,
        };
        std::fs::write(&dead_lock, serde_json::to_string(&dead_record).unwrap()).unwrap();

        let live_record =
            create_lock_record("test-live", CreateLockRecordOptions::default()).unwrap();
        std::fs::write(&live_lock, serde_json::to_string(&live_record).unwrap()).unwrap();

        let reclaimed_list = Arc::new(Mutex::new(Vec::<String>::new()));
        let reclaimed_clone = reclaimed_list.clone();
        let count = sweep_dead_owner_locks(
            dir.path(),
            &DeadOwnerSweepOptions {
                on_failure: None,
                on_reclaimed: Some(Arc::new(move |path, owner| {
                    reclaimed_clone.lock().unwrap().push(format!(
                        "{}:{}",
                        path.display(),
                        owner.nonce
                    ));
                })),
            },
        );
        assert_eq!(count, 1);
        assert!(!dead_lock.exists());
        assert!(live_lock.exists());
        assert_eq!(reclaimed_list.lock().unwrap().len(), 1);
    }

    #[test]
    fn missing_directory_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            sweep_dead_owner_locks(
                &dir.path().join("missing"),
                &DeadOwnerSweepOptions::default()
            ),
            0
        );
    }
}
