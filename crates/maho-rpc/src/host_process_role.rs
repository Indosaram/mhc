//! Whether this process is running as a host generation (senpi `rpc/host-process-role.ts`).
use std::sync::atomic::{AtomicUsize, Ordering};

static HOST_GENERATION_SCOPES: AtomicUsize = AtomicUsize::new(0);

pub fn is_host_generation_process() -> bool {
    HOST_GENERATION_SCOPES.load(Ordering::SeqCst) > 0
}

struct ScopeGuard;

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        HOST_GENERATION_SCOPES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs `action` with the host-generation role set; the scope ends however the future ends (completion,
/// error or drop), matching senpi's `try/finally`.
pub async fn run_as_host_generation_process<T, F>(action: F) -> T
where
    F: std::future::Future<Output = T>,
{
    HOST_GENERATION_SCOPES.fetch_add(1, Ordering::SeqCst);
    let _guard = ScopeGuard;
    action.await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_role_holds_only_inside_the_scope() {
        let inside = run_as_host_generation_process(async { is_host_generation_process() }).await;
        assert!(inside);
        assert!(!is_host_generation_process());
    }
}
