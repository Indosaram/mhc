//! Exclusive file-backed lock acquisition, recovery of dead owners, and scoped execution.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::lock_record::{LockRecord, parse_lock_record};
use super::process_identity::{ProcessLiveness, get_pid_liveness, get_process_start_identity};

/// Error raised when an exclusive lock is currently held by another owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockContentionError {
    pub lock_path: PathBuf,
    pub owner: Option<LockRecord>,
}

impl LockContentionError {
    /// Constructs a new contention error with the held lock path and current owner record.
    pub fn new(lock_path: PathBuf, owner: Option<LockRecord>) -> Self {
        Self { lock_path, owner }
    }

    /// Indicates whether this lock contention error is retriable (always true).
    pub fn retriable(&self) -> bool {
        true
    }
}

impl fmt::Display for LockContentionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lock is held: {}", self.lock_path.display())
    }
}

impl std::error::Error for LockContentionError {}

/// Errors that can occur when attempting to acquire an exclusive lock.
#[derive(Debug)]
pub enum AcquireLockError {
    Contention(Box<LockContentionError>),
    Aborted,
    InvalidOptions(String),
    Io(std::io::Error),
}

impl fmt::Display for AcquireLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contention(err) => write!(f, "{err}"),
            Self::Aborted => write!(f, "lock acquisition was aborted"),
            Self::InvalidOptions(msg) => write!(f, "{msg}"),
            Self::Io(err) => write!(f, "io error during lock acquisition: {err}"),
        }
    }
}

impl std::error::Error for AcquireLockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Contention(err) => Some(err.as_ref()),
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<LockContentionError> for AcquireLockError {
    fn from(err: LockContentionError) -> Self {
        Self::Contention(Box::new(err))
    }
}

impl From<std::io::Error> for AcquireLockError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Options controlling wait timeouts, retry intervals, and abort cancellation.
#[derive(Default)]
pub struct AcquireLockOptions<'a> {
    pub wait_timeout_ms: Option<u64>,
    pub retry_delay_ms: Option<u64>,
    /// Optional cancellation probe; returns true if acquisition should be aborted.
    pub cancellation: Option<&'a (dyn Fn() -> bool + 'a)>,
}

impl<'a> AcquireLockOptions<'a> {
    /// Creates default lock acquisition options.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Trait for extracting the nonce identifier used to verify lock ownership upon release.
pub trait LockNonce {
    /// Returns the nonce string slice.
    fn nonce(&self) -> &str;
}

impl LockNonce for str {
    fn nonce(&self) -> &str {
        self
    }
}

impl LockNonce for &str {
    fn nonce(&self) -> &str {
        self
    }
}

impl LockNonce for String {
    fn nonce(&self) -> &str {
        self
    }
}

impl LockNonce for LockRecord {
    fn nonce(&self) -> &str {
        &self.nonce
    }
}

struct OwnerSnapshot {
    raw: String,
    record: Option<LockRecord>,
}

fn read_owner(lock_path: &Path) -> std::io::Result<Option<OwnerSnapshot>> {
    match std::fs::read_to_string(lock_path) {
        Ok(raw) => {
            let record = parse_lock_record(&raw);
            Ok(Some(OwnerSnapshot { raw, record }))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        #[cfg(windows)]
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Ok(Some(OwnerSnapshot {
            raw: String::new(),
            record: None,
        })),
        Err(err) => Err(err),
    }
}

fn publish_exclusive(lock_path: &Path, record: &LockRecord) -> std::io::Result<bool> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let uuid = crate::support::random::random_uuid();
    let file_name = lock_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lock".to_string());
    let candidate_path = lock_path.with_file_name(format!("{file_name}.candidate-{uuid}"));

    let write_res = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate_path)?;
        let json = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        use std::io::Write;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    })();

    let link_res = match write_res {
        Ok(()) => match std::fs::hard_link(&candidate_path, lock_path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    };

    if let Err(err) = std::fs::remove_file(&candidate_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(err);
    }

    link_res
}

fn is_proven_dead(owner: &LockRecord) -> bool {
    if owner.hostname != crate::support::host::hostname() {
        return false;
    }
    let liveness = get_pid_liveness(owner.pid);
    if liveness == ProcessLiveness::Dead {
        return true;
    }
    if liveness == ProcessLiveness::Unknown {
        return false;
    }
    let actual_start = get_process_start_identity(owner.pid);
    match actual_start {
        Some(actual) if owner.process_start != "unavailable" => actual != owner.process_start,
        _ => false,
    }
}

fn recover_dead_owner(
    lock_path: &Path,
    snapshot: &OwnerSnapshot,
    contender: &LockRecord,
) -> std::io::Result<bool> {
    let owner_record = match &snapshot.record {
        Some(r) => r,
        None => return Ok(false),
    };
    if !is_proven_dead(owner_record) {
        return Ok(false);
    }

    let file_name = lock_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lock".to_string());
    let recovery_path = lock_path.with_file_name(format!("{file_name}.recovery"));

    let recovery_record = LockRecord {
        pid: contender.pid,
        process_start: contender.process_start.clone(),
        hostname: contender.hostname.clone(),
        nonce: crate::support::random::random_uuid(),
        created_at: crate::support::time::now_iso(),
        purpose: format!("{}:recovery", contender.purpose),
        run_id: contender.run_id.clone(),
    };

    if !publish_exclusive(&recovery_path, &recovery_record)? {
        return Ok(false);
    }

    let result = (|| -> std::io::Result<bool> {
        let current = match read_owner(lock_path)? {
            Some(c) => c,
            None => return Ok(true),
        };
        if current.raw != snapshot.raw {
            return Ok(false);
        }
        let current_record = match &current.record {
            Some(r) => r,
            None => return Ok(false),
        };
        if !is_proven_dead(current_record) {
            return Ok(false);
        }
        match std::fs::remove_file(lock_path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
        }
    })();

    let _ = release_lock(&recovery_path, &recovery_record.nonce);
    result
}

/// Acquires an exclusive lock file, retrying until timeout or recovery of proven-dead owners.
pub fn acquire_lock(
    lock_path: &Path,
    record: &LockRecord,
    options: &AcquireLockOptions,
) -> Result<(), AcquireLockError> {
    let wait_timeout_ms = options.wait_timeout_ms.unwrap_or(0);
    let retry_delay_ms = options.retry_delay_ms.unwrap_or(25);
    if retry_delay_ms == 0 {
        return Err(AcquireLockError::InvalidOptions(
            "lock wait options must be positive".to_string(),
        ));
    }

    let deadline = crate::support::time::now_millis() + wait_timeout_ms as i64;

    loop {
        if options.cancellation.map(|f| f()).unwrap_or(false) {
            return Err(AcquireLockError::Aborted);
        }

        if publish_exclusive(lock_path, record)? {
            return Ok(());
        }

        if options.cancellation.map(|f| f()).unwrap_or(false) {
            return Err(AcquireLockError::Aborted);
        }

        let owner = match read_owner(lock_path)? {
            Some(o) => o,
            None => continue,
        };

        if recover_dead_owner(lock_path, &owner, record)? {
            continue;
        }

        if options.cancellation.map(|f| f()).unwrap_or(false) {
            return Err(AcquireLockError::Aborted);
        }

        let now = crate::support::time::now_millis();
        if now >= deadline {
            return Err(AcquireLockError::Contention(Box::new(
                LockContentionError::new(lock_path.to_path_buf(), owner.record),
            )));
        }

        let remaining = (deadline - now).max(1) as u64;
        let sleep_ms = retry_delay_ms.min(remaining);
        let sleep_dur = Duration::from_millis(sleep_ms);

        if let Some(cancel) = options.cancellation {
            let start = Instant::now();
            while start.elapsed() < sleep_dur {
                if cancel() {
                    return Err(AcquireLockError::Aborted);
                }
                let step = Duration::from_millis(5).min(sleep_dur - start.elapsed());
                std::thread::sleep(step);
            }
        } else {
            std::thread::sleep(sleep_dur);
        }
    }
}

/// Releases an exclusive lock file if the caller holds the matching nonce.
pub fn release_lock<N: LockNonce + ?Sized>(
    lock_path: &Path,
    nonce_source: &N,
) -> Result<bool, std::io::Error> {
    let owner = match read_owner(lock_path)? {
        Some(o) => o,
        None => return Ok(false),
    };
    let owner_nonce = match &owner.record {
        Some(r) => &r.nonce,
        None => return Ok(false),
    };
    if owner_nonce != nonce_source.nonce() {
        return Ok(false);
    }
    match std::fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// Returns true if the lock file currently exists on disk.
pub fn is_held(lock_path: &Path) -> bool {
    lock_path.exists()
}

/// Errors returned by the `with_lock` wrapper.
#[derive(Debug)]
pub enum WithLockError<E> {
    Acquire(AcquireLockError),
    User(E),
}

impl<E: fmt::Display> fmt::Display for WithLockError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquire(err) => write!(f, "lock acquisition failed: {err}"),
            Self::User(err) => write!(f, "{err}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for WithLockError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Acquire(err) => Some(err),
            Self::User(err) => Some(err),
        }
    }
}

/// Executes a closure under exclusive lock ownership and guarantees lock release on return.
pub fn with_lock<T, E, F>(
    lock_path: &Path,
    record: &LockRecord,
    options: &AcquireLockOptions,
    f: F,
) -> Result<T, WithLockError<E>>
where
    F: FnOnce() -> Result<T, E>,
{
    acquire_lock(lock_path, record, options).map_err(WithLockError::Acquire)?;
    struct Guard<'a> {
        path: &'a Path,
        nonce: &'a str,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = release_lock(self.path, self.nonce);
        }
    }
    let _guard = Guard {
        path: lock_path,
        nonce: &record.nonce,
    };
    f().map_err(WithLockError::User)
}

#[cfg(test)]
#[path = "acquire_tests.rs"]
mod tests;
