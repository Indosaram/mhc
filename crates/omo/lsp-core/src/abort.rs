//! Minimal `AbortController`/`AbortSignal` equivalent used across the crate.

use crate::lsp::errors::LspError;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use tokio::sync::Notify;

type AbortListener = Box<dyn FnOnce(&LspError) + Send>;

#[derive(Default)]
struct State {
    reason: Option<LspError>,
    next_listener_id: u64,
    listeners: Vec<(u64, AbortListener)>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    notify: Notify,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("AbortSignal")
            .field("reason", &state.reason)
            .finish()
    }
}

/// Handle returned by [`AbortSignal::on_abort`]; pass it to [`AbortSignal::remove_listener`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbortListenerId(u64);

/// Cloneable, cheaply shared cancellation flag (TS `AbortSignal`).
#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    inner: Arc<Inner>,
}

impl AbortSignal {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn aborted(&self) -> bool {
        self.state().reason.is_some()
    }

    /// TS `signal.reason`: the error the signal was aborted with.
    pub fn reason(&self) -> Option<LspError> {
        self.state().reason.clone()
    }

    /// TS `signal.throwIfAborted()`.
    pub fn throw_if_aborted(&self) -> Result<(), LspError> {
        match self.reason() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// TS `addEventListener("abort", listener, { once: true })`. The listener runs
    /// synchronously inside `abort`; it is not called if the signal is already aborted.
    pub fn on_abort(&self, listener: impl FnOnce(&LspError) + Send + 'static) -> AbortListenerId {
        let mut state = self.state();
        let id = state.next_listener_id;
        state.next_listener_id += 1;
        state.listeners.push((id, Box::new(listener)));
        AbortListenerId(id)
    }

    /// TS `removeEventListener("abort", listener)`.
    pub fn remove_listener(&self, id: AbortListenerId) {
        self.state()
            .listeners
            .retain(|(listener_id, _)| *listener_id != id.0);
    }

    /// Resolves once the signal is aborted (immediately if it already is).
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.aborted() {
                return;
            }
            notified.await;
        }
    }

    fn abort_with(&self, reason: LspError) {
        let listeners = {
            let mut state = self.state();
            if state.reason.is_some() {
                return;
            }
            state.reason = Some(reason.clone());
            std::mem::take(&mut state.listeners)
        };
        self.inner.notify.notify_waiters();
        for (_, listener) in listeners {
            listener(&reason);
        }
    }
}

/// TS `AbortController`.
#[derive(Debug, Clone, Default)]
pub struct AbortController {
    signal: AbortSignal,
}

impl AbortController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    /// TS `controller.abort()`: the reason is an `AbortError`.
    pub fn abort(&self) {
        self.signal.abort_with(LspError::Aborted);
    }

    /// TS `controller.abort(reason)`. A second abort is ignored.
    pub fn abort_with(&self, reason: LspError) {
        self.signal.abort_with(reason);
    }
}
