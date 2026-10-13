//! Port of senpi packages/coding-agent/src/core/provider-concurrency.ts.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use maho_ai::utils::abort::{AbortReason, AbortSignal};
use tokio::sync::oneshot;

struct Waiter {
    id: u64,
    grant: oneshot::Sender<()>,
}

struct ProviderSemaphore {
    /// `None` = unlimited (senpi `Infinity`).
    limit: Option<usize>,
    active: usize,
    waiters: VecDeque<Waiter>,
    next_waiter: u64,
}

type Semaphores = Arc<Mutex<HashMap<String, ProviderSemaphore>>>;

fn lock(providers: &Semaphores) -> std::sync::MutexGuard<'_, HashMap<String, ProviderSemaphore>> {
    providers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn drain(semaphore: &mut ProviderSemaphore) {
    while semaphore.limit.is_none_or(|limit| semaphore.active < limit) {
        let Some(waiter) = semaphore.waiters.pop_front() else { break };
        // A waiter whose receiver is gone (aborted) never takes the slot.
        if waiter.grant.send(()).is_ok() {
            semaphore.active += 1;
        }
    }
}

fn normalize_limit(limit: i64) -> Option<usize> {
    if limit > 0 { usize::try_from(limit).ok() } else { None }
}

/// A granted provider slot; dropping it releases the slot and admits the next waiter.
pub struct ProviderPermit {
    providers: Semaphores,
    provider_id: String,
}

impl Drop for ProviderPermit {
    fn drop(&mut self) {
        let mut providers = lock(&self.providers);
        if let Some(semaphore) = providers.get_mut(&self.provider_id) {
            semaphore.active = semaphore.active.saturating_sub(1);
            drain(semaphore);
        }
    }
}

/// Limit provider requests, not agent turns: tool execution must not retain a slot.
pub struct ProviderSemaphores {
    providers: Semaphores,
    get_limit: Arc<dyn Fn(&str) -> i64 + Send + Sync>,
}

impl ProviderSemaphores {
    pub fn new(get_limit: impl Fn(&str) -> i64 + Send + Sync + 'static) -> Self {
        Self { providers: Arc::new(Mutex::new(HashMap::new())), get_limit: Arc::new(get_limit) }
    }

    /// senpi `resize`: apply a new limit to an existing semaphore and admit waiters it now fits.
    pub fn resize(&self, provider_id: &str, limit: i64) {
        let mut providers = lock(&self.providers);
        let Some(semaphore) = providers.get_mut(provider_id) else { return };
        semaphore.limit = normalize_limit(limit);
        drain(semaphore);
    }

    /// senpi `setSettingsManager`'s resize pass: re-read the limit of every existing semaphore.
    pub fn resize_all(&self) {
        let ids: Vec<String> = lock(&self.providers).keys().cloned().collect();
        for id in ids { self.resize(&id, (self.get_limit)(&id)); }
    }

    async fn acquire(&self, provider_id: &str, signal: Option<&AbortSignal>) -> Result<ProviderPermit, AbortReason> {
        if let Some(signal) = signal {
            signal.throw_if_aborted()?;
        }
        let (grant, mut granted) = oneshot::channel();
        let waiter_id = {
            let mut providers = lock(&self.providers);
            let Some(semaphore) = providers.get_mut(provider_id) else {
                return Ok(ProviderPermit { providers: self.providers.clone(), provider_id: provider_id.to_owned() });
            };
            let id = semaphore.next_waiter;
            semaphore.next_waiter += 1;
            semaphore.waiters.push_back(Waiter { id, grant });
            drain(semaphore);
            id
        };
        let permit = || ProviderPermit { providers: self.providers.clone(), provider_id: provider_id.to_owned() };
        let outcome = match signal {
            Some(signal) => maho_ai::utils::abort::race_with_abort_signal(&mut granted, signal).await,
            None => Ok((&mut granted).await),
        };
        match outcome {
            Ok(Ok(())) => Ok(permit()),
            Ok(Err(_)) => Err(AbortReason::dom_default()),
            Err(reason) => {
                // The receiver is still alive, so under the lock the waiter is either still queued
                // (no slot taken) or was granted (the slot must be released by dropping the permit).
                let mut providers = lock(&self.providers);
                if let Some(semaphore) = providers.get_mut(provider_id) {
                    semaphore.waiters.retain(|waiter| waiter.id != waiter_id);
                }
                let was_granted = granted.try_recv().is_ok();
                drop(providers);
                if was_granted { drop(permit()); }
                Err(reason)
            }
        }
    }

    /// senpi `bracket`: run one provider request under the provider's limit. The returned permit is
    /// held until the request's stream settles; `None` means the provider is unlimited.
    pub async fn bracket<T>(
        &self,
        provider_id: &str,
        signal: Option<&AbortSignal>,
        run: impl FnOnce() -> T,
    ) -> Result<(T, Option<ProviderPermit>), AbortReason> {
        let limit = normalize_limit((self.get_limit)(provider_id));
        self.resize(provider_id, (self.get_limit)(provider_id));
        let Some(limit) = limit else { return Ok((run(), None)) };
        lock(&self.providers).entry(provider_id.to_owned()).or_insert_with(|| ProviderSemaphore {
            limit: Some(limit), active: 0, waiters: VecDeque::new(), next_waiter: 0,
        });
        let permit = self.acquire(provider_id, signal).await?;
        if let Some(signal) = signal {
            signal.throw_if_aborted()?;
        }
        Ok((run(), Some(permit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[tokio::test]
    async fn a_changed_limit_applies_to_new_acquisitions() {
        let limit = Arc::new(AtomicI64::new(1));
        let source = Arc::clone(&limit);
        let semaphores = ProviderSemaphores::new(move |_| source.load(Ordering::SeqCst));
        let (_, _held) = semaphores.bracket("p", None, || ()).await.expect("first slot");
        limit.store(2, Ordering::SeqCst);
        let (_, second) = tokio::time::timeout(std::time::Duration::from_secs(5), semaphores.bracket("p", None, || ())).await.expect("resized slot").expect("bracket");
        assert!(second.is_some());
        limit.store(0, Ordering::SeqCst);
        let (_, unlimited) = semaphores.bracket("p", None, || ()).await.expect("unlimited");
        assert!(unlimited.is_none());
    }

    #[tokio::test]
    async fn a_zero_limit_runs_unbracketed() {
        let semaphores = ProviderSemaphores::new(|_| 0);
        let (value, permit) = semaphores.bracket("p", None, || 7).await.expect("bracket");
        assert_eq!(value, 7);
        assert!(permit.is_none());
    }

    #[tokio::test]
    async fn the_limit_bounds_held_permits_and_release_admits_the_next() {
        let semaphores = Arc::new(ProviderSemaphores::new(|_| 1));
        let (_, first) = semaphores.bracket("p", None, || ()).await.expect("first");
        let waiting = {
            let semaphores = semaphores.clone();
            tokio::spawn(async move { semaphores.bracket("p", None, || ()).await.map(|(_, permit)| permit.is_some()) })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(first);
        assert!(waiting.await.expect("join").expect("second"));
    }

    #[tokio::test]
    async fn resizing_up_admits_waiters() {
        let limit = Arc::new(AtomicI64::new(1));
        let reader = limit.clone();
        let semaphores = Arc::new(ProviderSemaphores::new(move |_| reader.load(Ordering::SeqCst)));
        let (_, _held) = semaphores.bracket("p", None, || ()).await.expect("first");
        let (granted_tx, granted_rx) = oneshot::channel();
        let waiter = {
            let semaphores = semaphores.clone();
            tokio::spawn(async move {
                let (_, permit) = semaphores.acquire_for_test("p").await;
                let _ = granted_tx.send(());
                permit
            })
        };
        tokio::task::yield_now().await;
        limit.store(2, Ordering::SeqCst);
        semaphores.resize("p", 2);
        tokio::time::timeout(std::time::Duration::from_secs(5), granted_rx).await.expect("granted in time").expect("grant");
        drop(waiter.await.expect("join"));
    }

    #[tokio::test]
    async fn an_aborted_wait_takes_no_slot() {
        let semaphores = ProviderSemaphores::new(|_| 1);
        let (_, held) = semaphores.bracket("p", None, || ()).await.expect("first");
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        assert!(semaphores.bracket("p", Some(&controller.signal()), || ()).await.is_err());
        drop(held);
        let (_, next) = semaphores.bracket("p", None, || ()).await.expect("slot freed");
        assert!(next.is_some());
    }

    impl ProviderSemaphores {
        async fn acquire_for_test(&self, provider_id: &str) -> ((), Option<ProviderPermit>) {
            self.bracket(provider_id, None, || ()).await.expect("acquire")
        }
    }
}
