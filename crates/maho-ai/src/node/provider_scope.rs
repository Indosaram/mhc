//! Port of senpi packages/ai/src/node/provider-scope.ts.
//!
//! `AsyncLocalStorage` maps to a tokio task-local for async work plus a thread-local for
//! synchronous callers, so a scope follows `.await` points the way the TS store does.

use crate::api_registry::RegisteredApiProvider;
use crate::images_api_registry::RegisteredImagesApiProvider;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeState {
    Active,
    Closed,
}

#[derive(Default)]
struct ScopeInner {
    closed: bool,
    overlay: HashMap<String, RegisteredApiProvider>,
    images_overlay: HashMap<String, RegisteredImagesApiProvider>,
}

#[derive(Clone, Default)]
pub struct ProviderScope {
    inner: Arc<Mutex<ScopeInner>>,
}

impl std::fmt::Debug for ProviderScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderScope").field("state", &self.state()).finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderScopeError {
    #[error("Provider scope is closed")]
    Closed,
    #[error("Provider scope is required in strict mode")]
    StrictModeRequiresScope,
    #[error("No active provider scope to bind")]
    NoActiveScope,
}

impl ProviderScope {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, ScopeInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn state(&self) -> ScopeState {
        if self.lock().closed { ScopeState::Closed } else { ScopeState::Active }
    }

    pub fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.overlay.clear();
        inner.images_overlay.clear();
    }

    pub(crate) fn with_overlay<T>(&self, f: impl FnOnce(&mut HashMap<String, RegisteredApiProvider>) -> T) -> T {
        f(&mut self.lock().overlay)
    }

    pub(crate) fn with_images_overlay<T>(
        &self,
        f: impl FnOnce(&mut HashMap<String, RegisteredImagesApiProvider>) -> T,
    ) -> T {
        f(&mut self.lock().images_overlay)
    }

    pub fn ptr_eq(&self, other: &ProviderScope) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

tokio::task_local! {
    static TASK_SCOPE: ProviderScope;
}

thread_local! {
    static SYNC_SCOPE: RefCell<Vec<ProviderScope>> = const { RefCell::new(Vec::new()) };
}

static STRICT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_provider_scope_strict_mode(enabled: bool) {
    STRICT_MODE.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn strict_mode() -> bool {
    STRICT_MODE.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn active_provider_scope() -> Option<ProviderScope> {
    SYNC_SCOPE
        .with(|stack| stack.borrow().last().cloned())
        .or_else(|| TASK_SCOPE.try_with(Clone::clone).ok())
}

/// The scope registries read, validated like `getActiveProviderScope` in the TS registries.
pub(crate) fn checked_active_scope() -> Result<Option<ProviderScope>, ProviderScopeError> {
    let scope = active_provider_scope();
    match &scope {
        Some(scope) if scope.state() == ScopeState::Closed => Err(ProviderScopeError::Closed),
        None if strict_mode() => Err(ProviderScopeError::StrictModeRequiresScope),
        _ => Ok(scope),
    }
}

pub fn run_with_provider_scope<T>(scope: &ProviderScope, f: impl FnOnce() -> T) -> Result<T, ProviderScopeError> {
    if scope.state() == ScopeState::Closed {
        return Err(ProviderScopeError::Closed);
    }
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            SYNC_SCOPE.with(|stack| stack.borrow_mut().pop());
        }
    }
    SYNC_SCOPE.with(|stack| stack.borrow_mut().push(scope.clone()));
    let _pop = Pop;
    Ok(f())
}

pub async fn run_with_provider_scope_async<F: Future>(scope: &ProviderScope, future: F) -> Result<F::Output, ProviderScopeError> {
    if scope.state() == ScopeState::Closed {
        return Err(ProviderScopeError::Closed);
    }
    Ok(TASK_SCOPE.scope(scope.clone(), future).await)
}

/// Captures the active scope so `f` later runs inside it.
pub fn bind_to_provider_scope<T>(
    f: impl Fn() -> T + Send + Sync + 'static,
) -> Result<impl Fn() -> Result<T, ProviderScopeError>, ProviderScopeError> {
    let scope = active_provider_scope().ok_or(ProviderScopeError::NoActiveScope)?;
    Ok(move || run_with_provider_scope(&scope, &f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_scope_nesting_and_closed_rejection() {
        assert!(active_provider_scope().is_none());
        let outer = ProviderScope::new();
        let inner = ProviderScope::new();
        run_with_provider_scope(&outer, || {
            assert!(active_provider_scope().is_some_and(|s| s.ptr_eq(&outer)));
            run_with_provider_scope(&inner, || {
                assert!(active_provider_scope().is_some_and(|s| s.ptr_eq(&inner)));
            })
            .expect("inner");
            assert!(active_provider_scope().is_some_and(|s| s.ptr_eq(&outer)));
        })
        .expect("outer");
        assert!(active_provider_scope().is_none());
        inner.close();
        assert_eq!(run_with_provider_scope(&inner, || ()), Err(ProviderScopeError::Closed));
        assert!(bind_to_provider_scope(|| 1).is_err());
    }

    #[tokio::test]
    async fn async_scope_survives_await_and_bind_reenters() {
        let scope = ProviderScope::new();
        let seen = run_with_provider_scope_async(&scope, async {
            tokio::task::yield_now().await;
            let bound = bind_to_provider_scope(|| active_provider_scope().is_some()).expect("bind");
            (active_provider_scope().is_some_and(|s| s.ptr_eq(&scope)), bound)
        })
        .await
        .expect("scope");
        assert!(seen.0);
        assert_eq!((seen.1)(), Ok(true));
    }
}
