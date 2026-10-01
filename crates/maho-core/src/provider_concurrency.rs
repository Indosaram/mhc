//! Port of senpi packages/coding-agent/src/core/provider-concurrency.ts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// Limit provider requests, not agent turns: tool execution must not retain a slot.
pub struct ProviderSemaphores {
    providers: Arc<Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>>,
    get_limit: Arc<dyn Fn(&str) -> i64 + Send + Sync>,
}

impl ProviderSemaphores {
    pub fn new(get_limit: impl Fn(&str) -> i64 + Send + Sync + 'static) -> Self {
        Self { providers: Arc::new(Mutex::new(HashMap::new())), get_limit: Arc::new(get_limit) }
    }

    fn semaphore_for(&self, provider_id: &str, limit: usize) -> Arc<tokio::sync::Semaphore> {
        let mut providers = self.providers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(providers.entry(provider_id.to_owned()).or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(limit))))
    }

    /// Whether a configured limit applies at all; a limit of zero means unlimited.
    pub fn limit_for(&self, provider_id: &str) -> Option<usize> {
        let configured = (self.get_limit)(provider_id);
        if configured > 0 { Some(configured as usize) } else { None }
    }

    /// Runs one provider request under the provider's semaphore. A limit of zero runs unbracketed.
    pub async fn bracket<T>(
        &self,
        provider_id: &str,
        run: impl std::future::Future<Output = T>,
    ) -> T {
        match self.limit_for(provider_id) {
            None => run.await,
            Some(limit) => {
                let semaphore = self.semaphore_for(provider_id, limit);
                let _permit = semaphore.acquire_owned().await.expect("provider semaphore is never closed");
                run.await
            }
        }
    }
}

/// A grant that releases its slot on drop.
pub struct ProviderSlot {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Acquires a slot, or resolves to None when the caller's cancellation signal fires first.
pub async fn acquire_or_abort(
    semaphore: Arc<tokio::sync::Semaphore>,
    mut aborted: oneshot::Receiver<()>,
) -> Option<ProviderSlot> {
    tokio::select! {
        permit = semaphore.acquire_owned() => permit.ok().map(|permit| ProviderSlot { _permit: permit }),
        _ = &mut aborted => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn a_zero_limit_means_unlimited() {
        let semaphores = ProviderSemaphores::new(|_| 0);
        assert!(semaphores.limit_for("p").is_none());
        let semaphores = ProviderSemaphores::new(|_| 3);
        assert_eq!(semaphores.limit_for("p"), Some(3));
    }

    #[tokio::test]
    async fn bracketing_runs_the_request() {
        let semaphores = ProviderSemaphores::new(|_| 2);
        let value = semaphores.bracket("p", async { 7 }).await;
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn an_unbracketed_request_still_runs() {
        let semaphores = ProviderSemaphores::new(|_| 0);
        assert_eq!(semaphores.bracket("p", async { "ok" }).await, "ok");
    }

    #[tokio::test]
    async fn the_limit_bounds_concurrent_requests() {
        let active = Arc::new(AtomicI64::new(0));
        let peak = Arc::new(AtomicI64::new(0));
        let semaphores = Arc::new(ProviderSemaphores::new(|_| 2));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let semaphores = Arc::clone(&semaphores);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                semaphores
                    .bracket("p", async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await;
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }

    #[tokio::test]
    async fn an_aborted_acquisition_yields_no_slot() {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let _held = semaphore.clone().acquire_owned().await.expect("permit");
        let (tx, rx) = oneshot::channel();
        tx.send(()).expect("send");
        assert!(acquire_or_abort(semaphore, rx).await.is_none());
    }
}
