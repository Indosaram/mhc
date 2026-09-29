//! Port of senpi packages/ai/src/utils/abort.ts, plus the `AbortController`/`AbortSignal`
//! primitives the TS code takes from the platform.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortReason {
    pub name: String,
    pub message: String,
}

impl AbortReason {
    pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self { name: name.into(), message: message.into() }
    }

    /// The DOM default reason for `controller.abort()` without an argument.
    pub fn dom_default() -> Self {
        Self::new("AbortError", "This operation was aborted")
    }
}

impl std::fmt::Display for AbortReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AbortReason {}

type Listener = Box<dyn FnOnce(&AbortReason) + Send>;

#[derive(Default)]
struct SignalState {
    reason: Option<AbortReason>,
    listeners: Vec<(u64, Listener)>,
    next_id: u64,
}

struct SignalInner {
    state: Mutex<SignalState>,
    token: CancellationToken,
}

#[derive(Clone)]
pub struct AbortSignal {
    inner: Arc<SignalInner>,
}

impl std::fmt::Debug for AbortSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortSignal").field("aborted", &self.aborted()).finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerId(u64);

impl AbortSignal {
    fn new() -> Self {
        Self { inner: Arc::new(SignalInner { state: Mutex::new(SignalState::default()), token: CancellationToken::new() }) }
    }

    fn state(&self) -> MutexGuard<'_, SignalState> {
        self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn aborted(&self) -> bool {
        self.state().reason.is_some()
    }

    pub fn reason(&self) -> Option<AbortReason> {
        self.state().reason.clone()
    }

    /// `signal.throwIfAborted()`.
    pub fn throw_if_aborted(&self) -> Result<(), AbortReason> {
        match self.reason() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// `addEventListener("abort", listener, { once: true })`. Listeners added after abort never fire,
    /// matching the platform.
    pub fn add_abort_listener(&self, listener: impl FnOnce(&AbortReason) + Send + 'static) -> ListenerId {
        let mut state = self.state();
        let id = state.next_id;
        state.next_id += 1;
        if state.reason.is_none() {
            state.listeners.push((id, Box::new(listener)));
        }
        ListenerId(id)
    }

    pub fn remove_abort_listener(&self, id: ListenerId) {
        self.state().listeners.retain(|(listener_id, _)| *listener_id != id.0);
    }

    /// Resolves once the signal aborts.
    pub async fn cancelled(&self) {
        self.inner.token.cancelled().await;
    }

    fn abort_with(&self, reason: AbortReason) {
        let listeners = {
            let mut state = self.state();
            if state.reason.is_some() {
                return;
            }
            state.reason = Some(reason.clone());
            std::mem::take(&mut state.listeners)
        };
        self.inner.token.cancel();
        for (_, listener) in listeners {
            listener(&reason);
        }
    }
}

#[derive(Clone, Debug)]
pub struct AbortController {
    signal: AbortSignal,
}

impl Default for AbortController {
    fn default() -> Self {
        Self::new()
    }
}

impl AbortController {
    pub fn new() -> Self {
        Self { signal: AbortSignal::new() }
    }

    pub fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    pub fn abort(&self, reason: Option<AbortReason>) {
        self.signal.abort_with(reason.unwrap_or_else(AbortReason::dom_default));
    }
}

fn abort_reason(signal: &AbortSignal) -> AbortReason {
    signal.reason().unwrap_or_else(|| AbortReason::new("AbortError", "The operation was aborted"))
}

/// Create an operation-local signal for public APIs whose signal is optional.
pub fn operation_signal(signal: Option<AbortSignal>) -> AbortSignal {
    signal.unwrap_or_else(|| AbortController::new().signal())
}

/// Stop waiting for an operation when its signal aborts. The abandoned operation future is
/// dropped (Rust futures are inert when not polled), which subsumes the TS "keep observing the
/// abandoned promise" handling.
pub async fn race_with_abort_signal<T>(operation: impl Future<Output = T>, signal: &AbortSignal) -> Result<T, AbortReason> {
    if signal.aborted() {
        return Err(abort_reason(signal));
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(abort_reason(signal)),
        value = operation => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn race_resolves_with_operation_value() {
        let controller = AbortController::new();
        assert_eq!(race_with_abort_signal(async { 7 }, &controller.signal()).await, Ok(7));
    }

    #[tokio::test]
    async fn race_rejects_immediately_when_already_aborted() {
        let controller = AbortController::new();
        controller.abort(Some(AbortReason::new("Error", "stop")));
        let result = race_with_abort_signal(std::future::pending::<()>(), &controller.signal()).await;
        assert_eq!(result, Err(AbortReason::new("Error", "stop")));
    }

    #[tokio::test]
    async fn race_rejects_when_signal_aborts_mid_flight() {
        let controller = AbortController::new();
        let signal = controller.signal();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            race_with_abort_signal(async move { rx.await.ok() }, &signal).await
        });
        controller.abort(None);
        let result = task.await.expect("join");
        assert_eq!(result, Err(AbortReason::dom_default()));
        drop(tx);
    }

    #[test]
    fn listeners_fire_once_and_can_be_removed() {
        let controller = AbortController::new();
        let signal = controller.signal();
        let hits = Arc::new(AtomicUsize::new(0));
        let kept = hits.clone();
        signal.add_abort_listener(move |_| {
            kept.fetch_add(1, Ordering::SeqCst);
        });
        let removed = hits.clone();
        let id = signal.add_abort_listener(move |_| {
            removed.fetch_add(10, Ordering::SeqCst);
        });
        signal.remove_abort_listener(id);
        controller.abort(None);
        controller.abort(None);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(operation_signal(Some(signal)).aborted());
        assert!(!operation_signal(None).aborted());
    }
}
