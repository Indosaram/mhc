//! Inter-process and in-process lock primitives for transcript journal state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::journal::store::JournalError;

/// Maximum duration in milliseconds to retry acquiring a lock before timing out.
pub const ACQUISITION_WAIT_MS: u64 = 5_000;

/// Delay in milliseconds between retry attempts when lock is contested.
pub const RETRY_DELAY_MS: u64 = 10;

/// Error raised when lock acquisition wait deadline expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalLockTimeoutError {
    pub lock_path: PathBuf,
    pub retriable: bool,
}

impl JournalLockTimeoutError {
    /// Create a new timeout error for the given lock path.
    pub fn new(lock_path: impl Into<PathBuf>) -> Self {
        Self {
            lock_path: lock_path.into(),
            retriable: true,
        }
    }
}

impl std::fmt::Display for JournalLockTimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Timed out acquiring transcript journal lock: {}",
            self.lock_path.display()
        )
    }
}

impl std::error::Error for JournalLockTimeoutError {}

/// Process liveness status.
pub use crate::locks::process_identity::ProcessLiveness;

/// Check whether a process ID is currently alive.
pub fn get_pid_liveness(pid: u32) -> ProcessLiveness {
    if pid == 0 {
        return ProcessLiveness::Dead;
    }
    crate::locks::process_identity::get_pid_liveness(pid)
}

/// Read an OS-specific start identity for process reuse detection.
pub fn get_process_start_identity(pid: u32) -> Option<String> {
    crate::locks::process_identity::get_process_start_identity(pid)
}

struct OwnerPayload {
    pid: u32,
    start_identity: String,
}

fn parse_owner(raw: &str) -> Option<OwnerPayload> {
    let mut lines = raw.trim_end().split('\n');
    let pid_text = lines.next()?.trim();
    let pid: u32 = pid_text.parse().ok()?;
    if pid == 0 {
        return None;
    }
    let start_identity = lines.next().unwrap_or("").to_string();
    Some(OwnerPayload {
        pid,
        start_identity,
    })
}

fn is_owner_dead(owner: &OwnerPayload) -> bool {
    if get_pid_liveness(owner.pid) == ProcessLiveness::Dead {
        return true;
    }
    if owner.start_identity.is_empty() {
        return false;
    }
    let actual = get_process_start_identity(owner.pid);
    actual.is_some() && actual.as_ref() != Some(&owner.start_identity)
}

/// Attempt to reclaim a stale lock file left by a dead process or abandoned mid-acquisition.
pub fn try_reclaim_stale_lock(lock_path: &Path) {
    let snapshot = match std::fs::read_to_string(lock_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let owner = parse_owner(&snapshot);
    if let Some(owner) = owner {
        if !is_owner_dead(&owner) {
            return;
        }
    } else {
        let metadata = match std::fs::metadata(lock_path) {
            Ok(m) => m,
            Err(_) => return,
        };
        let mtime_millis = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        let now = crate::support::time::now_millis() as u64;
        match mtime_millis {
            Some(mtime) if now.saturating_sub(mtime) <= ACQUISITION_WAIT_MS => return,
            None => return,
            _ => {}
        }
    }
    let current = match std::fs::read_to_string(lock_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    if current != snapshot {
        return;
    }
    let _ = std::fs::remove_file(lock_path);
}

fn local_path_mutex(path: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap();
    let key = path.to_path_buf();
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

struct LockFileGuard<'a> {
    file: Option<std::fs::File>,
    path: &'a Path,
}

impl<'a> Drop for LockFileGuard<'a> {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(self.path);
    }
}

fn acquire_and_run<T>(
    lock_path: &Path,
    task: impl FnOnce() -> Result<T, JournalError>,
    cancel: Option<&dyn Fn() -> bool>,
    wait_ms: u64,
) -> Result<T, JournalError> {
    let deadline = crate::support::time::now_millis() as u64 + wait_ms;
    let file = loop {
        if let Some(c) = cancel
            && c()
        {
            return Err(JournalError::Aborted);
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(lock_path) {
            Ok(f) => break f,
            Err(err) => {
                let is_exist = err.kind() == std::io::ErrorKind::AlreadyExists;
                let is_past_deadline = wait_ms > 0 && (crate::support::time::now_millis() as u64) >= deadline;
                if !is_exist {
                    return Err(JournalError::Io(err));
                }
                if wait_ms == 0 {
                    try_reclaim_stale_lock(lock_path);
                    match options.open(lock_path) {
                        Ok(f) => break f,
                        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Err(JournalError::LockTimeout(JournalLockTimeoutError::new(lock_path))),
                        Err(err) => return Err(JournalError::Io(err)),
                    }
                }
                if is_past_deadline {
                    return Err(JournalError::LockTimeout(JournalLockTimeoutError::new(lock_path)));
                }
                if let Some(c) = cancel
                    && c()
                {
                    return Err(JournalError::Aborted);
                }
                try_reclaim_stale_lock(lock_path);
                if let Some(c) = cancel
                    && c()
                {
                    return Err(JournalError::Aborted);
                }
                std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
            }
        }
    };

    let mut guard = LockFileGuard {
        file: Some(file),
        path: lock_path,
    };
    let pid = std::process::id();
    let start_identity = get_process_start_identity(pid).unwrap_or_default();
    let payload = format!("{pid}\n{start_identity}\n");
    if let Some(ref mut f) = guard.file {
        use std::io::Write;
        f.write_all(payload.as_bytes()).map_err(JournalError::Io)?;
        f.flush().map_err(JournalError::Io)?;
    }
    if let Some(c) = cancel
        && c()
    {
        return Err(JournalError::Aborted);
    }
    task()
}

/// Execute a task holding both an in-process lock and an on-disk lock file.
pub fn with_local_journal_lock<T>(
    lock_path: &Path,
    task: impl FnOnce() -> Result<T, JournalError>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<T, JournalError> {
    let mutex = local_path_mutex(lock_path);
    let _in_process_guard = loop {
        if let Some(c) = cancel
            && c()
        {
            return Err(JournalError::Aborted);
        }
        match mutex.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    };

    if let Some(c) = cancel
        && c()
    {
        return Err(JournalError::Aborted);
    }

    acquire_and_run(lock_path, task, cancel, ACQUISITION_WAIT_MS)
}

pub fn try_with_local_journal_lock<T>(
    lock_path: &Path,
    task: impl FnOnce() -> Result<T, JournalError>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<T, JournalError> {
    let mutex = local_path_mutex(lock_path);
    let _in_process_guard = match mutex.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return Err(JournalError::LockTimeout(JournalLockTimeoutError::new(lock_path)));
        }
    };
    if cancel.is_some_and(|c| c()) { return Err(JournalError::Aborted); }
    acquire_and_run(lock_path, task, cancel, 0)
}

pub fn with_journal_lock_without_wait<T>(
    lock_path: &Path,
    task: impl FnOnce() -> Result<T, JournalError>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<T, JournalError> {
    acquire_and_run(lock_path, task, cancel, 0)
}

/// Abstract lock contract for journal state operations.
pub trait JournalLock: Send + Sync {
    /// Execute a task within the lock boundary.
    fn with_lock(
        &self,
        lock_path: &Path,
        task: &mut dyn FnMut() -> Result<(), JournalError>,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(), JournalError>;
}

impl<F> JournalLock for F
where
    F: Fn(
            &Path,
            &mut dyn FnMut() -> Result<(), JournalError>,
            Option<&dyn Fn() -> bool>,
        ) -> Result<(), JournalError>
        + Send
        + Sync,
{
    fn with_lock(
        &self,
        lock_path: &Path,
        task: &mut dyn FnMut() -> Result<(), JournalError>,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(), JournalError> {
        (self)(lock_path, task, cancel)
    }
}

/// Default file and local mutex based journal lock.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultJournalLock;

impl JournalLock for DefaultJournalLock {
    fn with_lock(
        &self,
        lock_path: &Path,
        task: &mut dyn FnMut() -> Result<(), JournalError>,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(), JournalError> {
        with_local_journal_lock(lock_path, task, cancel)
    }
}

/// Helper to execute any generic task through a [`JournalLock`] trait object.
pub fn run_with_lock<T>(
    lock: &dyn JournalLock,
    lock_path: &Path,
    task: impl FnOnce() -> Result<T, JournalError>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<T, JournalError> {
    let mut task_opt = Some(task);
    let mut result_opt = None;
    lock.with_lock(
        lock_path,
        &mut || {
            let t = task_opt.take().expect("task executed once");
            let res = t()?;
            result_opt = Some(res);
            Ok(())
        },
        cancel,
    )?;
    Ok(result_opt.expect("result must be set after task execution"))
}
