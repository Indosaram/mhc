//! Machine-wide recall-wake domain: a counting lease admitting at most N concurrent wakes, in
//! ticket order (pin `locks/recall-wake-domain.ts`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::fs;
use crate::locks::acquire::{
    AcquireLockError, AcquireLockOptions, delay, is_lock_owner_proven_dead, release_lock,
};
use crate::locks::lock_record::{CreateLockRecordOptions, LockRecord, create_lock_record, parse_lock_record};
use crate::support::random::random_uuid;
use crate::support::time::now_millis;

/// Concurrent wakes one machine admits when the caller names no count.
pub const RECALL_WAKE_DEFAULT_SLOTS: usize = 2;

const SLOT_PURPOSE: &str = "recall-wake";
const TICKET_PURPOSE: &str = "recall-wake:ticket";
const TICKET_SUFFIX: &str = ".ticket";

/// Lease options; `cancellation` is the pinned `AbortSignal` seam.
#[derive(Default)]
pub struct RecallWakeLeaseOptions<'a> {
    pub max_concurrent: Option<usize>,
    pub wait_timeout_ms: Option<u64>,
    pub retry_delay_ms: Option<u64>,
    pub cancellation: Option<&'a dyn Fn() -> bool>,
}

/// A held wake slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallWakeLease {
    pub slot: usize,
    lock_path: PathBuf,
    nonce: String,
}

impl RecallWakeLease {
    /// Hands the slot back, preserving the real failure: `Ok(true)` released, `Ok(false)` already
    /// gone (no owner, a different nonce, or the file already removed), `Err` a filesystem failure
    /// the bool API below would have swallowed. Upstream `releaseLease` distinguishes exactly these
    /// three so the caller can warn "already gone" on `false` and "release failed" on `Err`.
    pub fn try_release(&self) -> Result<bool, std::io::Error> {
        release_lock(&self.lock_path, &self.nonce)
    }

    /// Hands the slot back; `false` when the lease was already released OR the release failed.
    /// Retained for existing callers; the typed [`Self::try_release`] distinguishes the two.
    pub fn release(&self) -> bool {
        self.try_release().unwrap_or(false)
    }
}

/// Every slot stayed with a live owner for the whole wait budget: retry later, nothing is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallWakeBusyError {
    pub waited_ms: u64,
    pub max_concurrent: usize,
}

impl RecallWakeBusyError {
    pub fn retriable(&self) -> bool {
        true
    }
}

impl std::fmt::Display for RecallWakeBusyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "recall-wake: all {} slot(s) busy after {}ms",
            self.max_concurrent, self.waited_ms
        )
    }
}

impl std::error::Error for RecallWakeBusyError {}

#[derive(Debug)]
pub enum RecallWakeError {
    Busy(RecallWakeBusyError),
    Aborted,
    InvalidOptions(String),
    Lock(String),
    Io(std::io::Error),
}

impl std::fmt::Display for RecallWakeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(err) => write!(formatter, "{err}"),
            Self::Aborted => write!(formatter, "The operation was aborted"),
            Self::InvalidOptions(msg) => write!(formatter, "{msg}"),
            Self::Lock(msg) => write!(formatter, "recall-wake lock error: {msg}"),
            Self::Io(err) => write!(formatter, "recall-wake io error: {err}"),
        }
    }
}

impl std::error::Error for RecallWakeError {}

pub fn recall_wake_lock_path(locks_directory: &Path, slot: usize) -> Result<PathBuf, RecallWakeError> {
    if slot < 1 {
        return Err(RecallWakeError::InvalidOptions(format!(
            "recall-wake slot must be a positive integer, got {slot}"
        )));
    }
    Ok(locks_directory.join(format!("recall-wake.slot-{slot}.lock")))
}

pub fn recall_wake_ticket_directory(locks_directory: &Path) -> PathBuf {
    locks_directory.join("recall-wake.tickets")
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
) -> Result<(), RecallWakeError> {
    fs::create_dir_all(ticket_directory).map_err(RecallWakeError::Io)?;
    let staging = ticket_directory.join(format!("{name}.staging"));
    let payload = serde_json::to_string(record)
        .map_err(|error| RecallWakeError::Lock(error.to_string()))?;
    fs::write(&staging, format!("{payload}\n").as_bytes()).map_err(RecallWakeError::Io)?;
    fs::rename(&staging, &ticket_directory.join(name)).map_err(RecallWakeError::Io)
}

fn list_tickets(ticket_directory: &Path) -> Result<Vec<String>, RecallWakeError> {
    match fs::read_dir_names(ticket_directory) {
        Ok(mut names) => {
            names.retain(|name| name.ends_with(TICKET_SUFFIX));
            names.sort();
            Ok(names)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(RecallWakeError::Io(error)),
    }
}

fn unlink_if_present(path: &Path) -> Result<(), RecallWakeError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RecallWakeError::Io(error)),
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
fn reap_dead_head(ticket_directory: &Path, head: &str) -> Result<bool, RecallWakeError> {
    let ticket_path = ticket_directory.join(head);
    let raw = match fs::read_to_string(&ticket_path) {
        Ok(raw) => raw,
        // Gone already: its owner acquired or withdrew between our readdir and this read.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        // A denied open means the owner is publishing or withdrawing it; keep the head and poll again.
        Err(error) if is_ticket_read_sharing_error(&error) => return Ok(false),
        Err(error) => return Err(RecallWakeError::Io(error)),
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
) -> Result<Option<RecallWakeLease>, RecallWakeError> {
    for slot in 1..=max_concurrent {
        let lock_path = recall_wake_lock_path(locks_directory, slot)?;
        let options = AcquireLockOptions {
            wait_timeout_ms: Some(0),
            cancellation,
            ..Default::default()
        };
        match crate::locks::acquire::acquire_lock(&lock_path, record, &options) {
            Ok(()) => {
                return Ok(Some(RecallWakeLease {
                    slot,
                    lock_path,
                    nonce: record.nonce.clone(),
                }));
            }
            Err(AcquireLockError::Contention(_)) => continue,
            Err(AcquireLockError::Aborted) => return Err(RecallWakeError::Aborted),
            Err(error) => return Err(RecallWakeError::Lock(error.to_string())),
        }
    }
    Ok(None)
}

/// Takes a ticket, waits (bounded) for the head of the queue and a free slot, and withdraws the
/// ticket in every case.
pub fn acquire_recall_wake_lease(
    locks_directory: &Path,
    options: &RecallWakeLeaseOptions<'_>,
) -> Result<RecallWakeLease, RecallWakeError> {
    let max_concurrent = options.max_concurrent.unwrap_or(RECALL_WAKE_DEFAULT_SLOTS);
    let wait_timeout_ms = options.wait_timeout_ms.unwrap_or(0);
    let retry_delay_ms = options.retry_delay_ms.unwrap_or(25);
    if max_concurrent < 1 {
        return Err(RecallWakeError::InvalidOptions(format!(
            "recall-wake maxConcurrent must be a positive integer, got {max_concurrent}"
        )));
    }
    if retry_delay_ms == 0 {
        return Err(RecallWakeError::InvalidOptions(
            "lock wait options must be positive".to_string(),
        ));
    }
    let cancellation = options.cancellation;
    let ticket_directory = recall_wake_ticket_directory(locks_directory);
    let name = ticket_name();
    let started = now_millis().max(0) as u64;
    let deadline = started + wait_timeout_ms;

    let ticket_record = create_lock_record(TICKET_PURPOSE, CreateLockRecordOptions::default())
        .map_err(|error| RecallWakeError::Lock(error.to_string()))?;
    // Publication order within a process is the call order: the queue directory only ever sees a
    // completed ticket, and this guard serializes the staging write plus the rename.
    {
        let _guard = TICKET_PUBLICATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        throw_if_aborted(cancellation)?;
        publish_ticket(&ticket_directory, &name, &ticket_record)?;
    }

    let outcome = (|| -> Result<RecallWakeLease, RecallWakeError> {
        let record = create_lock_record(SLOT_PURPOSE, CreateLockRecordOptions::default())
            .map_err(|error| RecallWakeError::Lock(error.to_string()))?;
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
            let now = now_millis().max(0) as u64;
            if now >= deadline {
                return Err(RecallWakeError::Busy(RecallWakeBusyError {
                    waited_ms: now - started,
                    max_concurrent,
                }));
            }
            let pause = retry_delay_ms.min((deadline - now).max(1));
            if !delay(pause, cancellation) {
                return Err(RecallWakeError::Aborted);
            }
        }
    })();

    if let Err(error) = unlink_if_present(&ticket_directory.join(&name)) {
        if outcome.is_ok() {
            return Err(error);
        }
    }
    outcome
}

pub fn with_recall_wake_lease<T>(
    locks_directory: &Path,
    options: &RecallWakeLeaseOptions<'_>,
    operation: impl FnOnce(&RecallWakeLease) -> T,
) -> Result<T, RecallWakeError> {
    let lease = acquire_recall_wake_lease(locks_directory, options)?;
    let result = operation(&lease);
    lease.release();
    Ok(result)
}

fn throw_if_aborted(cancellation: Option<&dyn Fn() -> bool>) -> Result<(), RecallWakeError> {
    if cancellation.map(|probe| probe()).unwrap_or(false) {
        return Err(RecallWakeError::Aborted);
    }
    Ok(())
}

#[cfg(test)]
#[path = "recall_wake_domain_tests.rs"]
mod tests;
