//! File-backed exclusive locks: acquiring, releasing only your own, and reclaiming dead owners.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-lock-acquire.md`.
//!
//! A lock file is the only thing stopping two processes from rewriting the same memory file. Its
//! two failure modes are asymmetric and both silent:
//!
//! - **Two owners at once** — a memory write is replaced by a concurrent one, with no error anywhere.
//! - **A dead owner that never yields** — every later writer waits forever on a lock nobody holds.
//!
//! So publication is atomic (`hard_link` of a fully written candidate, which fails with `AlreadyExists`
//! rather than overwriting), release checks the nonce, and reclamation needs positive proof the owner
//! is dead rather than merely old.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::lock_record::{LockRecord, parse_lock_record};

/// How long an unreadable/partial lock file is left alone before it may be reclaimed.
///
/// A file with no parseable record is normally a publisher mid-write — it created the file and has not
/// finished the body. Treating that as stale would delete a lock that is about to be held.
pub const INCOMPLETE_LOCK_GRACE_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockContention {
    pub lock_path: PathBuf,
    /// The holder as recorded. `None` means the file exists but its record is unreadable.
    pub owner: Option<LockRecord>,
}

impl std::fmt::Display for LockContention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lock is held: {}", self.lock_path.display())
    }
}

impl std::error::Error for LockContention {}

#[derive(Debug)]
pub enum AcquireError {
    Contention(Box<LockContention>),
    Io(io::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contention(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "io error during lock acquisition: {error}"),
        }
    }
}

impl std::error::Error for AcquireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Contention(error) => Some(error.as_ref()),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for AcquireError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Whether `pid` is still alive, as far as this host can tell.
///
/// Injected rather than called directly so dead-owner reclamation is testable without spawning and
/// killing real processes.
pub trait Liveness {
    fn is_alive(&self, record: &LockRecord) -> bool;
}

/// Which process a stale-owner decision is being made on behalf of.
pub trait LocalHost {
    fn hostname(&self) -> String;
}

/// The three answers a stale-owner check can give. `Unknown` is not `Dead`: reclaiming a lock whose
/// owner might still be running destroys mutual exclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Alive,
    ProvenDead,
    Unknown,
}

/// Decide whether `owner` may be reclaimed.
///
/// A lock held on another host is never reclaimed: this process cannot observe that host's process
/// table, so it cannot prove anything about the owner. A record with no parseable owner falls back to
/// the file's age.
pub fn freshness(
    owner: Option<&LockRecord>,
    liveness: Option<&dyn Liveness>,
    local: &dyn LocalHost,
    lock_age: Option<Duration>,
) -> Freshness {
    let Some(owner) = owner else {
        return match lock_age {
            Some(age) if age.as_millis() > u128::from(INCOMPLETE_LOCK_GRACE_MS) => {
                Freshness::ProvenDead
            }
            _ => Freshness::Unknown,
        };
    };
    if owner.hostname != local.hostname() {
        return Freshness::Unknown;
    }
    match liveness {
        Some(probe) if probe.is_alive(owner) => Freshness::Alive,
        // Without a liveness probe the only honest answer about a parseable owner is "cannot tell".
        Some(_) => Freshness::ProvenDead,
        None => Freshness::Unknown,
    }
}

/// Serialize `record` into a candidate file and publish it with a hard link.
///
/// Returns `Ok(false)` when another owner published first. The candidate is always removed, including
/// on failure, so a failed acquire leaves no `*.candidate-*` debris for the next sweep to reason about.
pub fn publish(lock_path: &Path, record: &LockRecord, nonce: &str) -> io::Result<bool> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let base = lock_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lock".to_owned());
    let candidate = lock_path.with_file_name(format!("{base}.candidate-{nonce}"));

    let written = write_candidate(&candidate, record);
    let linked = written.and_then(|()| match std::fs::hard_link(&candidate, lock_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    });

    match std::fs::remove_file(&candidate) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    linked
}

fn write_candidate(path: &Path, record: &LockRecord) -> io::Result<()> {
    let json = serde_json::to_string(record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

pub fn read_owner(lock_path: &Path) -> io::Result<Option<Option<LockRecord>>> {
    let raw = match std::fs::read_to_string(lock_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        // Windows reports a file another process holds open this way. The lock exists; the record is
        // simply not readable right now.
        #[cfg(windows)]
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(Some(None)),
        Err(error) => return Err(error),
    };
    Ok(Some(parse_lock_record(&raw)))
}

pub fn lock_age(lock_path: &Path, now: SystemTime) -> Option<Duration> {
    std::fs::metadata(lock_path)
        .ok()?
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
}

/// Remove `lock_path` only if it still holds `nonce`.
///
/// The nonce check is the whole point: a release that removed the file unconditionally would delete a
/// successor's lock after this holder's lease had already been reclaimed as stale, handing the lock to
/// a third process while the second still believes it is exclusive.
pub fn release(lock_path: &Path, nonce: &str) -> io::Result<bool> {
    let Some(owner) = read_owner(lock_path)? else {
        return Ok(false);
    };
    let holds_nonce = owner.as_ref().is_some_and(|record| record.nonce == nonce);
    if !holds_nonce {
        return Ok(false);
    }
    match std::fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn try_acquire(
    lock_path: &Path,
    record: &LockRecord,
    liveness: &dyn Liveness,
    local: &dyn LocalHost,
    now: SystemTime,
) -> Result<(), AcquireError> {
    for _ in 0..2 {
        if publish(lock_path, record, &record.nonce)? {
            return Ok(());
        }
        let owner = read_owner(lock_path)?;
        let age = lock_age(lock_path, now);
        let state = freshness(
            owner.as_ref().and_then(|owner| owner.as_ref()),
            Some(liveness),
            local,
            age,
        );
        if state != Freshness::ProvenDead {
            return Err(AcquireError::Contention(Box::new(LockContention {
                lock_path: lock_path.to_owned(),
                owner: owner.flatten(),
            })));
        }
        // The owner is dead, so removing its file cannot steal a live lock. A concurrent reclaimer may
        // have won the race; the loop retries the link once and then reports contention truthfully.
        match std::fs::remove_file(lock_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(AcquireError::Io(error)),
        }
    }
    Err(AcquireError::Contention(Box::new(LockContention {
        lock_path: lock_path.to_owned(),
        owner: None,
    })))
}

pub fn is_held(lock_path: &Path) -> bool {
    lock_path.exists()
}

pub enum WithLockError<E> {
    Acquire(AcquireError),
    Body(E),
}

impl<E: std::fmt::Display> std::fmt::Display for WithLockError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Acquire(error) => write!(f, "{error}"),
            Self::Body(error) => write!(f, "{error}"),
        }
    }
}

pub fn with_lock<T, E>(
    lock_path: &Path,
    record: &LockRecord,
    liveness: &dyn Liveness,
    local: &dyn LocalHost,
    now: SystemTime,
    body: impl FnOnce() -> Result<T, E>,
) -> Result<T, WithLockError<E>> {
    try_acquire(lock_path, record, liveness, local, now).map_err(WithLockError::Acquire)?;
    let result = body();
    // A release failure must not mask the body's own outcome: the body's error is what the caller acts
    // on, and the lock is reclaimed by the stale sweep once its owner is gone.
    let _ = release(lock_path, &record.nonce);
    result.map_err(WithLockError::Body)
}

/// An `OpenOptions` handle an OS-level advisory lock would be taken on.
///
/// Kept separate from [`publish`] because the two answer different questions: the record file says
/// *who* holds the lock (needed for diagnostics and dead-owner recovery), the OS lock says whether a
/// holder is still alive (needed when a process dies without releasing).
pub fn open_lock_handle(lock_path: &Path) -> io::Result<File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    struct FixedHost(&'static str);

    impl LocalHost for FixedHost {
        fn hostname(&self) -> String {
            self.0.to_owned()
        }
    }

    struct DeadPids(HashSet<u32>);

    impl Liveness for DeadPids {
        fn is_alive(&self, record: &LockRecord) -> bool {
            !self.0.contains(&record.pid)
        }
    }

    struct Panics;

    impl Liveness for Panics {
        fn is_alive(&self, _record: &LockRecord) -> bool {
            panic!("liveness must not be probed for this case");
        }
    }

    fn record(nonce: &str, pid: u32) -> LockRecord {
        LockRecord {
            pid,
            process_start: "start-token".to_owned(),
            hostname: "host-a".to_owned(),
            nonce: nonce.to_owned(),
            created_at: "2026-11-10T00:00:00.000Z".to_owned(),
            purpose: "memory-write".to_owned(),
            run_id: None,
            git_activity_lock: None,
        }
    }

    fn local() -> FixedHost {
        FixedHost("host-a")
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn publishing_creates_the_lock_and_records_the_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("locks/memory-write.lock");
        assert!(publish(&path, &record("n1", 1), "n1").expect("publish"));
        let owner = read_owner(&path).expect("read").expect("present");
        assert_eq!(owner.expect("record").nonce, "n1");
    }

    #[test]
    fn a_second_publish_reports_contention_without_overwriting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        assert!(publish(&path, &record("first", 1), "first").expect("publish"));
        assert!(!publish(&path, &record("second", 2), "second").expect("publish"));
        let owner = read_owner(&path)
            .expect("read")
            .expect("present")
            .expect("record");
        assert_eq!(owner.nonce, "first");
    }

    #[test]
    fn publishing_leaves_no_candidate_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("n1", 1), "n1").expect("publish");
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["memory-write.lock"]);
    }

    #[test]
    fn a_lost_link_race_also_leaves_no_candidate_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("winner", 1), "winner").expect("first");
        assert!(!publish(&path, &record("loser", 2), "loser").expect("second"));
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["memory-write.lock"]);
    }

    #[test]
    fn reading_an_absent_lock_reports_absence() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read_owner(&dir.path().join("nothing.lock")).expect("read"),
            None
        );
    }

    #[test]
    fn reading_a_lock_with_an_unparseable_body_reports_a_present_but_unknown_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("partial.lock");
        std::fs::write(&path, "{\"pid\": 12").expect("write");
        let owner = read_owner(&path).expect("read").expect("present");
        assert_eq!(owner, None);
    }

    #[test]
    fn releasing_with_the_right_nonce_removes_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("mine", 1), "mine").expect("publish");
        assert!(release(&path, "mine").expect("release"));
        assert!(!path.exists());
    }

    #[test]
    fn releasing_with_someone_elses_nonce_leaves_the_lock_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("successor", 2), "successor").expect("publish");
        assert!(!release(&path, "mine").expect("release"));
        assert!(path.exists());
        let owner = read_owner(&path)
            .expect("read")
            .expect("present")
            .expect("record");
        assert_eq!(owner.nonce, "successor");
    }

    #[test]
    fn releasing_an_absent_lock_reports_nothing_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!release(&dir.path().join("nothing.lock"), "mine").expect("release"));
    }

    #[test]
    fn acquiring_an_unheld_lock_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        try_acquire(
            &path,
            &record("n1", 1),
            &DeadPids(HashSet::new()),
            &local(),
            now(),
        )
        .expect("acquire");
        assert!(path.exists());
    }

    #[test]
    fn acquiring_a_lock_held_by_a_live_process_reports_contention_with_the_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("theirs", 42), "theirs").expect("publish");
        let error = try_acquire(
            &path,
            &record("mine", 7),
            &DeadPids(HashSet::new()),
            &local(),
            now(),
        )
        .unwrap_err();
        match error {
            AcquireError::Contention(contention) => {
                assert_eq!(contention.owner.expect("owner").pid, 42);
                assert_eq!(contention.lock_path, path);
            }
            AcquireError::Io(error) => panic!("unexpected io error: {error}"),
        }
    }

    #[test]
    fn a_lock_whose_owner_is_proven_dead_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("theirs", 42), "theirs").expect("published");
        let dead = DeadPids(HashSet::from([42]));
        try_acquire(&path, &record("mine", 7), &dead, &local(), now()).expect("acquire");
        let owner = read_owner(&path)
            .expect("read")
            .expect("present")
            .expect("record");
        assert_eq!(owner.nonce, "mine");
    }

    #[test]
    fn a_lock_held_on_another_host_is_never_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        let mut remote = record("theirs", 42);
        remote.hostname = "host-b".to_owned();
        publish(&path, &remote, "theirs").expect("publish");
        // The liveness probe would say dead, but it must not even be consulted across hosts.
        let error = try_acquire(&path, &record("mine", 7), &Panics, &local(), now()).unwrap_err();
        assert!(matches!(error, AcquireError::Contention(_)));
        let owner = read_owner(&path)
            .expect("read")
            .expect("present")
            .expect("record");
        assert_eq!(owner.nonce, "theirs");
    }

    #[test]
    fn a_partial_lock_inside_the_grace_window_is_not_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        std::fs::write(&path, "{\"pid\": 12").expect("write");
        let fresh = SystemTime::now();
        assert_eq!(
            freshness(None, Some(&Panics), &local(), lock_age(&path, fresh)),
            Freshness::Unknown
        );
        let error = try_acquire(&path, &record("mine", 7), &Panics, &local(), fresh).unwrap_err();
        assert!(matches!(error, AcquireError::Contention(_)));
    }

    #[test]
    fn a_partial_lock_past_the_grace_window_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        std::fs::write(&path, "{\"pid\": 12").expect("write");
        // One hour in the future, so the file looks far older than the grace window.
        let later = SystemTime::now() + Duration::from_secs(3_600);
        try_acquire(&path, &record("mine", 7), &Panics, &local(), later).expect("acquire");
        let owner = read_owner(&path)
            .expect("read")
            .expect("present")
            .expect("record");
        assert_eq!(owner.nonce, "mine");
    }

    #[test]
    fn freshness_of_a_parsed_owner_on_this_host_follows_the_probe() {
        let holder = record("n", 5);
        let alive = DeadPids(HashSet::new());
        let dead = DeadPids(HashSet::from([5]));
        assert_eq!(
            freshness(Some(&holder), Some(&alive), &local(), None),
            Freshness::Alive
        );
        assert_eq!(
            freshness(Some(&holder), Some(&dead), &local(), None),
            Freshness::ProvenDead
        );
    }

    #[test]
    fn freshness_without_a_probe_is_unknown_rather_than_dead() {
        let holder = record("n", 5);
        assert_eq!(
            freshness(Some(&holder), None, &local(), None),
            Freshness::Unknown
        );
    }

    #[test]
    fn freshness_of_a_remote_owner_is_unknown() {
        let mut remote = record("n", 5);
        remote.hostname = "host-b".to_owned();
        assert_eq!(
            freshness(Some(&remote), Some(&Panics), &local(), None),
            Freshness::Unknown
        );
    }

    #[test]
    fn a_lock_file_with_no_mtime_is_unknown() {
        assert_eq!(
            freshness(None, Some(&Panics), &local(), None),
            Freshness::Unknown
        );
    }

    #[test]
    fn with_lock_runs_the_body_and_releases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        let result: Result<u8, String> = with_lock(
            &path,
            &record("n1", 1),
            &DeadPids(HashSet::new()),
            &local(),
            now(),
            || Ok::<u8, String>(7),
        )
        .map_err(|error| error.to_string());
        assert_eq!(result, Ok(7));
        assert!(!path.exists());
    }

    #[test]
    fn with_lock_releases_when_the_body_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        let result = with_lock(
            &path,
            &record("n1", 1),
            &DeadPids(HashSet::new()),
            &local(),
            now(),
            || -> Result<(), &'static str> { Err("body failed") },
        );
        assert!(matches!(result, Err(WithLockError::Body("body failed"))));
        assert!(!path.exists());
    }

    #[test]
    fn with_lock_does_not_run_the_body_when_the_lock_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        publish(&path, &record("theirs", 42), "theirs").expect("publish");
        let ran = Mutex::new(false);
        let result = with_lock(
            &path,
            &record("mine", 7),
            &DeadPids(HashSet::new()),
            &local(),
            now(),
            || {
                *ran.lock().expect("lock") = true;
                Ok::<(), String>(())
            },
        );
        assert!(matches!(result, Err(WithLockError::Acquire(_))));
        assert!(!*ran.lock().expect("lock"));
        assert!(path.exists());
    }

    #[test]
    fn is_held_reflects_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        assert!(!is_held(&path));
        publish(&path, &record("n1", 1), "n1").expect("publish");
        assert!(is_held(&path));
    }

    #[test]
    fn opening_a_handle_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/locks/memory-write.lock");
        let file = open_lock_handle(&path).expect("handle");
        drop(file);
        assert!(path.exists());
    }

    #[test]
    fn contention_names_the_lock_path_in_its_message() {
        let contention = LockContention {
            lock_path: PathBuf::from("/locks/memory-write.lock"),
            owner: None,
        };
        assert!(contention.to_string().contains("memory-write.lock"));
    }

    #[test]
    fn a_korean_purpose_round_trips_through_a_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory-write.lock");
        let mut korean = record("n1", 1);
        korean.purpose = "메모리 쓰기".to_owned();
        publish(&path, &korean, "n1").expect("publish");
        let owner = read_owner(&path)
            .expect("read")
            .expect("present")
            .expect("record");
        assert_eq!(owner.purpose, "메모리 쓰기");
    }
}
