//! Reusable FIFO, crash-recoverable counting leases (pin `locks/counting-lease.ts`).

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

/// Configuration shared by every contender in a lease domain.
pub struct CountingLeaseDomain<'a> {
    pub purpose: &'a str,
    pub default_slots: usize,
    /// None represents an unbounded queue wait.
    pub wait_timeout_ms: Option<u64>,
}

const TICKET_SUFFIX: &str = ".ticket";

type TicketReader = std::sync::Arc<dyn Fn(&Path) -> std::io::Result<String> + Send + Sync>;
type SharingClassifier = std::sync::Arc<dyn Fn(&std::io::Error) -> bool + Send + Sync>;
#[derive(Clone, Default)]
pub struct CountingLeaseTicketFs {
    pub read_file: Option<TicketReader>,
    pub is_sharing_error: Option<SharingClassifier>,
}
thread_local! {
    static TICKET_FS: std::cell::RefCell<CountingLeaseTicketFs> = std::cell::RefCell::default();
}
/// Thread-local injection prevents concurrently running Rust tests from replacing each other's IO.
pub fn set_counting_lease_ticket_fs_for_tests(next: CountingLeaseTicketFs) -> impl FnOnce() {
    let previous = TICKET_FS.with(|fs| fs.replace(next));
    move || {
        TICKET_FS.with(|fs| fs.replace(previous));
    }
}
fn read_ticket(path: &Path) -> std::io::Result<String> {
    let reader = TICKET_FS.with(|fs| fs.borrow().read_file.clone());
    match reader {
        Some(reader) => reader(path),
        None => fs::read_to_string(path),
    }
}
fn ticket_sharing_error(error: &std::io::Error) -> bool {
    let classifier = TICKET_FS.with(|fs| fs.borrow().is_sharing_error.clone());
    classifier.map_or_else(
        || is_ticket_read_sharing_error(error),
        |classify| classify(error),
    )
}

/// Lease options; `cancellation` is the pinned `AbortSignal` seam.
#[derive(Default)]
pub struct CountingLeaseOptions<'a> {
    pub max_concurrent: Option<usize>,
    pub wait_timeout_ms: Option<u64>,
    pub retry_delay_ms: Option<u64>,
    pub cancellation: Option<&'a dyn Fn() -> bool>,
}

/// A held wake slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountingLease {
    pub slot: usize,
    pub(crate) lock_path: PathBuf,
    pub(crate) nonce: String,
}

impl CountingLease {
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

    pub fn release_sync(&self) -> Result<bool, std::io::Error> {
        self.try_release()
    }

    /// Transfer crash-recovery ownership to the gated child while keeping the release nonce.
    pub fn retain_for(&self, pid: u32, process_start: Option<&str>) -> Result<(), std::io::Error> {
        let raw = fs::read_to_string(&self.lock_path)?;
        let mut record = parse_lock_record(&raw).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid lease record")
        })?;
        if record.nonce != self.nonce {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "lease owner changed",
            ));
        }
        record.pid = pid;
        record.process_start = process_start.unwrap_or("unavailable").to_owned();
        record.git_activity_lock = None;
        let staging = PathBuf::from(format!(
            "{}.handoff-{}",
            self.lock_path.display(),
            self.nonce
        ));
        let outcome = (|| {
            let payload = serde_json::to_string(&record).map_err(std::io::Error::other)?;
            write_private_file(&staging, format!("{payload}\n").as_bytes())?;
            fs::rename(&staging, &self.lock_path)
        })();
        match fs::remove_file(&staging) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        outcome
    }
}

/// Every slot stayed with a live owner for the whole wait budget: retry later, nothing is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountingLeaseBusyError {
    pub waited_ms: u64,
    pub max_concurrent: usize,
    pub purpose: String,
}

impl CountingLeaseBusyError {
    pub fn retriable(&self) -> bool {
        true
    }
}

impl std::fmt::Display for CountingLeaseBusyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: all {} slot(s) busy after {}ms",
            self.purpose, self.max_concurrent, self.waited_ms
        )
    }
}

impl std::error::Error for CountingLeaseBusyError {}

#[derive(Debug)]
pub enum CountingLeaseError {
    Busy(CountingLeaseBusyError),
    Aborted,
    InvalidOptions(String),
    Lock(String),
    Io(std::io::Error),
}

impl std::fmt::Display for CountingLeaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(err) => write!(formatter, "{err}"),
            Self::Aborted => write!(formatter, "The operation was aborted"),
            Self::InvalidOptions(msg) => write!(formatter, "{msg}"),
            Self::Lock(msg) => write!(formatter, "counting lease lock error: {msg}"),
            Self::Io(err) => write!(formatter, "counting lease io error: {err}"),
        }
    }
}

impl std::error::Error for CountingLeaseError {}

pub fn counting_lease_lock_path(
    locks_directory: &Path,
    purpose: &str,
    slot: usize,
) -> Result<PathBuf, CountingLeaseError> {
    if slot < 1 {
        return Err(CountingLeaseError::InvalidOptions(format!(
            "{purpose} slot must be a positive integer, got {slot}"
        )));
    }
    Ok(locks_directory.join(format!("{purpose}.slot-{slot}.lock")))
}

pub fn counting_lease_ticket_directory(locks_directory: &Path, purpose: &str) -> PathBuf {
    locks_directory.join(format!("{purpose}.tickets"))
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

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = crate::fs::retry::retry_on_eintr(|| options.open(path))?;
    crate::fs::write_all::write_all_to_handle(&mut handle, bytes)?;
    Ok(())
}

/// Writes the ticket whole under a temporary name, then renames: a `.ticket` file is complete or absent.
pub(crate) fn publish_ticket(
    ticket_directory: &Path,
    name: &str,
    record: &LockRecord,
) -> Result<(), CountingLeaseError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        crate::fs::retry::retry_on_eintr(|| builder.create(ticket_directory))
            .map_err(CountingLeaseError::Io)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(ticket_directory).map_err(CountingLeaseError::Io)?;
    let staging = ticket_directory.join(format!("{name}.staging"));
    let payload = serde_json::to_string(record)
        .map_err(|error| CountingLeaseError::Lock(error.to_string()))?;
    write_private_file(&staging, format!("{payload}\n").as_bytes())
        .map_err(CountingLeaseError::Io)?;
    fs::rename(&staging, &ticket_directory.join(name)).map_err(CountingLeaseError::Io)
}

pub(crate) fn list_tickets(ticket_directory: &Path) -> Result<Vec<String>, CountingLeaseError> {
    match fs::read_dir_names(ticket_directory) {
        Ok(mut names) => {
            names.retain(|name| name.ends_with(TICKET_SUFFIX));
            names.sort();
            Ok(names)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(CountingLeaseError::Io(error)),
    }
}

fn unlink_if_present(path: &Path) -> Result<(), CountingLeaseError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CountingLeaseError::Io(error)),
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
pub(crate) fn reap_dead_head(
    ticket_directory: &Path,
    head: &str,
) -> Result<bool, CountingLeaseError> {
    let ticket_path = ticket_directory.join(head);
    let raw = match read_ticket(&ticket_path) {
        Ok(raw) => raw,
        // Gone already: its owner acquired or withdrew between our readdir and this read.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        // A denied open means the owner is publishing or withdrawing it; keep the head and poll again.
        Err(error) if ticket_sharing_error(&error) => return Ok(false),
        Err(error) => return Err(CountingLeaseError::Io(error)),
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
) -> Result<Option<CountingLease>, CountingLeaseError> {
    for slot in 1..=max_concurrent {
        let lock_path = counting_lease_lock_path(locks_directory, &record.purpose, slot)?;
        let options = AcquireLockOptions {
            wait_timeout_ms: Some(0),
            cancellation,
            ..Default::default()
        };
        match crate::locks::acquire::acquire_lock(&lock_path, record, &options) {
            Ok(()) => {
                return Ok(Some(CountingLease {
                    slot,
                    lock_path,
                    nonce: record.nonce.clone(),
                }));
            }
            Err(AcquireLockError::Contention(_)) => continue,
            Err(AcquireLockError::Aborted) => return Err(CountingLeaseError::Aborted),
            Err(error) => return Err(CountingLeaseError::Lock(error.to_string())),
        }
    }
    Ok(None)
}

/// Takes a ticket, waits (bounded) for the head of the queue and a free slot, and withdraws the
/// ticket in every case.
pub fn acquire_counting_lease(
    locks_directory: &Path,
    domain: &CountingLeaseDomain<'_>,
    options: &CountingLeaseOptions<'_>,
) -> Result<CountingLease, CountingLeaseError> {
    let max_concurrent = options.max_concurrent.unwrap_or(domain.default_slots);
    let wait_timeout_ms = options.wait_timeout_ms.or(domain.wait_timeout_ms);
    let retry_delay_ms = options.retry_delay_ms.unwrap_or(25);
    if max_concurrent < 1 {
        return Err(CountingLeaseError::InvalidOptions(format!(
            "{} maxConcurrent must be a positive integer, got {max_concurrent}",
            domain.purpose
        )));
    }
    if retry_delay_ms == 0 {
        return Err(CountingLeaseError::InvalidOptions(
            "lock wait options must be positive".to_string(),
        ));
    }
    let cancellation = options.cancellation;
    let ticket_directory = counting_lease_ticket_directory(locks_directory, domain.purpose);
    let started = now_millis().max(0) as u64;
    let deadline = wait_timeout_ms.map(|wait| started.saturating_add(wait));

    let ticket_record = create_lock_record(
        &format!("{}:ticket", domain.purpose),
        CreateLockRecordOptions::default(),
    )
    .map_err(|error| CountingLeaseError::Lock(error.to_string()))?;
    // Publication order within a process is the call order: the queue directory only ever sees a
    // completed ticket, and this guard serializes the staging write plus the rename.
    let name = {
        let _guard = TICKET_PUBLICATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        throw_if_aborted(cancellation)?;
        let name = ticket_name();
        publish_ticket(&ticket_directory, &name, &ticket_record)?;
        name
    };

    let outcome = (|| -> Result<CountingLease, CountingLeaseError> {
        let record = create_lock_record(domain.purpose, CreateLockRecordOptions::default())
            .map_err(|error| CountingLeaseError::Lock(error.to_string()))?;
        let mut attempt = 0u32;
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
            if deadline.is_some_and(|deadline| now >= deadline) {
                return Err(CountingLeaseError::Busy(CountingLeaseBusyError {
                    waited_ms: now - started,
                    max_concurrent,
                    purpose: domain.purpose.to_owned(),
                }));
            }
            let ceiling = retry_delay_ms
                .saturating_mul(1u64 << attempt.min(30))
                .min(250);
            let random =
                u64::from_str_radix(&random_uuid()[..8], 16).unwrap_or(0) as f64 / u32::MAX as f64;
            let pause = ((ceiling as f64 / 2.0 * (1.0 + random)).round() as u64).max(1);
            let pause = deadline.map_or(pause, |deadline| pause.min((deadline - now).max(1)));
            attempt = attempt.saturating_add(1);
            if !delay(pause, cancellation) {
                return Err(CountingLeaseError::Aborted);
            }
        }
    })();

    unlink_if_present(&ticket_directory.join(&name))?;
    outcome
}

fn throw_if_aborted(cancellation: Option<&dyn Fn() -> bool>) -> Result<(), CountingLeaseError> {
    if cancellation.map(|probe| probe()).unwrap_or(false) {
        return Err(CountingLeaseError::Aborted);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_child_record_preserves_nonce_without_activity_proof() {
        let directory = tempfile::tempdir().expect("directory");
        let domain = CountingLeaseDomain {
            purpose: "child",
            default_slots: 1,
            wait_timeout_ms: Some(0),
        };
        let lease =
            acquire_counting_lease(directory.path(), &domain, &Default::default()).expect("lease");
        lease.retain_for(123, None).expect("retain");
        let record = parse_lock_record(&fs::read_to_string(&lease.lock_path).expect("read"))
            .expect("record");
        assert_eq!(record.pid, 123);
        assert_eq!(record.process_start, "unavailable");
        assert_eq!(record.nonce, lease.nonce);
        assert_eq!(record.git_activity_lock, None);
        assert!(lease.release_sync().expect("release"));
        assert!(!lease.release_sync().expect("already gone"));
    }
    #[test]
    fn transient_ticket_sharing_keeps_the_live_head() {
        let directory = tempfile::tempdir().expect("directory");
        let record = create_lock_record("test:ticket", Default::default()).expect("record");
        publish_ticket(directory.path(), "head.ticket", &record).expect("ticket");
        let restore = set_counting_lease_ticket_fs_for_tests(CountingLeaseTicketFs {
            read_file: Some(std::sync::Arc::new(|_| {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            })),
            is_sharing_error: Some(std::sync::Arc::new(|error| {
                error.kind() == std::io::ErrorKind::PermissionDenied
            })),
        });
        assert!(!reap_dead_head(directory.path(), "head.ticket").expect("sharing is not fatal"));
        assert!(directory.path().join("head.ticket").exists());
        restore();
        assert!(!reap_dead_head(directory.path(), "head.ticket").expect("live head"));
    }
}
