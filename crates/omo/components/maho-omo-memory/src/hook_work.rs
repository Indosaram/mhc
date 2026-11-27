//! Tracks in-flight memory hooks and asynchronous work so shutdown drain waits cleanly (#11135, `hook-work.ts`).

use std::future::Future;
use std::sync::Arc;

use memory_core::compile::InFlightReads;

/// Wraps an asynchronous hook, tool, or RPC body so `InFlightReads::settled()` waits for completion.
pub fn track_memory_work<T, Fut>(
    work: &Arc<InFlightReads>,
    task: impl FnOnce() -> Fut,
) -> impl Future<Output = T>
where
    Fut: Future<Output = T>,
{
    let work = Arc::clone(work);
    let fut = task();
    async move {
        let _guard = work.enter();
        fut.await
    }
}
