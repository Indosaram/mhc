//! The resident sidecar ownership registry (senpi `Rte`), which is what triggers the sweep.
//!
//! Each bound session owns its sidecar directory for as long as it lives: it takes the
//! `recall-sidecar.<encoded>.lock` for the session's life and reports its encoded name as owned so a
//! concurrent sweep never reclaims it. Releasing that ownership is what finally makes the directory
//! eligible, so shutdown releases the lock and then sweeps.
//!
//! Two dedup sets with different lifetimes, exactly as upstream:
//!
//! - `kick_once` lives for the process and never clears, so `own` sweeps at most once per recall
//!   directory. Without it every new session would re-stat the whole tree.
//! - `in_flight` lives until a sweep settles, so two sweeps of one recall directory never overlap.
//!
//! Two deliberate divergences from upstream, both recorded in the parity ledger:
//!
//! 1. The owned set is read from the live map at the moment each entry is examined, not snapshotted
//!    when the sweep is kicked. A session that takes ownership while a sweep is already running must
//!    still be protected; upstream's snapshot would miss it.
//! 2. Upstream fires its sweep through a fire-and-forget promise. Here the caller supplies the splice
//!    (`offload_blocking`), because both the sweep and the owner acquire are blocking filesystem work
//!    that must never run on the thread that paints and reads input.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use memory_core::locks::acquire::{AcquireLockOptions, acquire_lock, release_lock};
use memory_core::locks::lock_record::{CreateLockRecordOptions, LockRecord, create_lock_record};

use crate::sidecar_sweep::{SweepOptions, SweepReport, sweep_recall_sidecars};

/// The owner lock wait. Upstream spends two seconds here because a session that fails to take the lock
/// is protected only inside its own process: another process sees an expired, unowned directory and
/// reclaims it. A zero wait - correct for the sweep's claim - would make every concurrent claim a
/// permanently un-locked live session, since nothing ever retries.
const OWNER_LOCK_WAIT_MS: u64 = 2_000;

const LOCK_PURPOSE: &str = "recall-sidecar";

/// One session's claim on its sidecar directory.
///
/// `acquired` carries the record the acquire actually wrote. Releasing needs the SAME nonce, so a
/// freshly-built record cannot release the lock.
struct OwnedSidecar {
    encoded: String,
    lock_path: PathBuf,
    recall_dir: PathBuf,
    locks_dir: PathBuf,
    acquired: Arc<Mutex<Option<LockRecord>>>,
}

/// The process-wide owner registry: one per `MemoryRecallWiring`, which the mount builds once per host.
///
/// The offload seam, the warning sink and the clock are passed per call rather than captured at
/// construction, so the registry always uses the wiring's current options.
#[derive(Default)]
pub struct SidecarOwner {
    owned: Mutex<BTreeMap<String, OwnedSidecar>>,
    kick_once: Mutex<BTreeSet<PathBuf>>,
    in_flight: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// Signalled when `in_flight` becomes empty, so `when_idle` waits on the event, not a poll.
    idle: Arc<tokio::sync::Notify>,
}

/// What one ownership operation needs from its caller, read at call time.
pub struct OwnerDeps<'a> {
    pub offload: &'a (dyn Fn(BlockingWork) + Send + Sync),
    pub warn: &'a Arc<dyn Fn(&str) + Send + Sync>,
    pub now_ms: &'a Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// A unit of blocking work handed to the offload seam.
pub type BlockingWork = Box<dyn FnOnce() + Send + 'static>;

impl SidecarOwner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A registry that schedules nothing: it records ownership and releases it. Used where no offload
    /// seam exists (the child-sentinel paths) and by tests that only assert the bookkeeping.
    pub fn inert() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Takes ownership of a session's sidecar directory and kicks the first sweep of that recall
    /// directory.
    ///
    /// The entry is recorded BEFORE the lock is acquired, so a sweep kicked from here already sees the
    /// encoded name as owned. The acquire itself is blocking (it sleeps and fsyncs the record), so it
    /// runs on the offload seam.
    pub fn own(&self, deps: &OwnerDeps<'_>, session_id: &str, recall_dir: &Path, locks_dir: &Path) {
        let encoded = crate::sidecar_sweep::encode_session(session_id);
        let lock_path = crate::sidecar_sweep::sidecar_owner_lock(locks_dir, &encoded);
        let entry = OwnedSidecar {
            encoded: encoded.clone(),
            lock_path: lock_path.clone(),
            recall_dir: recall_dir.to_path_buf(),
            locks_dir: locks_dir.to_path_buf(),
            acquired: Arc::new(Mutex::new(None)),
        };
        let acquired = Arc::clone(&entry.acquired);
        let replaced = self
            .owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id.to_string(), entry);
        if replaced.is_some() {
            return;
        }
        let warn = Arc::clone(deps.warn);
        (deps.offload)(Box::new(move || {
            let record = match create_lock_record(
                LOCK_PURPOSE,
                CreateLockRecordOptions {
                    run_id: Some(encoded.clone()),
                },
            ) {
                Ok(record) => record,
                Err(error) => {
                    warn(&format!(
                        "omo-senpi kibitzer sidecar directory owner lock unavailable: {error}"
                    ));
                    return;
                }
            };
            if let Err(error) = acquire_lock(
                &lock_path,
                &record,
                &AcquireLockOptions {
                    wait_timeout_ms: Some(OWNER_LOCK_WAIT_MS),
                    retry_delay_ms: None,
                    cancellation: None,
                },
            ) {
                warn(&format!(
                    "omo-senpi kibitzer sidecar directory owner lock unavailable: {error}"
                ));
                return;
            }
            // Kept for the release: `release_lock` verifies this nonce, so the record built at
            // acquire time is the only one that can release the lock.
            *acquired
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record);
        }));
        self.kick_sweep_once(deps, recall_dir, locks_dir);
    }

    /// Releases a session's ownership: drop the entry, release the lock, then sweep. The release is
    /// what makes the directory eligible, so it must precede the sweep.
    ///
    /// Called from `session_shutdown` AFTER `sidecar.shutdown()` resolves, which has no identity context
    /// left to resolve, so the paths come from the stored entry.
    pub fn release_and_sweep(&self, deps: &OwnerDeps<'_>, session_id: &str) {
        let entry = self
            .owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
        let Some(entry) = entry else {
            return;
        };
        let warn = Arc::clone(deps.warn);
        let acquired = Arc::clone(&entry.acquired);
        let lock_path = entry.lock_path.clone();
        // Release and sweep are ONE task, in that order. Two tasks would race on a real thread pool,
        // and a sweep that runs first finds its own session's lock still held and keeps the directory.
        // The entry is already out of the owned map, so this sweep may reclaim it.
        //
        // A sweep already in flight keeps its own (earlier) owned snapshot, so releasing here and
        // letting it finish would leave this session's directory for a LATER sweep that may never come.
        // The release therefore always schedules, and a sweep follows it whenever none is running.
        let sweep_after = self.claim_in_flight(&entry.recall_dir);
        let owned = if sweep_after {
            self.owned_in(&entry.recall_dir)
        } else {
            Vec::new()
        };
        let now_ms = Arc::clone(deps.now_ms);
        let in_flight = Arc::clone(&self.in_flight);
        let idle = Arc::clone(&self.idle);
        let recall_dir = entry.recall_dir;
        let locks_dir = entry.locks_dir;
        let swept = recall_dir.clone();
        (deps.offload)(Box::new(move || {
            release_held_lock(&lock_path, &acquired, warn.as_ref());
            if sweep_after {
                run_sweep(&recall_dir, &locks_dir, now_ms(), owned, warn.as_ref());
                // Only the sweep that claimed the slot may clear it; clearing a slot owned by a
                // still-running sweep would let a second one start on the same directory.
                let empty = {
                    let mut in_flight = in_flight
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    in_flight.remove(&swept);
                    in_flight.is_empty()
                };
                if empty {
                    idle.notify_waiters();
                }
            }
        }));
    }

    pub fn owns(&self, session_id: &str) -> bool {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(session_id)
    }

    fn owned_in(&self, recall_dir: &Path) -> Vec<String> {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|entry| entry.recall_dir == recall_dir)
            .map(|entry| entry.encoded.clone())
            .collect()
    }

    fn kick_sweep_once(&self, deps: &OwnerDeps<'_>, recall_dir: &Path, locks_dir: &Path) {
        let first = self
            .kick_once
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(recall_dir.to_path_buf());
        if first {
            self.sweep(deps, recall_dir.to_path_buf(), locks_dir.to_path_buf());
        }
    }

    fn sweep(&self, deps: &OwnerDeps<'_>, recall_dir: PathBuf, locks_dir: PathBuf) {
        if !self.claim_in_flight(&recall_dir) {
            return;
        }
        let owned = self.owned_in(&recall_dir);
        let warn = Arc::clone(deps.warn);
        let now_ms = Arc::clone(deps.now_ms);
        let in_flight = Arc::clone(&self.in_flight);
        let idle = Arc::clone(&self.idle);
        let swept = recall_dir.clone();
        (deps.offload)(Box::new(move || {
            run_sweep(&recall_dir, &locks_dir, now_ms(), owned, warn.as_ref());
            let empty = {
                let mut in_flight = in_flight
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                in_flight.remove(&swept);
                in_flight.is_empty()
            };
            if empty {
                idle.notify_waiters();
            }
        }));
    }

    /// Claims the one in-flight slot of a recall directory. `false` means a sweep is already running,
    /// so the caller must not schedule another.
    fn claim_in_flight(&self, recall_dir: &Path) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(recall_dir.to_path_buf())
    }

    /// The recall directories a sweep is currently running for. Empty means every scheduled sweep has
    /// finished, which is the drain point tests and shutdown observe.
    pub fn in_flight(&self) -> Vec<PathBuf> {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// `Rte.whenIdle`'s owner half: resolves once no sweep is in flight.
    ///
    /// The registration happens BEFORE the emptiness check, so a sweep that finishes between the two
    /// still wakes this waiter - the waiter cannot miss the notification it is waiting for.
    pub async fn when_idle(&self) -> Result<(), String> {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            if self.in_flight().is_empty() {
                return Ok(());
            }
            notified.await;
        }
    }
}

fn release_held_lock(
    lock_path: &Path,
    acquired: &Mutex<Option<LockRecord>>,
    warn: &(dyn Fn(&str) + Send + Sync),
) {
    let held = acquired
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(record) = held
        && let Err(error) = release_lock(lock_path, &record)
    {
        warn(&format!(
            "omo-senpi kibitzer sidecar directory owner lock release failed: {error}"
        ));
    }
}

fn run_sweep(
    recall_dir: &Path,
    locks_dir: &Path,
    now_ms: i64,
    owned: Vec<String>,
    warn: &(dyn Fn(&str) + Send + Sync),
) {
    let report = sweep_recall_sidecars(SweepOptions::new(
        recall_dir, locks_dir, now_ms, owned, warn,
    ));
    log_report(&report, warn);
}

fn log_report(report: &SweepReport, warn: &(dyn Fn(&str) + Send + Sync)) {
    if report.pruned.is_empty() {
        return;
    }
    warn(&format!(
        "omo-senpi kibitzer sidecar sweep pruned {} directories, kept {}",
        report.pruned.len(),
        report.kept.len()
    ));
}

#[cfg(test)]
#[path = "sidecar_owner_tests.rs"]
mod tests;
