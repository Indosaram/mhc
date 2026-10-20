//! Port of `src/context/index.ts`: immutable identity-keyed contexts.
use crate::types::{AbortSignal, BoxFuture, Context, ContextKey, ContextRef, ServiceError};
use std::{any::Any, marker::PhantomData, sync::{Arc, LazyLock, atomic::{AtomicU64, Ordering}}};
use tokio::sync::watch;

struct EmptyContext(&'static str);
impl Context for EmptyContext {
    fn abort_signal(&self) -> Option<Arc<dyn AbortSignal>> { None }
    fn value(&self, _: &Arc<()>) -> Option<Arc<dyn Any + Send + Sync>> { None }
    fn to_string(&self) -> String { self.0.into() }
}
struct ContextValue {
    parent: ContextRef,
    key: Arc<()>,
    description: Arc<str>,
    value: Arc<dyn Any + Send + Sync>,
}
impl Context for ContextValue {
    fn abort_signal(&self) -> Option<Arc<dyn AbortSignal>> {
        self.value(&ABORT_KEY.identity).and_then(|v| v.downcast_ref::<Option<Arc<dyn AbortSignal>>>().cloned()).flatten()
    }
    fn value(&self, key: &Arc<()>) -> Option<Arc<dyn Any + Send + Sync>> {
        if Arc::ptr_eq(key, &self.key) { Some(self.value.clone()) } else { self.parent.value(key) }
    }
    fn to_string(&self) -> String { format!("{}.WithValue({})", self.parent.to_string(), self.description) }
}
pub static BACKGROUND_CONTEXT: LazyLock<ContextRef> = LazyLock::new(|| Arc::new(EmptyContext("[Context BACKGROUND_CONTEXT]")));
pub static TODO_CONTEXT: LazyLock<ContextRef> = LazyLock::new(|| Arc::new(EmptyContext("[Context TODO_CONTEXT]")));
static ABORT_KEY: LazyLock<ContextKey<Option<Arc<dyn AbortSignal>>>> = LazyLock::new(|| create_context_key("chord.abortSignal"));
pub fn create_context_key<T>(description: &str) -> ContextKey<T> {
    ContextKey { token: description.into(), identity: Arc::new(()), value_type: PhantomData }
}
pub fn context_value<T: Clone + Send + Sync + 'static>(context: &ContextRef, key: &ContextKey<T>) -> Option<T> {
    context.value(&key.identity).and_then(|v| v.downcast_ref::<T>().cloned())
}
pub fn with_context_value<T: Send + Sync + 'static>(key: &ContextKey<T>, value: T, parent: &ContextRef) -> ContextRef {
    Arc::new(ContextValue { parent: parent.clone(), key: key.identity.clone(), description: key.token.clone(), value: Arc::new(value) })
}
struct CombinedSignal { parent: Arc<dyn AbortSignal>, signal: Arc<dyn AbortSignal>, initially_aborted: Option<bool> }
impl AbortSignal for CombinedSignal {
    fn aborted(&self) -> bool { self.parent.aborted() || self.signal.aborted() }
    fn cancellation_order(&self) -> Option<u64> {
        if let Some(parent) = self.initially_aborted { return if parent { self.parent.cancellation_order() } else { self.signal.cancellation_order() }; }
        match (self.parent.cancellation_order(), self.signal.cancellation_order()) {
            (Some(a), Some(b)) => Some(a.min(b)), (Some(a), None) => Some(a), (None, Some(b)) => Some(b), (None, None) => None,
        }
    }
    fn reason(&self) -> Option<Arc<dyn std::error::Error + Send + Sync>> {
        if let Some(parent) = self.initially_aborted { return if parent { self.parent.reason() } else { self.signal.reason() }; }
        match (self.parent.cancellation_order(), self.signal.cancellation_order()) {
            (Some(a), Some(b)) if b < a => self.signal.reason(),
            _ => { if self.parent.aborted() { self.parent.reason() } else { self.signal.reason() } }
        }
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { tokio::select! { () = self.parent.cancelled() => {}, () = self.signal.cancelled() => {} } })
    }
}
pub fn with_abort_signal(signal: Arc<dyn AbortSignal>, context: &ContextRef) -> ContextRef {
    let combined = match context.abort_signal() {
        None => signal,
        Some(parent) => {
            let initially_aborted = if parent.aborted() { Some(true) } else if signal.aborted() { Some(false) } else { None };
            Arc::new(CombinedSignal { parent, signal, initially_aborted })
        }
    };
    with_context_value(&ABORT_KEY, Some(combined), context)
}
pub fn without_abort_signal(context: &ContextRef) -> ContextRef { with_context_value(&ABORT_KEY, None, context) }
pub type CancellationReason = Arc<dyn std::error::Error + Send + Sync>;
static CANCELLATION_ORDER: AtomicU64 = AtomicU64::new(1);
struct Cancellation { reason: CancellationReason, order: u64 }
struct CancelSignal(watch::Sender<Option<Arc<Cancellation>>>);
impl AbortSignal for CancelSignal {
    fn aborted(&self) -> bool { self.0.borrow().is_some() }
    fn reason(&self) -> Option<CancellationReason> { self.0.borrow().as_ref().map(|c| c.reason.clone()) }
    fn cancellation_order(&self) -> Option<u64> { self.0.borrow().as_ref().map(|c| c.order) }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut receiver = self.0.subscribe();
            let closed = receiver.wait_for(Option::is_some).await.is_err();
            if closed { std::future::pending::<()>().await; }
        })
    }
}
#[derive(Clone)]
pub struct CancelHandle(Arc<CancelSignal>);
impl CancelHandle {
    pub fn cancel(&self) { self.cancel_with_reason(Arc::new(ContextCancelled)); }
    pub fn cancel_with_reason(&self, reason: CancellationReason) { self.0.0.send_if_modified(|current| { if current.is_some() { false } else { *current = Some(Arc::new(Cancellation { reason, order: CANCELLATION_ORDER.fetch_add(1, Ordering::SeqCst) })); true } }); }
}
pub fn with_cancel(context: &ContextRef) -> (ContextRef, CancelHandle) {
    let signal = Arc::new(CancelSignal(watch::channel(None).0));
    (with_abort_signal(signal.clone(), context), CancelHandle(signal))
}
#[derive(Debug, thiserror::Error)]
#[error("The operation was aborted")]
pub struct ContextCancelled;
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CancellationError(pub CancellationReason);
pub fn cancellation_error(signal: &dyn AbortSignal) -> ServiceError {
    signal.reason().map_or_else(|| Box::new(ContextCancelled) as ServiceError, |reason| Box::new(CancellationError(reason)))
}
/// Wait without owning the operation: cancelling or dropping this waiter never
/// drops the caller-owned task. The caller can still await its completion.
pub async fn await_with_context<T>(operation: &mut tokio::task::JoinHandle<T>, context: &ContextRef) -> Result<T, ServiceError> {
    match context.abort_signal() {
        None => Ok(operation.await?),
        Some(signal) => {
            if signal.aborted() { return Err(cancellation_error(signal.as_ref())); }
            tokio::select! { biased;
                () = signal.cancelled() => Err(cancellation_error(signal.as_ref())),
                result = operation => Ok(result?),
            }
        }
    }
}
