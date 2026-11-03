//! Port of senpi mcp/modern/consent-queue.ts: one private form at a time per session binding, FIFO.
use super::errors::{McpMrtrError, McpMrtrErrorCode};
use maho_ext_api::AbortSignal;
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};

static QUEUES: std::sync::LazyLock<Mutex<HashMap<usize, Arc<tokio::sync::Mutex<()>>>>> =
    std::sync::LazyLock::new(Default::default);

/// `binding` is the identity of the session binding (upstream keys a `WeakMap` by the binding object).
/// A job whose signal aborts while queued is dropped with `MCP_MRTR_PROCESSING_UNAVAILABLE`; `run` starts
/// only once the job reaches the head of the queue. tokio's mutex is FIFO-fair, which is the queue order.
pub async fn queued_form<T, F, Fut>(
    binding: usize,
    signal: &AbortSignal,
    run: F,
) -> Result<T, McpMrtrError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, McpMrtrError>>,
{
    let queue = QUEUES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(binding)
        .or_default()
        .clone();
    struct Release {
        binding: usize,
        queue: Option<Arc<tokio::sync::Mutex<()>>>,
    }
    impl Drop for Release {
        fn drop(&mut self) {
            let mut queues = QUEUES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(self.queue.take());
            // The map holds one reference; nobody else waiting means the queue is empty.
            if queues
                .get(&self.binding)
                .is_some_and(|queue| Arc::strong_count(queue) == 1)
            {
                queues.remove(&self.binding);
            }
        }
    }
    let release = Release {
        binding,
        queue: Some(queue),
    };
    // Declared after `release`, so it drops first and `release` sees only the map's reference.
    let queue = release.queue.clone().expect("queue present until drop");
    let unavailable = || McpMrtrError::new(McpMrtrErrorCode::ProcessingUnavailable);
    if signal.is_aborted() {
        return Err(unavailable());
    }
    let turn = tokio::select! {
        biased;
        () = signal.cancelled() => return Err(unavailable()),
        guard = queue.lock() => guard,
    };
    if signal.is_aborted() {
        return Err(unavailable());
    }
    let result = run().await;
    drop(turn);
    drop(queue);
    drop(release);
    result
}
