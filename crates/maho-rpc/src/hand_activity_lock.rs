//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-activity-lock.ts`.
//!
//! The memory component's git-activity lock, held by a hand around a `run` (S40 PR 2, design v2.4).
//! Every memory git run holds a SHARED flock on its identity's lock file and hands the descriptor
//! to git so git and everything it starts keep it; the stranded-lock sweep's EXCLUSIVE flock then
//! fails while any of them lives.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use maho_agent::execution::device_files::{DeviceFileError, DeviceFileErrorCode};

pub const GIT_ACTIVITY_LOCK_NAME: &str = "git-activity.flock";
const GIT_ACTIVITY_SHARED_WAIT_MS: u64 = 10_000;
const RETRY_MS: u64 = 5;
pub const LOCK_SH: i32 = 1;
pub const LOCK_NB: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlockOutcome {
    pub rc: i32,
    pub errno: i32,
}

/// `flock(2)` and the errno it left; `None` when this runtime cannot call it at all.
pub type Flock = Arc<dyn Fn(i32, i32) -> FlockOutcome + Send + Sync>;

/// Safe kernel lock backend: `Std` uses `std::fs::File::try_lock_shared` (no `unsafe` FFI), while
/// `Custom` lets tests supply an explicit `Flock` callback.
#[derive(Clone)]
pub enum FlockBackend {
    Std,
    Custom(Flock),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlockClass {
    Busy,
    NoLock,
    Failed,
}

/// The kernel-lock ruling: only `EWOULDBLOCK`/`EAGAIN` is busy (retried); `ENOLCK`, `EOPNOTSUPP`
/// and `ENOSYS` mean no lock is possible here and the run falls back to the pre-lock behaviour;
/// anything else is a typed failure, never idle.
pub fn classify_flock_errno(errno: i32) -> FlockClass {
    #[cfg(unix)]
    {
        if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
            return FlockClass::Busy;
        }
        if errno == libc::ENOLCK || errno == libc::EOPNOTSUPP || errno == libc::ENOSYS {
            return FlockClass::NoLock;
        }
        FlockClass::Failed
    }
    #[cfg(not(unix))]
    {
        let _ = errno;
        FlockClass::NoLock
    }
}

/// `<root>/agents/<id>/repo` -> `<root>/agents/<id>/runtime/locks`; any other directory has no
/// activity lock.
pub fn identity_locks_dir(repo_dir: &Path) -> Option<PathBuf> {
    let repo = std::fs::canonicalize(repo_dir).unwrap_or_else(|_| repo_dir.to_path_buf());
    let file_name = repo.file_name()?.to_str()?;
    if file_name != "repo" {
        return None;
    }
    let agent_dir = repo.parent()?;
    let agents_dir = agent_dir.parent()?;
    if agents_dir.file_name()?.to_str()? != "agents" {
        return None;
    }
    Some(agent_dir.join("runtime").join("locks"))
}

pub struct ActivityHold {
    file: Option<File>,
}

impl ActivityHold {
    pub fn none() -> Self {
        Self { file: None }
    }

    pub fn file(&self) -> Option<&File> {
        self.file.as_ref()
    }

    #[cfg(unix)]
    pub fn fd(&self) -> Option<i32> {
        use std::os::fd::AsRawFd;
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }

    #[cfg(not(unix))]
    pub fn fd(&self) -> Option<i32> {
        None
    }

    pub fn release(mut self) {
        drop(self.file.take());
    }
}

fn open_lock_file(locks_dir: &Path) -> Result<File, DeviceFileError> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(locks_dir.join(GIT_ACTIVITY_LOCK_NAME))
        .map_err(|err| DeviceFileError::new(DeviceFileErrorCode::Io, err.to_string()))
}

/// Takes the shared activity lock for a `run` in `cwd`. The lock file is opened only when its
/// directory already exists (a late git operation must never recreate a removed identity tree, so
/// a missing directory is `not_found`). `None` backend is the `ENOSYS` case: no hold.
pub async fn hold_activity_lock(
    cwd: &Path,
    backend: Option<&FlockBackend>,
    wait_ms: Option<u64>,
) -> Result<ActivityHold, DeviceFileError> {
    let Some(backend) = backend else {
        return Ok(ActivityHold::none());
    };
    let Some(locks_dir) = identity_locks_dir(cwd) else {
        return Ok(ActivityHold::none());
    };
    if !locks_dir.exists() {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::NotFound,
            "the identity's locks directory does not exist",
        ));
    }
    let file = open_lock_file(&locks_dir)?;
    let wait_ms = wait_ms.unwrap_or(GIT_ACTIVITY_SHARED_WAIT_MS);
    let deadline = Instant::now() + Duration::from_millis(wait_ms);

    loop {
        match backend {
            FlockBackend::Std => match file.try_lock_shared() {
                Ok(()) => return Ok(ActivityHold { file: Some(file) }),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(err)) => {
                    let errno = err.raw_os_error().unwrap_or(0);
                    match classify_flock_errno(errno) {
                        FlockClass::NoLock => return Ok(ActivityHold::none()),
                        FlockClass::Busy => {}
                        FlockClass::Failed => {
                            return Err(DeviceFileError::new(
                                DeviceFileErrorCode::Io,
                                format!("flock on the git-activity lock failed (errno {errno})"),
                            ));
                        }
                    }
                }
            },
            FlockBackend::Custom(flock) => {
                #[cfg(unix)]
                let raw_fd = {
                    use std::os::fd::AsRawFd;
                    file.as_raw_fd()
                };
                #[cfg(not(unix))]
                let raw_fd = -1;
                let outcome = flock(raw_fd, LOCK_SH | LOCK_NB);
                if outcome.rc == 0 {
                    return Ok(ActivityHold { file: Some(file) });
                }
                match classify_flock_errno(outcome.errno) {
                    FlockClass::NoLock => return Ok(ActivityHold::none()),
                    FlockClass::Busy => {}
                    FlockClass::Failed => {
                        return Err(DeviceFileError::new(
                            DeviceFileErrorCode::Io,
                            format!(
                                "flock on the git-activity lock failed (errno {})",
                                outcome.errno
                            ),
                        ));
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::Io,
                format!("the git-activity lock stayed exclusively held for {wait_ms} ms"),
            ));
        }
        tokio::time::sleep(Duration::from_millis(RETRY_MS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn resolves_identity_locks_dir_only_under_agents_id_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("agents").join("mem-1").join("repo");
        std::fs::create_dir_all(&repo).expect("create repo");
        let locks = identity_locks_dir(&repo).expect("locks dir");
        assert!(locks.ends_with("agents/mem-1/runtime/locks"));

        let other = dir.path().join("workspace").join("repo");
        std::fs::create_dir_all(&other).expect("create other");
        assert!(identity_locks_dir(&other).is_none());
    }

    #[tokio::test]
    async fn missing_locks_dir_is_not_found_and_no_backend_is_no_hold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("agents").join("mem-1").join("repo");
        std::fs::create_dir_all(&repo).expect("create repo");

        let no_backend = hold_activity_lock(&repo, None, Some(10))
            .await
            .expect("no backend");
        assert!(no_backend.fd().is_none());

        let err = hold_activity_lock(&repo, Some(&FlockBackend::Std), Some(10))
            .await
            .err()
            .expect("missing locks dir");
        assert_eq!(err.code, DeviceFileErrorCode::NotFound);
    }

    #[tokio::test]
    async fn acquires_shared_lock_and_falls_back_on_enosys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("agents").join("mem-1");
        let repo = base.join("repo");
        let locks = base.join("runtime").join("locks");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::create_dir_all(&locks).expect("locks");

        let hold = hold_activity_lock(&repo, Some(&FlockBackend::Std), Some(100))
            .await
            .expect("acquired");
        assert!(hold.file().is_some());
        hold.release();

        let enosys = FlockBackend::Custom(Arc::new(|_, _| FlockOutcome {
            rc: -1,
            errno: libc::ENOSYS,
        }));
        let fallback = hold_activity_lock(&repo, Some(&enosys), Some(10))
            .await
            .expect("enosys fallback");
        assert!(fallback.file().is_none());

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_ref = Arc::clone(&calls);
        let busy_then_ok = FlockBackend::Custom(Arc::new(move |_, _| {
            if calls_ref.fetch_add(1, Ordering::SeqCst) == 0 {
                FlockOutcome {
                    rc: -1,
                    errno: libc::EWOULDBLOCK,
                }
            } else {
                FlockOutcome { rc: 0, errno: 0 }
            }
        }));
        let retried = hold_activity_lock(&repo, Some(&busy_then_ok), Some(200))
            .await
            .expect("retried");
        assert!(retried.file().is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
