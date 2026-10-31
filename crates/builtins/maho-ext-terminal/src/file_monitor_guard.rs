//! Port of senpi `terminal/file-monitor-guard.ts`: a file watch's registration window. Every step
//! races the registration deadline, so a wedged mount fails the registration instead of freezing
//! the loop, and a late result is cleaned up (a value produced after the deadline goes to the late
//! cleanup). Finishing a pending registration either hands its capacity slot to the new watch or
//! releases it.
//!
//! Native shape: upstream arms a `setTimeout` that aborts the controller. Here the deadline is a
//! `tokio::time::Instant` checked whenever the registration is consulted and raced inside
//! [`RegistrationGuard::guard`], so the deadline follows tokio's clock (a paused test clock drives
//! it deterministically) and no timer task outlives the registration.
use crate::manager::CapacityReservation;
use maho_tools::definition::AbortSignal;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

/// The error every guarded step fails with once the registration timed out or was disposed.
pub const REGISTRATION_ABORTED: &str =
    "Cannot create file monitor: registration timed out or was disposed.";

struct PendingState {
    release: Option<CapacityReservation>,
    settled: bool,
}

struct PendingInner {
    key: u64,
    abort: AbortSignal,
    deadline: tokio::time::Instant,
    state: Mutex<PendingState>,
}

/// Upstream `PendingFileRegistration`: the capacity slot, the abort signal and the deadline of one
/// registration in flight.
#[derive(Clone)]
pub struct PendingFileRegistration(Arc<PendingInner>);

impl PendingFileRegistration {
    fn state(&self) -> std::sync::MutexGuard<'_, PendingState> {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The abort signal; it fires on dispose/finish, and on the first check after the deadline.
    pub fn signal(&self) -> &AbortSignal {
        &self.0.abort
    }
    /// Aborted by a finish/dispose, or past its deadline (which aborts it from here on).
    pub fn is_aborted(&self) -> bool {
        if !self.0.abort.is_aborted() && tokio::time::Instant::now() >= self.0.deadline {
            self.0.abort.abort();
        }
        self.0.abort.is_aborted()
    }
    pub fn is_settled(&self) -> bool {
        self.state().settled
    }
    pub fn holds_capacity(&self) -> bool {
        self.state().release.is_some()
    }
    /// Resolves when the registration is aborted or its deadline passes, whichever comes first.
    async fn aborted(&self) {
        tokio::select! {
            () = self.0.abort.cancelled() => {}
            () = tokio::time::sleep_until(self.0.deadline) => self.0.abort.abort(),
        }
    }
}

/// Upstream `RegistrationGuard`: the set of registrations still in flight.
#[derive(Default)]
pub struct RegistrationGuard {
    pending: Mutex<BTreeMap<u64, PendingFileRegistration>>,
    next: std::sync::atomic::AtomicU64,
}

impl RegistrationGuard {
    fn pending(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, PendingFileRegistration>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `open`: track a registration holding `release` that must finish within `timeout_ms`.
    pub fn open(
        &self,
        release: Option<CapacityReservation>,
        timeout_ms: u64,
    ) -> PendingFileRegistration {
        let key = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pending = PendingFileRegistration(Arc::new(PendingInner {
            key,
            abort: AbortSignal::default(),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms),
            state: Mutex::new(PendingState {
                release,
                settled: false,
            }),
        }));
        self.pending().insert(key, pending.clone());
        pending
    }

    /// Registrations still in flight.
    pub fn len(&self) -> usize {
        self.pending().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `finishAll`: abort every registration in flight and release its capacity.
    pub fn finish_all(&self) {
        let pending = self.pending().values().cloned().collect::<Vec<_>>();
        for registration in pending {
            self.finish(&registration, false);
        }
    }

    /// `finish`: end a registration once. With `transfer` the capacity slot is handed back to the
    /// caller (the new watch owns it); otherwise the registration is aborted and the slot released.
    pub fn finish(
        &self,
        pending: &PendingFileRegistration,
        transfer: bool,
    ) -> Option<CapacityReservation> {
        let release = {
            let mut state = pending.state();
            if state.settled {
                return None;
            }
            state.settled = true;
            state.release.take()
        };
        self.pending().remove(&pending.0.key);
        if transfer {
            return release;
        }
        pending.0.abort.abort();
        drop(release);
        None
    }

    /// `await`: run one registration step against the deadline. An aborted registration fails
    /// with [`REGISTRATION_ABORTED`]; a value the step produces after that goes to `late_cleanup`
    /// (upstream closes a handle opened after the deadline), and its late error is dropped.
    pub async fn guard<T, F, C>(
        &self,
        pending: &PendingFileRegistration,
        operation: F,
        late_cleanup: Option<C>,
    ) -> std::io::Result<T>
    where
        T: Send + 'static,
        F: Future<Output = std::io::Result<T>> + Send + 'static,
        C: FnOnce(T) + Send + 'static,
    {
        let mut operation = Box::pin(operation);
        if !pending.is_aborted() {
            tokio::select! {
                biased;
                result = &mut operation => return result,
                () = pending.aborted() => {}
            }
        }
        if let Some(cleanup) = late_cleanup {
            tokio::spawn(async move {
                if let Ok(value) = operation.await {
                    cleanup(value);
                }
            });
        }
        Err(std::io::Error::other(REGISTRATION_ABORTED))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_cleanup(_: u32) {}

    #[tokio::test(start_paused = true)]
    async fn a_step_that_finishes_in_time_returns_its_value_and_error() {
        let guard = RegistrationGuard::default();
        let pending = guard.open(None, 1_000);
        let value = guard
            .guard(
                &pending,
                async { Ok::<u32, std::io::Error>(7) },
                None::<fn(u32)>,
            )
            .await
            .unwrap();
        assert_eq!(value, 7);
        let error = guard
            .guard(
                &pending,
                async { Err::<u32, _>(std::io::Error::other("EACCES")) },
                Some(no_cleanup),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "EACCES");
        assert_eq!(guard.len(), 1);
    }

    /// A wedged step fails at the deadline instead of hanging; the paused clock is the injected time.
    #[tokio::test(start_paused = true)]
    async fn a_wedged_step_fails_at_the_registration_deadline() {
        let guard = RegistrationGuard::default();
        let pending = guard.open(None, 5_000);
        let started = tokio::time::Instant::now();
        let error = guard
            .guard(
                &pending,
                std::future::pending::<std::io::Result<u32>>(),
                None::<fn(u32)>,
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), REGISTRATION_ABORTED);
        assert!(started.elapsed() >= std::time::Duration::from_millis(5_000));
        assert!(pending.is_aborted() && pending.signal().is_aborted());
        // Every later step fails up front.
        let error = guard
            .guard(
                &pending,
                async { Ok::<u32, std::io::Error>(1) },
                None::<fn(u32)>,
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), REGISTRATION_ABORTED);
    }

    /// A handle produced after the registration was aborted goes to the late cleanup.
    #[tokio::test(start_paused = true)]
    async fn a_late_value_is_handed_to_the_late_cleanup() {
        let guard = Arc::new(RegistrationGuard::default());
        let pending = guard.open(None, 5_000);
        let (open_tx, open_rx) = tokio::sync::oneshot::channel::<u32>();
        let (cleaned_tx, cleaned_rx) = tokio::sync::oneshot::channel::<u32>();
        let step = {
            let guard = guard.clone();
            let pending = pending.clone();
            tokio::spawn(async move {
                guard
                    .guard(
                        &pending,
                        async move { open_rx.await.map_err(std::io::Error::other) },
                        Some(move |handle: u32| {
                            cleaned_tx.send(handle).unwrap();
                        }),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        guard.finish_all();
        assert_eq!(
            step.await.unwrap().unwrap_err().to_string(),
            REGISTRATION_ABORTED
        );
        open_tx.send(42).unwrap();
        assert_eq!(cleaned_rx.await.unwrap(), 42);
    }

    /// An already-aborted registration still drains the step so its late value is cleaned up.
    #[tokio::test]
    async fn a_step_started_after_abort_is_cleaned_up_when_it_resolves() {
        let guard = RegistrationGuard::default();
        let pending = guard.open(None, 5_000);
        guard.finish(&pending, false);
        let (cleaned_tx, cleaned_rx) = tokio::sync::oneshot::channel::<u32>();
        let error = guard
            .guard(
                &pending,
                async { Ok::<u32, std::io::Error>(9) },
                Some(move |handle: u32| {
                    cleaned_tx.send(handle).unwrap();
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), REGISTRATION_ABORTED);
        assert_eq!(cleaned_rx.await.unwrap(), 9);
    }

    #[tokio::test]
    async fn finish_releases_or_transfers_capacity_exactly_once() {
        let mut manager = crate::manager::TerminalManager::new(1);
        let guard = RegistrationGuard::default();
        let released = guard.open(manager.reserve().unwrap(), 5_000);
        assert!(released.holds_capacity());
        assert!(manager.reserve().unwrap().is_none());
        assert!(guard.finish(&released, false).is_none());
        assert!(released.is_settled() && released.is_aborted());
        assert_eq!(manager.active_size().unwrap(), 0);
        // A second finish is a no-op.
        assert!(guard.finish(&released, true).is_none());

        let transferred = guard.open(manager.reserve().unwrap(), 5_000);
        let slot = guard.finish(&transferred, true);
        assert!(slot.is_some() && !transferred.is_aborted());
        assert_eq!(manager.active_size().unwrap(), 1);
        guard.finish_all();
        assert_eq!(manager.active_size().unwrap(), 1, "the watch owns the slot");
        drop(slot);
        assert_eq!(manager.active_size().unwrap(), 0);
        assert!(guard.is_empty());
    }

    #[tokio::test]
    async fn finish_all_aborts_and_releases_every_pending_registration() {
        let mut manager = crate::manager::TerminalManager::new(2);
        let guard = RegistrationGuard::default();
        let first = guard.open(manager.reserve().unwrap(), 5_000);
        let second = guard.open(manager.reserve().unwrap(), 5_000);
        assert_eq!(guard.len(), 2);
        guard.finish_all();
        assert!(first.is_aborted() && second.is_aborted());
        assert!(guard.is_empty());
        assert_eq!(manager.active_size().unwrap(), 0);
    }
}
