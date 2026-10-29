//! One wake's machine-wide admission (latest `kibitzer/sidecar-admission.ts`).
//!
//! A seed or a follow-up takes ONE machine-wide wake lease before its provider turn starts and
//! holds it until the turn settles; a steer joins the running turn under that same lease. Waiting
//! is bounded and abortable (session shutdown cuts it short), a lease is handed back exactly once
//! on whichever exit comes first, and a wait that yields no lease is turned into the offer result.
//!
//! # Boundaries (this unit owns admission / release / refusal only)
//!
//! * [`KibitzerWakeSlot::acquire`] is SYNCHRONOUS: it performs a bounded blocking filesystem wait in
//!   the memory-core `recall-wake` domain. It is therefore run through the retained NATIVE blocking
//!   executor ([`KibitzerBlockingExecutor`]) and NEVER directly on the async reactor.
//! * The turn lifecycle ([`TurnLifecycle`]) and the recovery module ([`KibitzerRecovery`]) are
//!   SEPARATE, already-returned producers; this unit holds them directly (`Arc<TurnLifecycle>` /
//!   `Arc<KibitzerRecovery>`) and calls their real methods. No adapter trait is invented.
//! * A dropped `admit` future must not leave the installed admission behind or leak a lease the
//!   worker already won: [`AdmissionGuard`] owns this wait's installed handle, and the worker hands
//!   its lease over as a [`PendingLease`] that releases itself - REPORTING the exact outcome - if it
//!   is never consumed.
//! * Every hand-back reports its real result: `Ok(true)` released, `Ok(false)` already gone (warn),
//!   `Err` a filesystem failure the old `bool` API swallowed (warn with the error). No path silently
//!   loses ownership.
//! * No reporting IO, timers, wake triggering or child I/O lives here.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};

use memory_core::locks::recall_wake_domain::RecallWakeError;
use serde_json::json;

use crate::kibitzer_contract::{KibitzerBufferedReason, KibitzerOfferResult};
use crate::kibitzer_sidecar_core::{SidecarCore, Turn, describe};
use crate::kibitzer_sidecar_outcome::start_failure_end;
use crate::kibitzer_sidecar_recovery::KibitzerRecovery;
use crate::kibitzer_sidecar_turn::TurnLifecycle;
use crate::kibitzer_wake_slot::{KibitzerWakeAdmission, KibitzerWakeLease, KibitzerWakeSlot};

/// Hands one lease back and reports the outcome EXACTLY, never silently: `Ok(true)` is a clean
/// release, `Ok(false)` warns that the slot was already gone, and `Err` warns the real filesystem
/// failure the old `bool` API would have swallowed. `wake` is the turn that held the lease, absent
/// when no turn ever took it (a wait that was dropped before its lease was consumed).
fn release_and_report(core: &SidecarCore, lease: &KibitzerWakeLease, wake: Option<u64>) {
    match lease.try_release() {
        Ok(true) => {}
        Ok(false) => core.warn(
            "omo-senpi kibitzer sidecar wake lease was already gone",
            Some(json!({ "wake": wake, "slot": lease.slot })),
        ),
        Err(error) => core.warn(
            "omo-senpi kibitzer sidecar wake lease release failed",
            Some(json!({ "wake": wake, "slot": lease.slot, "error": describe(&error) })),
        ),
    }
}

/// A machine lease the worker took but has not yet handed to a turn. `KibitzerWakeLease` has no
/// `Drop`, so this wrapper releases the slot if the value is dropped without being taken, reporting
/// the exact release outcome through the sidecar's warn sink rather than losing it silently.
pub struct PendingLease {
    lease: Option<KibitzerWakeLease>,
    core: Arc<SidecarCore>,
}

impl PendingLease {
    fn new(lease: KibitzerWakeLease, core: Arc<SidecarCore>) -> Self {
        Self { lease: Some(lease), core }
    }

    /// Hands the lease to the turn; the wrapper then releases nothing. `None` only if it was already
    /// taken, which cannot happen on this path.
    pub fn into_lease(mut self) -> Option<KibitzerWakeLease> {
        self.lease.take()
    }
}

impl Drop for PendingLease {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            // The wait that would have owned this lease is gone; hand the slot back and report the
            // exact outcome. No turn ever took it, so there is no wake number to report.
            release_and_report(&self.core, &lease, None);
        }
    }
}

/// The lock-domain verdict of one blocking acquire, as the worker returns it. The lease is carried
/// drop-safely so a result nobody consumes still hands the slot back with a reported outcome.
pub enum WakeAdmissionAttempt {
    Acquired { lease: PendingLease, waited_ms: i64 },
    Busy { waited_ms: i64 },
    Aborted,
    Error(RecallWakeError),
}

/// One blocking work item: the synchronous acquire the executor runs off the reactor.
pub type BlockingAcquire = Box<dyn FnOnce() -> WakeAdmissionAttempt + Send + 'static>;

/// The future a retained native blocking executor resolves with the work item's value.
pub type BlockingAcquireFuture = Pin<Box<dyn Future<Output = WakeAdmissionAttempt> + Send + 'static>>;

/// The retained NATIVE blocking executor.
///
/// PRODUCER DEMAND (this host seam is not delivered in the memory component; no blocking-executor
/// helper exists there): the composition supplies an implementation backed by a retained blocking
/// worker - a `tokio::runtime::Handle::spawn_blocking` closure, or a dedicated blocking thread pool.
/// It runs [`KibitzerWakeSlot::acquire`] off the async reactor, and DROPPING the returned future
/// must NOT cancel the worker: the worker's own completion is what hands back a lease it won late.
/// When the returned future is dropped, the executor must still DROP the worker's produced value on
/// completion (never retain, buffer or `mem::forget` it) - that drop is what releases an unconsumed
/// [`PendingLease`] and reports its outcome. See the unit receipt (D4b).
pub trait KibitzerBlockingExecutor: Send + Sync {
    fn run_blocking(&self, task: BlockingAcquire) -> BlockingAcquireFuture;
}

/// The admission verdict: the wake slot's own verdict, or a lock-domain error
/// (`KibitzerWakeAdmission | { status: "error"; error: unknown }`).
pub enum Admission {
    /// A machine slot was taken; the lease is handed to the turn and released on exit.
    Acquired { lease: KibitzerWakeLease, waited_ms: i64 },
    /// Every slot stayed with a live owner for the whole bounded wait.
    Busy { waited_ms: i64 },
    /// The caller's signal fired while waiting.
    Aborted,
    /// A filesystem failure of the lock domain; the sidecar backs off.
    Error(RecallWakeError),
}

/// An admission that yielded no lease (`Exclude<Admission, { status: "acquired" }>`).
pub enum Refusal {
    Busy { waited_ms: i64 },
    Aborted,
    Error(RecallWakeError),
}

impl Admission {
    /// The refusal view of an admission; `Ok(admitted)` when a lease was taken.
    pub fn into_refusal(self) -> Result<Admission, Refusal> {
        match self {
            Admission::Busy { waited_ms } => Err(Refusal::Busy { waited_ms }),
            Admission::Aborted => Err(Refusal::Aborted),
            Admission::Error(error) => Err(Refusal::Error(error)),
            admitted @ Admission::Acquired { .. } => Ok(admitted),
        }
    }
}

/// Owns THIS wait's installed admission. Dropping it - a completed wait OR a future dropped
/// mid-flight - aborts this wait's own probe and removes this wait's handle. It never clears a newer
/// admission: the core's `end_admission` clears unconditionally, so a possibly-stale guard must not
/// call it and instead writes the record field under the same lock after a pointer check.
struct AdmissionGuard {
    core: Arc<SidecarCore>,
    signal: Arc<AtomicBool>,
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        // Abort THIS wait's probe so a parked blocking acquire ends at its next poll, even when the
        // awaiting future is dropped rather than completed.
        self.signal.store(true, Ordering::SeqCst);
        // Remove only OUR handle; a newer admission installed after ours is left intact.
        let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
        let still_ours = record
            .admission
            .as_ref()
            .map(|current| Arc::ptr_eq(&current.signal(), &self.signal))
            .unwrap_or(false);
        if still_ours {
            record.admission = None;
        }
    }
}

/// The resident wake admission (upstream `SidecarAdmission`).
pub struct SidecarAdmission {
    core: Arc<SidecarCore>,
    wake_slot: Arc<KibitzerWakeSlot>,
    turns: Arc<TurnLifecycle>,
    recovery: Arc<KibitzerRecovery>,
    executor: Arc<dyn KibitzerBlockingExecutor>,
}

impl SidecarAdmission {
    /// Waits (bounded) for a machine slot; shutdown aborts the wait through `core.abort_admission`.
    pub async fn admit(&self) -> Admission {
        // Closing before admission never starts a ticket wait.
        if self.core.is_closing() {
            return Admission::Aborted;
        }
        let cancel = self.core.begin_admission();
        let probe = cancel.signal();
        // The guard owns this wait's installed admission for its whole life, including a drop.
        let _guard = AdmissionGuard { core: Arc::clone(&self.core), signal: Arc::clone(&probe) };
        let slot = Arc::clone(&self.wake_slot);
        let worker_probe = Arc::clone(&probe);
        let worker_core = Arc::clone(&self.core);
        let attempt = self
            .executor
            .run_blocking(Box::new(move || {
                let cancelled = || worker_probe.load(Ordering::SeqCst);
                match slot.acquire(Some(&cancelled)) {
                    Ok(KibitzerWakeAdmission::Acquired { lease, waited_ms }) => {
                        if worker_probe.load(Ordering::SeqCst) {
                            // The probe fired after the lease was taken: hand it straight back and
                            // report the exact outcome - the worker owns this lease, so a silent
                            // release would lose the failure.
                            release_and_report(&worker_core, &lease, None);
                            WakeAdmissionAttempt::Aborted
                        } else {
                            // A drop-safe hand-over: if this result is never consumed (the awaiting
                            // future was dropped), dropping it releases the slot and reports it.
                            WakeAdmissionAttempt::Acquired { lease: PendingLease::new(lease, worker_core), waited_ms }
                        }
                    }
                    Ok(KibitzerWakeAdmission::Busy { waited_ms }) => WakeAdmissionAttempt::Busy { waited_ms },
                    Ok(KibitzerWakeAdmission::Aborted) => WakeAdmissionAttempt::Aborted,
                    Err(error) => WakeAdmissionAttempt::Error(error),
                }
            }))
            .await;
        match attempt {
            WakeAdmissionAttempt::Acquired { lease, waited_ms } => {
                let Some(lease) = lease.into_lease() else { return Admission::Aborted };
                // Shutdown raced the acquisition between the worker's check and here: hand the lease
                // back before reporting disposed, and never launch a child after closing.
                if self.core.is_closing() {
                    release_and_report(&self.core, &lease, None);
                    return Admission::Aborted;
                }
                Admission::Acquired { lease, waited_ms }
            }
            WakeAdmissionAttempt::Busy { waited_ms } => Admission::Busy { waited_ms },
            WakeAdmissionAttempt::Aborted => Admission::Aborted,
            // Upstream's `catch`: an error raised while the caller's signal fired is an abort, not a
            // start failure.
            WakeAdmissionAttempt::Error(_) if probe.load(Ordering::SeqCst) => Admission::Aborted,
            WakeAdmissionAttempt::Error(error) => Admission::Error(error),
        }
    }

    /// Idempotent: the first caller on any exit path hands the slot back, later ones find nothing.
    /// The release is synchronous (`KibitzerWakeLease::try_release`), so no `.await` occurs; the
    /// method stays `async` to keep the caller's exit-path ordering. The outcome is reported exactly:
    /// `Ok(true)` released, `Ok(false)` already gone, `Err` a real release failure.
    pub async fn release_lease(&self, turn: &Turn) {
        let lease = {
            let mut guard = turn.lease.lock().unwrap_or_else(PoisonError::into_inner);
            guard.take()
        };
        let Some(lease) = lease else { return };
        release_and_report(&self.core, &lease, Some(turn.wake));
    }

    /// An admission that yielded no lease, turned into the offer result; an error is a start failure.
    pub fn refused(&self, refusal: Refusal, generation: u64, max_items: usize, candidate_count: usize) -> KibitzerOfferResult {
        match refusal {
            Refusal::Busy { .. } => KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::SlotBusy },
            Refusal::Aborted => KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Disposed },
            Refusal::Error(error) => {
                let turn = self.turns.new_turn(generation, max_items, candidate_count);
                let end = start_failure_end(&format!("wake admission failed: {}", describe(&error)), None);
                self.turns.report(&turn, &end, &[], None, None);
                self.recovery.enter_backoff();
                KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Backoff }
            }
        }
    }
}

/// Builds one sidecar's admission (upstream `createAdmission(core, turns, recovery)`).
///
/// The wake slot is passed explicitly: the core record deliberately does NOT hold it (it belongs to
/// the composition), so this unit receives it rather than reading `core.options.wake_slot`.
pub fn create_admission(
    core: Arc<SidecarCore>,
    wake_slot: Arc<KibitzerWakeSlot>,
    turns: Arc<TurnLifecycle>,
    recovery: Arc<KibitzerRecovery>,
    executor: Arc<dyn KibitzerBlockingExecutor>,
) -> SidecarAdmission {
    SidecarAdmission { core, wake_slot, turns, recovery, executor }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll, Wake, Waker};

    use memory_core::locks::recall_wake_domain::recall_wake_lock_path;

    use super::*;
    use crate::kibitzer_contract::KibitzerWakeSpawn;
    use crate::kibitzer_sidecar_core::{AdmissionCancel, KibitzerSidecarCoreOptions, create_sidecar_core};
    use crate::kibitzer_sidecar_outcome::KibitzerWakeOutcome;
    use crate::kibitzer_sidecar_recovery::create_recovery;
    use crate::kibitzer_sidecar_turn::create_turn_lifecycle;
    use crate::kibitzer_session_resources::KibitzerSessionResourceRegistry;
    use crate::kibitzer_wake_slot::KibitzerWakeSlotOptions;

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    /// The manual retained executor: it counts invocations and NEVER completes on its own, so a test
    /// owns exactly when (and whether) the worker's result is produced. It runs nothing on the reactor
    /// and never wakes the awaiting future.
    #[derive(Default)]
    struct ManualExecutor {
        calls: AtomicUsize,
    }

    impl KibitzerBlockingExecutor for ManualExecutor {
        fn run_blocking(&self, _task: BlockingAcquire) -> BlockingAcquireFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    fn core_with_warnings(warnings: Arc<Mutex<Vec<String>>>) -> Arc<SidecarCore> {
        let resources = KibitzerSessionResourceRegistry::new().for_session("session");
        create_sidecar_core(KibitzerSidecarCoreOptions {
            session_id: "session".to_string(),
            resources,
            tool_budget: None,
            wake_deadline_ms: None,
            sidecar_max_tokens: None,
            event_caps: None,
            now: Some(Arc::new(|| 0)),
            random: None,
            timers: None,
            warn: Some(Arc::new(move |message: &str| {
                warnings.lock().unwrap_or_else(PoisonError::into_inner).push(message.to_string());
            })),
        })
    }

    fn slot(dir: &std::path::Path, max_concurrent: usize) -> KibitzerWakeSlot {
        KibitzerWakeSlot::new(KibitzerWakeSlotOptions {
            locks_directory: dir.to_path_buf(),
            max_concurrent,
            wait_timeout_ms: Some(0),
            poll_ms: Some(1),
            now: Arc::new(|| 0),
        })
        .unwrap()
    }

    fn admission_with(
        core: Arc<SidecarCore>,
        wake_slot: Arc<KibitzerWakeSlot>,
        executor: Arc<dyn KibitzerBlockingExecutor>,
    ) -> SidecarAdmission {
        let spawn: KibitzerWakeSpawn = Arc::new(|_future: Pin<Box<dyn Future<Output = ()> + Send>>| {});
        let turns = Arc::new(create_turn_lifecycle(
            Arc::clone(&core),
            Arc::clone(&spawn),
            Arc::new(|_outcome: KibitzerWakeOutcome| {}),
        ));
        let recovery = Arc::new(create_recovery(Arc::clone(&core), spawn));
        create_admission(core, wake_slot, turns, recovery, executor)
    }

    fn acquire(wake_slot: &KibitzerWakeSlot) -> KibitzerWakeLease {
        match wake_slot.acquire(None).unwrap() {
            KibitzerWakeAdmission::Acquired { lease, .. } => lease,
            _ => panic!("expected an acquired lease"),
        }
    }

    fn installed_admission(core: &SidecarCore) -> AdmissionCancel {
        core.record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .admission
            .clone()
            .expect("an admission is installed while the wait is pending")
    }

    fn warning_count(warnings: &Arc<Mutex<Vec<String>>>) -> usize {
        warnings.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    #[test]
    fn given_a_closing_core_when_admitted_then_no_blocking_worker_starts() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let core = core_with_warnings(Arc::clone(&warnings));
        core.mark_closing();
        let dir = tempfile::tempdir().unwrap();
        let executor = Arc::new(ManualExecutor::default());
        let admission = admission_with(Arc::clone(&core), Arc::new(slot(dir.path(), 1)), executor.clone());

        let waker = Waker::from(Arc::new(NoopWake));
        let mut cx = Context::from_waker(&waker);
        let mut future = Box::pin(admission.admit());

        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Ready(Admission::Aborted)));
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0, "closing must never start a blocking worker");
        assert!(
            core.record.lock().unwrap_or_else(PoisonError::into_inner).admission.is_none(),
            "a closing core installs no admission handle at all"
        );
    }

    #[test]
    fn given_a_pending_admit_when_dropped_then_it_aborts_its_probe_and_removes_only_its_own_handle() {
        let waker = Waker::from(Arc::new(NoopWake));
        let mut cx = Context::from_waker(&waker);

        // (A) No newer admission: the dropped wait aborts its own probe and removes its own handle.
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let core = core_with_warnings(Arc::clone(&warnings));
        let dir = tempfile::tempdir().unwrap();
        let executor = Arc::new(ManualExecutor::default());
        let admission = admission_with(Arc::clone(&core), Arc::new(slot(dir.path(), 1)), executor.clone());

        let mut future = Box::pin(admission.admit());
        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending), "the manual worker never completes");
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1, "the pending wait started exactly one worker");
        let own = installed_admission(&core);

        drop(future);
        assert!(own.is_aborted(), "dropping the wait aborts its own probe");
        assert!(
            core.record.lock().unwrap_or_else(PoisonError::into_inner).admission.is_none(),
            "the dropped wait removes its own admission handle"
        );

        // (B) A newer admission installed first survives: only the OWNED handle is removed.
        let warnings_b = Arc::new(Mutex::new(Vec::new()));
        let core_b = core_with_warnings(Arc::clone(&warnings_b));
        let dir_b = tempfile::tempdir().unwrap();
        let executor_b = Arc::new(ManualExecutor::default());
        let admission_b = admission_with(Arc::clone(&core_b), Arc::new(slot(dir_b.path(), 1)), executor_b.clone());

        let mut future_b = Box::pin(admission_b.admit());
        assert!(matches!(future_b.as_mut().poll(&mut cx), Poll::Pending));
        let own_b = installed_admission(&core_b);
        let newer = core_b.begin_admission();

        drop(future_b);
        assert!(own_b.is_aborted(), "the dropped wait still aborts its own probe");
        assert!(
            core_b.record.lock().unwrap_or_else(PoisonError::into_inner).admission.is_some(),
            "a newer admission must survive the dropped wait"
        );
        assert!(!newer.is_aborted(), "the newer admission's probe is untouched");
    }

    #[test]
    fn given_an_unconsumed_pending_lease_when_dropped_then_the_real_slot_frees_and_the_outcome_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let core = core_with_warnings(Arc::clone(&warnings));
        let wake_slot = slot(dir.path(), 1);

        // (A) A clean release frees the REAL slot and reports exactly zero warnings (Ok(true) silent).
        let lease = acquire(&wake_slot);
        drop(PendingLease::new(lease, Arc::clone(&core)));
        assert_eq!(warning_count(&warnings), 0, "a clean release emits no warning");
        match wake_slot.acquire(None).unwrap() {
            KibitzerWakeAdmission::Acquired { lease, .. } => assert!(lease.try_release().unwrap()),
            _ => panic!("the dropped lease must have freed the real slot"),
        }

        // (B) An already-gone release is reported exactly once: the real lock file is removed first.
        let lease = acquire(&wake_slot);
        let lock_path = recall_wake_lock_path(dir.path(), lease.slot).unwrap();
        std::fs::remove_file(&lock_path).unwrap();
        drop(PendingLease::new(lease, Arc::clone(&core)));
        assert_eq!(warning_count(&warnings), 1, "an already-gone release emits exactly one warning");
    }
}
