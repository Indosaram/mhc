//! Port of model-catalog-refresh.ts.
//!
//! Share concurrent interactive all-catalog refreshes while keeping each caller's cancellation
//! independent. senpi keys the in-flight refresh by runtime identity through a `WeakMap`; Rust has
//! no weak identity map over an arbitrary runtime value, so callers pass a stable `runtime_key`
//! (for example a pointer address or an explicit registry id) and a `start` closure that runs the
//! actual `modelRuntime.refresh({ signal })` for that runtime.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use maho_ai::models::ModelsRefreshResult;
use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal, race_with_abort_signal};

/// One in-flight refresh shared by every waiter on the same runtime.
struct ActiveRefresh {
    controller: AbortController,
    result: tokio::sync::watch::Sender<Option<ModelsRefreshResult>>,
    receiver: tokio::sync::watch::Receiver<Option<ModelsRefreshResult>>,
    waiters: AtomicUsize,
    /// Identity token: an entry is only removed/aborted while it is still the live one.
    token: u64,
}

impl ActiveRefresh {
    fn new(token: u64) -> Self {
        let (result, receiver) = tokio::sync::watch::channel(None);
        Self {
            controller: AbortController::new(),
            result,
            receiver,
            waiters: AtomicUsize::new(1),
            token,
        }
    }

    async fn stored(&self) -> ModelsRefreshResult {
        let mut receiver = self.receiver.clone();
        if let Some(value) = receiver.borrow().clone() {
            return value;
        }
        let _ = receiver.wait_for(Option::is_some).await;
        receiver.borrow().clone().unwrap_or_default()
    }
}

/// Coordinates refresh deduplication per runtime key.
#[derive(Default)]
pub struct ModelCatalogRefreshCoordinator {
    active: Mutex<HashMap<u64, Arc<ActiveRefresh>>>,
    next_token: AtomicU64,
}

impl ModelCatalogRefreshCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live waiters sharing the refresh for `runtime_key` (0 when idle).
    pub fn active_waiters(&self, runtime_key: u64) -> usize {
        self.lock()
            .get(&runtime_key)
            .map_or(0, |active| active.waiters.load(Ordering::SeqCst))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<ActiveRefresh>>> {
        self.active.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// senpi's `refresh`: the first caller on a runtime starts the operation, later callers join it,
    /// and the shared operation is aborted once the last waiter has gone.
    ///
    /// senpi starts the operation eagerly and races it against the shared controller, so a caller
    /// that cancels never blocks on the shared operation: it leaves the waiters set immediately
    /// while the operation keeps running for the remaining waiters. The port keeps that shape by
    /// driving the shared operation in its own task on the current runtime.
    pub async fn refresh<F, Fut>(
        self: &Arc<Self>,
        runtime_key: u64,
        signal: &AbortSignal,
        start: F,
    ) -> Result<ModelsRefreshResult, AbortReason>
    where
        F: FnOnce(AbortSignal) -> Fut + Send + 'static,
        Fut: Future<Output = ModelsRefreshResult> + Send + 'static,
    {
        signal.throw_if_aborted()?;
        let (active, leader) = self.acquire(runtime_key);
        if leader {
            let shared = active.controller.signal();
            let coordinator = Arc::clone(self);
            let operation = start(shared.clone());
            let active_for_task = Arc::clone(&active);
            tokio::spawn(async move {
                let outcome = race_with_abort_signal(operation, &shared)
                    .await
                    .unwrap_or_else(|_| ModelsRefreshResult { aborted: true, errors: Default::default() });
                active_for_task.result.send_replace(Some(outcome));
                coordinator.release(runtime_key, &active_for_task);
            });
        }
        let result = race_with_abort_signal(active.stored(), signal).await;
        self.decrement_waiters(runtime_key, &active);
        result
    }

    fn acquire(&self, runtime_key: u64) -> (Arc<ActiveRefresh>, bool) {
        let mut active = self.lock();
        if let Some(existing) = active.get(&runtime_key) {
            existing.waiters.fetch_add(1, Ordering::SeqCst);
            return (Arc::clone(existing), false);
        }
        let created = Arc::new(ActiveRefresh::new(self.next_token.fetch_add(1, Ordering::SeqCst)));
        active.insert(runtime_key, Arc::clone(&created));
        (created, true)
    }

    fn release(&self, runtime_key: u64, active: &Arc<ActiveRefresh>) {
        let mut map = self.lock();
        if map.get(&runtime_key).is_some_and(|current| current.token == active.token) {
            map.remove(&runtime_key);
        }
    }

    fn decrement_waiters(&self, runtime_key: u64, active: &Arc<ActiveRefresh>) {
        if active.waiters.fetch_sub(1, Ordering::SeqCst) == 1 {
            let map = self.lock();
            if map.get(&runtime_key).is_some_and(|current| current.token == active.token) {
                active.controller.abort(None);
            }
        }
    }
}

fn coordinator() -> &'static Arc<ModelCatalogRefreshCoordinator> {
    static COORDINATOR: OnceLock<Arc<ModelCatalogRefreshCoordinator>> = OnceLock::new();
    COORDINATOR.get_or_init(|| Arc::new(ModelCatalogRefreshCoordinator::new()))
}

/// senpi's module-level `refreshModelCatalogs(modelRuntime, signal)`.
pub async fn refresh_model_catalogs<F, Fut>(
    runtime_key: u64,
    signal: &AbortSignal,
    start: F,
) -> Result<ModelsRefreshResult, AbortReason>
where
    F: FnOnce(AbortSignal) -> Fut + Send + 'static,
    Fut: Future<Output = ModelsRefreshResult> + Send + 'static,
{
    coordinator().refresh(runtime_key, signal, start).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A deterministic two-way gate: the test opens it, the refresh closes it.
    fn gate() -> (tokio::sync::watch::Sender<bool>, tokio::sync::watch::Receiver<bool>) {
        tokio::sync::watch::channel(false)
    }

    async fn opened(mut receiver: tokio::sync::watch::Receiver<bool>) {
        let _ = receiver.wait_for(|open| *open).await;
    }

    async fn wait_for_waiters(coordinator: &ModelCatalogRefreshCoordinator, key: u64, count: usize) {
        for _ in 0..1_000_000 {
            if coordinator.active_waiters(key) >= count {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("waiters never reached {count}");
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_operation() {
        let coordinator = Arc::new(ModelCatalogRefreshCoordinator::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = gate();
        let (release_tx, release_rx) = gate();
        let signal = AbortController::new().signal();

        let make = {
            let calls = Arc::clone(&calls);
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            move |_shared: AbortSignal| {
                let calls = Arc::clone(&calls);
                let started_tx = started_tx.clone();
                let release_rx = release_rx.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    started_tx.send_replace(true);
                    let _ = release_rx.clone().wait_for(|open| *open).await;
                    ModelsRefreshResult::default()
                }
            }
        };

        let first = {
            let coordinator = Arc::clone(&coordinator);
            let signal = signal.clone();
            let make = make.clone();
            tokio::spawn(async move { coordinator.refresh(7, &signal, make).await })
        };
        opened(started_rx).await;
        let second = {
            let coordinator = Arc::clone(&coordinator);
            let signal = signal.clone();
            let make = make.clone();
            tokio::spawn(async move { coordinator.refresh(7, &signal, make).await })
        };
        wait_for_waiters(&coordinator, 7, 2).await;
        release_tx.send_replace(true);

        assert!(first.await.expect("join").is_ok());
        assert!(second.await.expect("join").is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn aborting_the_last_waiter_aborts_the_shared_refresh() {
        let coordinator = Arc::new(ModelCatalogRefreshCoordinator::new());
        let (started_tx, started_rx) = gate();
        let (release_tx, release_rx) = gate();
        let controller = AbortController::new();
        let signal = controller.signal();
        let shared_signal = Arc::new(Mutex::new(None::<AbortSignal>));

        // The operation runs until the test releases it; it captures the shared signal so the
        // assertion below proves the coordinator aborted it.
        let caller = {
            let coordinator = Arc::clone(&coordinator);
            let signal = signal.clone();
            let shared_signal = Arc::clone(&shared_signal);
            tokio::spawn(async move {
                coordinator
                    .refresh(3, &signal, move |shared: AbortSignal| {
                        *shared_signal.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            Some(shared);
                        let started_tx = started_tx.clone();
                        let release_rx = release_rx.clone();
                        async move {
                            started_tx.send_replace(true);
                            let _ = release_rx.clone().wait_for(|open| *open).await;
                            ModelsRefreshResult { aborted: false, errors: Default::default() }
                        }
                    })
                    .await
            })
        };
        opened(started_rx).await;

        // The only waiter cancels, so the zero-waiter branch aborts the shared operation.
        controller.abort(None);
        for _ in 0..1_000_000 {
            if coordinator.active_waiters(3) == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(coordinator.active_waiters(3), 0);
        let shared = shared_signal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("the operation observed the shared signal");
        assert!(shared.aborted(), "the shared refresh signal is aborted");

        release_tx.send_replace(true);
        // The caller's own signal aborted, so it sees the abort reason, not the stored result.
        let result = caller.await.expect("join");
        assert_eq!(result.expect_err("caller cancelled").name, "AbortError");
    }

    #[tokio::test]
    async fn one_callers_abort_does_not_cancel_another() {
        let coordinator = Arc::new(ModelCatalogRefreshCoordinator::new());
        let (started_tx, started_rx) = gate();
        let (release_tx, release_rx) = gate();
        let keep = AbortController::new();
        let drop_me = AbortController::new();

        let make = {
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            move |_shared: AbortSignal| {
                let started_tx = started_tx.clone();
                let release_rx = release_rx.clone();
                async move {
                    started_tx.send_replace(true);
                    let _ = release_rx.clone().wait_for(|open| *open).await;
                    ModelsRefreshResult::default()
                }
            }
        };

        let kept = {
            let coordinator = Arc::clone(&coordinator);
            let signal = keep.signal();
            let make = make.clone();
            tokio::spawn(async move { coordinator.refresh(9, &signal, make).await })
        };
        opened(started_rx).await;
        let dropped = {
            let coordinator = Arc::clone(&coordinator);
            let signal = drop_me.signal();
            let make = make.clone();
            tokio::spawn(async move { coordinator.refresh(9, &signal, make).await })
        };
        wait_for_waiters(&coordinator, 9, 2).await;
        drop_me.abort(None);
        assert_eq!(dropped.await.expect("join").unwrap_err().name, "AbortError");
        release_tx.send_replace(true);
        assert!(kept.await.expect("join").is_ok());
    }

    #[tokio::test]
    async fn an_aborted_caller_never_starts_an_operation() {
        let coordinator = Arc::new(ModelCatalogRefreshCoordinator::new());
        let controller = AbortController::new();
        controller.abort(None);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let result = coordinator
            .refresh(11, &controller.signal(), move |_shared: AbortSignal| {
                let observed = Arc::clone(&observed);
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    ModelsRefreshResult::default()
                }
            })
            .await;
        assert_eq!(result.unwrap_err().name, "AbortError");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
