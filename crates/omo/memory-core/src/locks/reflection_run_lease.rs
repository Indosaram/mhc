//! Machine-wide reflection-run admission (`locks/counting-lease.ts` + `locks/reflection-run-domain.ts`).
//!
//! A counting lease admits at most N reflection runs at a time, in ticket order:
//!
//! ```text
//! <locks>/reflection-run.slot-1.lock ... <locks>/reflection-run.slot-N.lock
//! <locks>/reflection-run.tickets/<issue>-<seq>-<pid>-<uuid>.ticket
//! ```
//!
//! A slot is an ordinary lock file, so a slot whose owner died is reclaimed by the same proof-based
//! policy as every other lock (pid liveness, then start identity; never age). A ticket is a lock
//! record too: only the ticket at the head of the queue may take a slot, and a head ticket left by a
//! dead process is reaped on the same proof, so a crash between enqueue and acquire can never freeze
//! the queue.
//!
//! Reflection runs queue without a timeout: only execution consumes the run budget, so the caller
//! waits until a slot frees or its own cancellation reports an abort.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::fs;
use crate::locks::acquire::{
    AcquireLockError, AcquireLockOptions, delay, is_lock_owner_proven_dead, release_lock,
};
use crate::locks::lock_record::{
    CreateLockRecordOptions, LockRecord, create_lock_record, parse_lock_record,
};
use crate::support::random::random_uuid;
use crate::support::time::now_millis;

/// Reflection runs one machine admits when the caller names no count.
pub const REFLECTION_RUN_DEFAULT_SLOTS: usize = 1;

const SLOT_PURPOSE: &str = "reflection-run";
const TICKET_PURPOSE: &str = "reflection-run:ticket";
const TICKET_SUFFIX: &str = ".ticket";
const DEFAULT_RETRY_DELAY_MS: u64 = 25;

/// Lease options; `cancellation` is the pinned `AbortSignal` seam.
#[derive(Default)]
pub struct ReflectionRunLeaseOptions<'a> {
    pub max_concurrent: Option<usize>,
    pub retry_delay_ms: Option<u64>,
    pub cancellation: Option<&'a dyn Fn() -> bool>,
}

/// A held reflection-run slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionRunLease {
    /// 1-based slot this lease holds.
    pub slot: usize,
    lock_path: PathBuf,
    record: LockRecord,
}

impl ReflectionRunLease {
    /// Hands the slot back, preserving the real failure: `Ok(true)` released, `Ok(false)` already
    /// gone, `Err` a filesystem failure.
    pub fn try_release(&self) -> Result<bool, std::io::Error> {
        release_lock(&self.lock_path, &self.record)
    }

    /// Hands the slot back; `false` when the lease was already released or the release failed.
    pub fn release(&self) -> bool {
        self.try_release().unwrap_or(false)
    }

    /// Keeps the slot occupied if the supervisor dies while its gated child survives: the slot's
    /// record is rewritten to name the child, so liveness proves the child, not the supervisor.
    pub fn retain_for(
        &self,
        pid: u32,
        process_start: Option<&str>,
    ) -> Result<(), ReflectionRunLeaseError> {
        let mut record = self.record.clone();
        record.pid = pid;
        record.process_start = process_start.unwrap_or("unavailable").to_string();
        let payload = serde_json::to_string(&record)
            .map_err(|error| ReflectionRunLeaseError::Lock(error.to_string()))?;
        let staging = PathBuf::from(format!(
            "{}.handoff-{}",
            self.lock_path.display(),
            self.record.nonce
        ));
        let written = fs::write(&staging, format!("{payload}\n").as_bytes())
            .and_then(|()| fs::rename(&staging, &self.lock_path));
        let _ = fs::remove_file(&staging);
        written.map_err(ReflectionRunLeaseError::Io)
    }
}

#[derive(Debug)]
pub enum ReflectionRunLeaseError {
    Aborted,
    InvalidOptions(String),
    Lock(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ReflectionRunLeaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aborted => write!(formatter, "The operation was aborted"),
            Self::InvalidOptions(message) => write!(formatter, "{message}"),
            Self::Lock(message) => write!(formatter, "reflection-run lock error: {message}"),
            Self::Io(error) => write!(formatter, "reflection-run io error: {error}"),
        }
    }
}

impl std::error::Error for ReflectionRunLeaseError {}

/// `<locks>/reflection-run.slot-<n>.lock`
pub fn reflection_run_lock_path(
    locks_directory: &Path,
    slot: usize,
) -> Result<PathBuf, ReflectionRunLeaseError> {
    if slot < 1 {
        return Err(ReflectionRunLeaseError::InvalidOptions(format!(
            "reflection-run slot must be a positive integer, got {slot}"
        )));
    }
    Ok(locks_directory.join(format!("{SLOT_PURPOSE}.slot-{slot}.lock")))
}

/// `<locks>/reflection-run.tickets`
pub fn reflection_run_ticket_directory(locks_directory: &Path) -> PathBuf {
    locks_directory.join(format!("{SLOT_PURPOSE}.tickets"))
}

static TICKET_SEQUENCE: Mutex<u64> = Mutex::new(0);
static TICKET_PUBLICATION: Mutex<()> = Mutex::new(());

fn ticket_name() -> String {
    let sequence = {
        let mut guard = TICKET_SEQUENCE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = (*guard + 1) % 1_000_000;
        *guard
    };
    format!(
        "{:0>16}-{:0>6}-{:0>10}-{}{}",
        now_millis().max(0),
        sequence,
        std::process::id(),
        random_uuid(),
        TICKET_SUFFIX
    )
}

/// Writes the ticket whole under a temporary name, then renames: a `.ticket` file is complete or absent.
fn publish_ticket(
    ticket_directory: &Path,
    name: &str,
    record: &LockRecord,
) -> Result<(), ReflectionRunLeaseError> {
    fs::create_dir_all(ticket_directory).map_err(ReflectionRunLeaseError::Io)?;
    let staging = ticket_directory.join(format!("{name}.staging"));
    let payload = serde_json::to_string(record)
        .map_err(|error| ReflectionRunLeaseError::Lock(error.to_string()))?;
    fs::write(&staging, format!("{payload}\n").as_bytes()).map_err(ReflectionRunLeaseError::Io)?;
    fs::rename(&staging, &ticket_directory.join(name)).map_err(ReflectionRunLeaseError::Io)
}

fn list_tickets(ticket_directory: &Path) -> Result<Vec<String>, ReflectionRunLeaseError> {
    match fs::read_dir_names(ticket_directory) {
        Ok(mut names) => {
            names.retain(|name| name.ends_with(TICKET_SUFFIX));
            names.sort();
            Ok(names)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(ReflectionRunLeaseError::Io(error)),
    }
}

fn unlink_if_present(path: &Path) -> Result<(), ReflectionRunLeaseError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ReflectionRunLeaseError::Io(error)),
    }
}

/// True when the error is a Windows sharing error; always false off Windows.
#[cfg(windows)]
fn is_ticket_read_sharing_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
        || error.kind() == std::io::ErrorKind::PermissionDenied
}

/// True when the error is a Windows sharing error; always false off Windows.
#[cfg(not(windows))]
fn is_ticket_read_sharing_error(_error: &std::io::Error) -> bool {
    false
}

/// Reaps the head ticket when its owner is proven dead; an unreadable ticket keeps its place.
fn reap_dead_head(ticket_directory: &Path, head: &str) -> Result<bool, ReflectionRunLeaseError> {
    let ticket_path = ticket_directory.join(head);
    let raw = match fs::read_to_string(&ticket_path) {
        Ok(raw) => raw,
        // Gone already: its owner acquired or withdrew between our readdir and this read.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        // A denied open means the owner is publishing or withdrawing it; keep the head and poll again.
        Err(error) if is_ticket_read_sharing_error(&error) => return Ok(false),
        Err(error) => return Err(ReflectionRunLeaseError::Io(error)),
    };
    let Some(owner) = parse_lock_record(&raw) else {
        return Ok(false);
    };
    if !is_lock_owner_proven_dead(&owner) {
        return Ok(false);
    }
    unlink_if_present(&ticket_path)?;
    Ok(true)
}

fn take_any_slot(
    locks_directory: &Path,
    record: &LockRecord,
    max_concurrent: usize,
    cancellation: Option<&dyn Fn() -> bool>,
) -> Result<Option<ReflectionRunLease>, ReflectionRunLeaseError> {
    for slot in 1..=max_concurrent {
        let lock_path = reflection_run_lock_path(locks_directory, slot)?;
        let options = AcquireLockOptions {
            wait_timeout_ms: Some(0),
            cancellation,
            ..Default::default()
        };
        match crate::locks::acquire::acquire_lock(&lock_path, record, &options) {
            Ok(()) => {
                return Ok(Some(ReflectionRunLease {
                    slot,
                    lock_path,
                    record: record.clone(),
                }));
            }
            Err(AcquireLockError::Contention(_)) => continue,
            Err(AcquireLockError::Aborted) => return Err(ReflectionRunLeaseError::Aborted),
            Err(error) => return Err(ReflectionRunLeaseError::Lock(error.to_string())),
        }
    }
    Ok(None)
}

/// Takes a FIFO ticket, waits for a slot without a timeout, and withdraws the ticket in every case.
pub fn acquire_reflection_run_lease(
    locks_directory: &Path,
    options: &ReflectionRunLeaseOptions<'_>,
) -> Result<ReflectionRunLease, ReflectionRunLeaseError> {
    let max_concurrent = options
        .max_concurrent
        .unwrap_or(REFLECTION_RUN_DEFAULT_SLOTS);
    let retry_delay_ms = options.retry_delay_ms.unwrap_or(DEFAULT_RETRY_DELAY_MS);
    if max_concurrent < 1 {
        return Err(ReflectionRunLeaseError::InvalidOptions(format!(
            "reflection-run maxConcurrent must be a positive integer, got {max_concurrent}"
        )));
    }
    if retry_delay_ms == 0 {
        return Err(ReflectionRunLeaseError::InvalidOptions(
            "lock wait options must be positive".to_string(),
        ));
    }
    let cancellation = options.cancellation;
    let ticket_directory = reflection_run_ticket_directory(locks_directory);
    let name = ticket_name();

    let ticket_record = create_lock_record(TICKET_PURPOSE, CreateLockRecordOptions::default())
        .map_err(|error| ReflectionRunLeaseError::Lock(error.to_string()))?;
    // Publication order within a process is the call order: the queue directory only ever sees a
    // completed ticket, and this guard serializes the staging write plus the rename.
    {
        let _guard = TICKET_PUBLICATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        throw_if_aborted(cancellation)?;
        publish_ticket(&ticket_directory, &name, &ticket_record)?;
    }

    let outcome = (|| -> Result<ReflectionRunLease, ReflectionRunLeaseError> {
        let record = create_lock_record(SLOT_PURPOSE, CreateLockRecordOptions::default())
            .map_err(|error| ReflectionRunLeaseError::Lock(error.to_string()))?;
        let mut attempt: u64 = 0;
        loop {
            throw_if_aborted(cancellation)?;
            let queue = list_tickets(&ticket_directory)?;
            let head = queue.first().cloned();
            match head {
                None => {
                    publish_ticket(&ticket_directory, &name, &ticket_record)?;
                }
                Some(head) if head == name => {
                    if let Some(lease) =
                        take_any_slot(locks_directory, &record, max_concurrent, cancellation)?
                    {
                        if let Err(error) = unlink_if_present(&ticket_directory.join(&name)) {
                            // A ticket we cannot withdraw would block the queue for as long as we
                            // live: give the slot back rather than leak the lease.
                            let _ = lease.release();
                            return Err(error);
                        }
                        return Ok(lease);
                    }
                }
                Some(head) => {
                    if !queue.iter().any(|ticket| ticket == &name) {
                        publish_ticket(&ticket_directory, &name, &ticket_record)?;
                    } else if reap_dead_head(&ticket_directory, &head)? {
                        // The queue moved without anyone acquiring: look again at once.
                        continue;
                    }
                }
            }
            // The queue has no timeout: back off, and keep waiting until the caller aborts.
            let pause = backoff_delay(attempt, retry_delay_ms);
            attempt = attempt.saturating_add(1);
            if !delay(pause, cancellation) {
                return Err(ReflectionRunLeaseError::Aborted);
            }
        }
    })();

    if let Err(error) = unlink_if_present(&ticket_directory.join(&name))
        && outcome.is_ok()
    {
        return Err(error);
    }
    outcome
}

/// `lockRetryDelayMs`: exponential from `base`, capped at `LOCK_RETRY_MAX_DELAY_MS`. The pinned jitter
/// only spreads contenders that started together; a single-process queue needs none, so the ceiling
/// itself is the pause.
fn backoff_delay(attempt: u64, base: u64) -> u64 {
    const LOCK_RETRY_MAX_DELAY_MS: u64 = 250;
    let factor = 2u64.saturating_pow(u32::try_from(attempt.min(30)).unwrap_or(30));
    LOCK_RETRY_MAX_DELAY_MS
        .min(base.saturating_mul(factor))
        .max(1)
}

fn throw_if_aborted(
    cancellation: Option<&dyn Fn() -> bool>,
) -> Result<(), ReflectionRunLeaseError> {
    if cancellation.map(|probe| probe()).unwrap_or(false) {
        return Err(ReflectionRunLeaseError::Aborted);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_machine_slot_is_the_default_and_the_paths_are_derived_from_the_purpose() {
        let locks = Path::new("/locks");
        assert_eq!(REFLECTION_RUN_DEFAULT_SLOTS, 1);
        assert_eq!(
            reflection_run_lock_path(locks, 2).unwrap(),
            PathBuf::from("/locks/reflection-run.slot-2.lock")
        );
        assert!(reflection_run_lock_path(locks, 0).is_err());
        assert_eq!(
            reflection_run_ticket_directory(locks),
            PathBuf::from("/locks/reflection-run.tickets")
        );
    }

    #[test]
    fn an_aborted_caller_takes_no_ticket_and_no_slot() {
        let root = tempfile::tempdir().unwrap();
        let aborted = || true;
        let error = acquire_reflection_run_lease(
            root.path(),
            &ReflectionRunLeaseOptions {
                cancellation: Some(&aborted),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, ReflectionRunLeaseError::Aborted));
        assert!(
            list_tickets(&reflection_run_ticket_directory(root.path()))
                .unwrap()
                .is_empty()
        );
        assert!(!reflection_run_lock_path(root.path(), 1).unwrap().exists());
    }

    #[test]
    fn a_slot_is_released_and_a_second_contender_can_take_it() {
        let root = tempfile::tempdir().unwrap();
        let first =
            acquire_reflection_run_lease(root.path(), &ReflectionRunLeaseOptions::default())
                .unwrap();
        assert_eq!(first.slot, 1);
        assert!(first.try_release().unwrap());
        assert!(!first.release());
        let second =
            acquire_reflection_run_lease(root.path(), &ReflectionRunLeaseOptions::default())
                .unwrap();
        assert_eq!(second.slot, 1);
        second.release();
    }

    #[test]
    fn a_retained_slot_names_the_child_so_the_supervisor_can_die() {
        let root = tempfile::tempdir().unwrap();
        let lease =
            acquire_reflection_run_lease(root.path(), &ReflectionRunLeaseOptions::default())
                .unwrap();
        let child_pid = std::process::id();
        let child_start = crate::locks::process_identity::get_process_start_identity(child_pid);
        lease.retain_for(child_pid, child_start.as_deref()).unwrap();
        let record = parse_lock_record(
            &fs::read_to_string(&reflection_run_lock_path(root.path(), 1).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(record.pid, child_pid);
        assert_eq!(record.purpose, SLOT_PURPOSE);
        assert!(lease.release());
    }

    #[test]
    fn two_slots_admit_two_holders_and_a_third_queues() {
        let root = tempfile::tempdir().unwrap();
        let two = ReflectionRunLeaseOptions {
            max_concurrent: Some(2),
            ..Default::default()
        };
        let first = acquire_reflection_run_lease(root.path(), &two).unwrap();
        let second = acquire_reflection_run_lease(root.path(), &two).unwrap();
        assert_eq!((first.slot, second.slot), (1, 2));
        let aborted = || true;
        let queued = acquire_reflection_run_lease(
            root.path(),
            &ReflectionRunLeaseOptions {
                max_concurrent: Some(2),
                cancellation: Some(&aborted),
                ..Default::default()
            },
        );
        assert!(matches!(queued, Err(ReflectionRunLeaseError::Aborted)));
        first.release();
        second.release();
        assert!(
            list_tickets(&reflection_run_ticket_directory(root.path()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_ticket_left_by_a_dead_owner_does_not_freeze_the_queue() {
        let root = tempfile::tempdir().unwrap();
        let tickets = reflection_run_ticket_directory(root.path());
        fs::create_dir_all(&tickets).unwrap();
        // A head ticket whose owner pid cannot be alive: the queue reaps it on the next pass.
        let dead = serde_json::json!({
            "pid": 2_147_483_647u32,
            "process_start": "gone",
            "hostname": crate::support::host::hostname(),
            "nonce": "dead-nonce",
            "created_at": "2026-01-01T00:00:00.000Z",
            "purpose": TICKET_PURPOSE,
        });
        fs::write(
            &tickets.join("0000000000000001-000000-0000000001-dead.ticket"),
            format!("{dead}\n").as_bytes(),
        )
        .unwrap();
        assert!(
            reap_dead_head(&tickets, "0000000000000001-000000-0000000001-dead.ticket").unwrap()
        );
        let lease =
            acquire_reflection_run_lease(root.path(), &ReflectionRunLeaseOptions::default())
                .unwrap();
        assert_eq!(lease.slot, 1);
        lease.release();
    }
}
