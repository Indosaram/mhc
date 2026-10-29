//! Recall-wake counting-lease domain.
use super::counting_lease::{self, CountingLeaseDomain, CountingLeaseError};
pub use super::counting_lease::{
    CountingLease as RecallWakeLease, CountingLeaseOptions as RecallWakeLeaseOptions,
};
pub use super::counting_lease::{
    CountingLeaseTicketFs as RecallWakeTicketFs,
    set_counting_lease_ticket_fs_for_tests as set_recall_wake_ticket_fs_for_tests,
};
use std::path::{Path, PathBuf};
pub const RECALL_WAKE_DEFAULT_SLOTS: usize = 2;
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

fn map_error(error: CountingLeaseError) -> RecallWakeError {
    match error {
        CountingLeaseError::Busy(busy) => RecallWakeError::Busy(RecallWakeBusyError {
            waited_ms: busy.waited_ms,
            max_concurrent: busy.max_concurrent,
        }),
        CountingLeaseError::Aborted => RecallWakeError::Aborted,
        CountingLeaseError::InvalidOptions(message) => RecallWakeError::InvalidOptions(message),
        CountingLeaseError::Lock(message) => RecallWakeError::Lock(message),
        CountingLeaseError::Io(error) => RecallWakeError::Io(error),
    }
}
pub fn recall_wake_lock_path(
    locks_directory: &Path,
    slot: usize,
) -> Result<PathBuf, RecallWakeError> {
    counting_lease::counting_lease_lock_path(locks_directory, "recall-wake", slot)
        .map_err(map_error)
}
pub fn recall_wake_ticket_directory(locks_directory: &Path) -> PathBuf {
    counting_lease::counting_lease_ticket_directory(locks_directory, "recall-wake")
}
pub fn acquire_recall_wake_lease(
    locks_directory: &Path,
    options: &RecallWakeLeaseOptions<'_>,
) -> Result<RecallWakeLease, RecallWakeError> {
    counting_lease::acquire_counting_lease(
        locks_directory,
        &CountingLeaseDomain {
            purpose: "recall-wake",
            default_slots: RECALL_WAKE_DEFAULT_SLOTS,
            wait_timeout_ms: Some(0),
        },
        options,
    )
    .map_err(map_error)
}
pub fn with_recall_wake_lease<T>(
    locks_directory: &Path,
    options: &RecallWakeLeaseOptions<'_>,
    operation: impl FnOnce(&RecallWakeLease) -> T,
) -> Result<T, RecallWakeError> {
    let lease = acquire_recall_wake_lease(locks_directory, options)?;
    struct Release<'a>(&'a RecallWakeLease);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _release = Release(&lease);
    Ok(operation(&lease))
}
#[cfg(test)]
use super::counting_lease::{list_tickets, publish_ticket, reap_dead_head};
#[cfg(test)]
use crate::{
    fs,
    locks::lock_record::{CreateLockRecordOptions, create_lock_record},
};
#[cfg(test)]
const TICKET_PURPOSE: &str = "recall-wake:ticket";
#[cfg(test)]
#[path = "recall_wake_domain_tests.rs"]
mod tests;
