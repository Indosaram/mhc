//! Exclusive file-backed lock acquisition, recovery of dead owners, and scoped execution.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::candidate_sweep::{CandidateSweepOptions, sweep_stale_lock_candidates};
use super::lock_record::{LockRecord, parse_lock_record};
use super::process_identity::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_conflict,
};

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
    metadata: Option<std::fs::Metadata>,
}

fn read_owner(lock_path: &Path) -> std::io::Result<Option<OwnerSnapshot>> {
    let read = || -> std::io::Result<OwnerSnapshot> {
        let raw = std::fs::read_to_string(lock_path)?;
        let metadata = std::fs::metadata(lock_path)?;
        let record = parse_lock_record(&raw);
        Ok(OwnerSnapshot {
            raw,
            record,
            metadata: Some(metadata),
        })
    };
    match read() {
        Ok(owner) => Ok(Some(owner)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        #[cfg(windows)]
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Ok(Some(OwnerSnapshot {
            raw: String::new(),
            record: None,
            metadata: None,
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
        Some(actual) if owner.process_start != "unavailable" => {
            start_identities_conflict(&owner.process_start, &actual)
        }
        _ => false,
    }
}

const INCOMPLETE_LOCK_GRACE_MS: u64 = 5_000;

fn is_same_owner(left: &OwnerSnapshot, right: &OwnerSnapshot) -> bool {
    let (Some(left_meta), Some(right_meta)) = (&left.metadata, &right.metadata) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left_meta.dev() != right_meta.dev() || left_meta.ino() != right_meta.ino() {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if left_meta.creation_time() != right_meta.creation_time() {
            return false;
        }
    }
    left.raw == right.raw
        && matches!((left_meta.modified(), right_meta.modified()), (Ok(a), Ok(b)) if a == b)
}

fn is_recoverable_owner(owner: &OwnerSnapshot, now: std::time::SystemTime) -> bool {
    match &owner.record {
        Some(record) => is_proven_dead(record),
        None => owner
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age.as_millis() > u128::from(INCOMPLETE_LOCK_GRACE_MS)),
    }
}

fn reclaim_stale_recovery_lock(
    recovery_path: &Path,
    now: std::time::SystemTime,
) -> std::io::Result<bool> {
    let Some(stale) = read_owner(recovery_path)? else {
        return Ok(false);
    };
    if !is_recoverable_owner(&stale, now) {
        return Ok(false);
    }
    let tombstone = recovery_path.with_file_name(format!(
        "{}.reaping-{}",
        recovery_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        crate::support::random::random_uuid(),
    ));
    match std::fs::rename(recovery_path, &tombstone) {
        Ok(()) => (),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || super::candidate_sweep::default_is_sharing_error(&error) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    }
    finish_recovery_reap(recovery_path, &tombstone, &stale)
}

fn finish_recovery_reap(
    recovery_path: &Path,
    tombstone: &Path,
    stale: &OwnerSnapshot,
) -> std::io::Result<bool> {
    if read_owner(tombstone)?.is_some_and(|moved| is_same_owner(&moved, stale)) {
        std::fs::remove_file(tombstone)?;
        return Ok(true);
    }
    // A replacement was moved, not the stale inode. Hand it back without overwriting a publisher.
    match std::fs::hard_link(tombstone, recovery_path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    std::fs::remove_file(tombstone)?;
    Ok(false)
}

fn recover_dead_owner(
    lock_path: &Path,
    snapshot: &OwnerSnapshot,
    contender: &LockRecord,
    now: std::time::SystemTime,
) -> std::io::Result<bool> {
    if !is_recoverable_owner(snapshot, now) {
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
        git_activity_lock: contender.git_activity_lock,
    };

    for attempt in 0..2 {
        if read_owner(&recovery_path)?.is_none()
            && publish_exclusive(&recovery_path, &recovery_record)?
        {
            break;
        }
        if attempt > 0 || !reclaim_stale_recovery_lock(&recovery_path, now)? {
            return Ok(false);
        }
    }

    let result = (|| -> std::io::Result<bool> {
        let current = match read_owner(lock_path)? {
            Some(c) => c,
            None => return Ok(true),
        };
        if !is_same_owner(&current, snapshot) {
            return Ok(false);
        }
        if !is_recoverable_owner(&current, now) {
            return Ok(false);
        }
        // The recovery nonce is the fence authorizing removal of the primary.
        let fence = read_owner(&recovery_path)?;
        if fence
            .as_ref()
            .and_then(|owner| owner.record.as_ref())
            .is_none_or(|owner| owner.nonce != recovery_record.nonce)
        {
            return Ok(false);
        }
        match std::fs::remove_file(lock_path) {
            Ok(()) => {
                if let Some(previous) = current.record.filter(is_lock_owner_proven_dead) {
                    PROVEN_DEAD_TAKEOVERS
                        .lock()
                        .expect("takeover proof lock")
                        .insert(lock_path.to_owned(), (contender.nonce.clone(), previous));
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
        }
    })();

    release_lock(&recovery_path, &recovery_record.nonce)?;
    result
}

/// Acquires an exclusive lock, recovering proven-dead owners or incomplete records past grace.
pub fn acquire_lock(
    lock_path: &Path,
    record: &LockRecord,
    options: &AcquireLockOptions,
) -> Result<(), AcquireLockError> {
    acquire_lock_with_clock(lock_path, record, options, std::time::SystemTime::now)
}

fn acquire_lock_with_clock(
    lock_path: &Path,
    record: &LockRecord,
    options: &AcquireLockOptions,
    now: impl Fn() -> std::time::SystemTime,
) -> Result<(), AcquireLockError> {
    let wait_timeout_ms = options.wait_timeout_ms.unwrap_or(0);
    let retry_delay_ms = options.retry_delay_ms.unwrap_or(25);
    if retry_delay_ms == 0 {
        return Err(AcquireLockError::InvalidOptions(
            "lock wait options must be positive".to_string(),
        ));
    }

    let deadline = crate::support::time::now_millis() + wait_timeout_ms as i64;

    // Advisory reclamation of crashed contenders' leaked candidates; a sweep failure never blocks
    // or fails the acquisition itself.
    if let Some(directory) = lock_path.parent() {
        let options = CandidateSweepOptions::default();
        let _ = sweep_stale_lock_candidates(
            directory,
            || crate::support::time::now_millis().max(0) as u64,
            &options,
        );
    }

    let mut previous = None;
    let mut first_attempt = true;
    loop {
        if options.cancellation.map(|f| f()).unwrap_or(false) {
            return Err(AcquireLockError::Aborted);
        }

        // On retries, an existing lock file means the hard-link publish would fail anyway; skip
        // the candidate create + fsync + link + unlink round trip and inspect the owner directly.
        let skip_publish = !first_attempt && lock_path.exists();
        first_attempt = false;
        if !skip_publish && publish_exclusive(lock_path, record)? {
            if let Some(previous) = previous.take() {
                super::proven_dead_takeovers::remember_proven_dead_takeover(
                    lock_path,
                    &record.nonce,
                    previous,
                );
            }
            return Ok(());
        }

        if options.cancellation.map(|f| f()).unwrap_or(false) {
            return Err(AcquireLockError::Aborted);
        }

        // A failed publish may have encountered a different acquisition after the recovered
        // owner. Never carry an earlier takeover's proof across that intervening owner.
        previous = None;
        let owner = match read_owner(lock_path)? {
            Some(o) => o,
            None => continue,
        };

        if recover_dead_owner(lock_path, &owner, record, now())? {
            previous = owner.record.filter(is_lock_owner_proven_dead);
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

/// Proof-based verdict that a lock owner is dead: pid liveness first, then start identity (never age).
pub fn is_lock_owner_proven_dead(owner: &LockRecord) -> bool {
    is_proven_dead(owner)
}

/// Sleeps for `duration_ms`, returning false early when `cancellation` reports an abort.
pub fn delay(duration_ms: u64, cancellation: Option<&dyn Fn() -> bool>) -> bool {
    let deadline = Instant::now() + Duration::from_millis(duration_ms);
    loop {
        if cancellation.map(|probe| probe()).unwrap_or(false) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5).min(remaining));
    }
}

/// Releases an exclusive lock file if the caller holds the matching nonce.
pub fn release_lock<N: LockNonce + ?Sized>(
    lock_path: &Path,
    nonce_source: &N,
) -> Result<bool, std::io::Error> {
    super::proven_dead_takeovers::forget_proven_dead_takeover(lock_path, nonce_source.nonce());
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

// Bound to the contender nonce, so releasing and reacquiring never reuses a dead-owner proof.
static PROVEN_DEAD_TAKEOVERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, (String, LockRecord)>>,
> = std::sync::LazyLock::new(Default::default);
pub fn proven_dead_takeover_for(path: &Path, nonce: &str) -> Option<LockRecord> {
    PROVEN_DEAD_TAKEOVERS
        .lock()
        .expect("takeover proof lock")
        .get(path)
        .filter(|(held, _)| held == nonce)
        .map(|(_, owner)| owner.clone())
}
