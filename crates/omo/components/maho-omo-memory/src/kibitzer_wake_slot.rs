//! Machine-wide admission for resident Kibitzer wakes (latest `kibitzer/wake-slot.ts`).
//!
//! Every provider turn of every sidecar on this machine first takes one of N leases from the
//! memory-core `recall-wake` domain. A wake that finds every slot held for the whole bounded wait
//! is told `busy` and simply keeps buffering; the caller's cancellation ends a wait at once.
//! Filesystem failures are not hidden behind `busy` - they propagate so the sidecar backs off.

use std::path::PathBuf;
use std::sync::Arc;

use memory_core::locks::recall_wake_domain::{
    RecallWakeError, RecallWakeLease, RecallWakeLeaseOptions, acquire_recall_wake_lease,
};

/// Longest one wake waits for a lease before it is reported busy.
pub const KIBITZER_WAKE_SLOT_WAIT_MS: u64 = 15_000;
/// Pause between queue polls while waiting: a delay, never a spin.
pub const KIBITZER_WAKE_SLOT_POLL_MS: u64 = 200;

/// A held machine slot.
pub struct KibitzerWakeLease {
    /// 1-based machine slot this wake occupies.
    pub slot: usize,
    inner: RecallWakeLease,
}

impl KibitzerWakeLease {
    /// Hands the slot back, preserving the real failure: `Ok(true)` released, `Ok(false)` already
    /// gone, `Err(RecallWakeError::Io(_))` a filesystem failure the bool API would have swallowed.
    /// Upstream `releaseLease` warns "already gone" on `false` and "release failed" on `Err`.
    pub fn try_release(&self) -> Result<bool, RecallWakeError> {
        self.inner.try_release().map_err(RecallWakeError::Io)
    }

    /// Hands the slot back; `false` when the lease was already released OR the release failed.
    /// Retained for existing callers; the typed [`Self::try_release`] distinguishes the two.
    pub fn release(&self) -> bool {
        self.inner.release()
    }
}

/// The admission verdict.
pub enum KibitzerWakeAdmission {
    /// A lease was taken.
    Acquired { lease: KibitzerWakeLease, waited_ms: i64 },
    /// Every slot stayed with a live owner for the whole bounded wait.
    Busy { waited_ms: i64 },
    /// The caller's signal fired while waiting.
    Aborted,
}

pub struct KibitzerWakeSlotOptions {
    /// The bound identity's `runtime/locks` directory: the domain is machine-wide.
    pub locks_directory: PathBuf,
    /// `memory.recall.max_concurrent_wakes`.
    pub max_concurrent: usize,
    pub wait_timeout_ms: Option<u64>,
    pub poll_ms: Option<u64>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

pub struct KibitzerWakeSlot {
    locks_directory: PathBuf,
    max_concurrent: usize,
    wait_timeout_ms: u64,
    poll_ms: u64,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl KibitzerWakeSlot {
    pub fn new(options: KibitzerWakeSlotOptions) -> Result<Self, String> {
        if options.max_concurrent < 1 {
            return Err(format!(
                "memory.recall.max_concurrent_wakes must be a positive integer, got {}",
                options.max_concurrent
            ));
        }
        Ok(Self {
            locks_directory: options.locks_directory,
            max_concurrent: options.max_concurrent,
            wait_timeout_ms: options.wait_timeout_ms.unwrap_or(KIBITZER_WAKE_SLOT_WAIT_MS),
            poll_ms: options.poll_ms.unwrap_or(KIBITZER_WAKE_SLOT_POLL_MS),
            now: options.now,
        })
    }

    /// Resolves with the admission verdict; rejects only on a filesystem failure of the lock domain.
    pub fn acquire(
        &self,
        cancellation: Option<&dyn Fn() -> bool>,
    ) -> Result<KibitzerWakeAdmission, RecallWakeError> {
        let started = (self.now)();
        let options = RecallWakeLeaseOptions {
            max_concurrent: Some(self.max_concurrent),
            wait_timeout_ms: Some(self.wait_timeout_ms),
            retry_delay_ms: Some(self.poll_ms),
            cancellation,
        };
        match acquire_recall_wake_lease(&self.locks_directory, &options) {
            Ok(lease) => Ok(KibitzerWakeAdmission::Acquired {
                lease: KibitzerWakeLease { slot: lease.slot, inner: lease },
                waited_ms: ((self.now)() - started).max(0),
            }),
            Err(RecallWakeError::Busy(_)) => Ok(KibitzerWakeAdmission::Busy {
                waited_ms: ((self.now)() - started).max(0),
            }),
            Err(RecallWakeError::Aborted) => Ok(KibitzerWakeAdmission::Aborted),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "kibitzer_wake_slot_tests.rs"]
mod tests;
