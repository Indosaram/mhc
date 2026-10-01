//! Port of senpi `packages/agent/src/harness/execution/effect-gate.ts`.

use std::sync::{Arc, Mutex};

use maho_ai::utils::abort::{AbortController, AbortSignal};

#[derive(Clone, Default)]
pub struct Cancellation {
    inner: Arc<CancellationInner>,
}

#[derive(Default)]
struct CancellationInner {
    resolved: Mutex<bool>,
    notify: tokio::sync::Notify,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve(&self) {
        {
            let mut resolved = self.inner.resolved.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *resolved = true;
        }
        self.inner.notify.notify_waiters();
    }

    pub fn is_resolved(&self) -> bool {
        *self.inner.resolved.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub async fn wait(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_resolved() {
                return;
            }
            notified.await;
            if self.is_resolved() {
                return;
            }
        }
    }
}

impl PartialEq for Cancellation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Cancellation {}

impl std::fmt::Debug for Cancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cancellation").field("resolved", &self.is_resolved()).finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortRequested {
    pub cancellation: Cancellation,
}

impl AbortRequested {
    pub fn new(cancellation: Cancellation) -> Self {
        Self { cancellation }
    }
}

impl std::fmt::Display for AbortRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Abort requested")
    }
}

impl std::error::Error for AbortRequested {}

#[derive(Debug, Clone)]
pub enum GateRefusal {
    Abort(AbortRequested),
    Closed(String),
}

impl std::fmt::Display for GateRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Abort(error) => write!(f, "{error}"),
            Self::Closed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GateRefusal {}

enum GateState {
    Open,
    Aborting(Cancellation),
    Closed(String),
}

struct GateInner {
    state: Mutex<GateState>,
    controller: AbortController,
}

#[derive(Clone)]
pub struct Gate {
    inner: Arc<GateInner>,
}

pub struct GateControl {
    inner: Arc<GateInner>,
}

impl Gate {
    pub fn admit<T>(&self, invoke: impl FnOnce() -> T) -> Result<T, GateRefusal> {
        self.check()?;
        Ok(invoke())
    }

    pub fn signal(&self) -> AbortSignal {
        self.inner.controller.signal()
    }

    fn check(&self) -> Result<(), GateRefusal> {
        let state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*state {
            GateState::Open => Ok(()),
            GateState::Aborting(cancellation) => {
                Err(GateRefusal::Abort(AbortRequested::new(cancellation.clone())))
            }
            GateState::Closed(message) => Err(GateRefusal::Closed(message.clone())),
        }
    }
}

impl GateControl {
    pub fn begin_abort(&self, cancellation: Cancellation) {
        let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(*state, GateState::Open) {
            *state = GateState::Aborting(cancellation);
        }
    }

    pub fn signal_abort(&self) {
        let cancellation = {
            let state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match &*state {
                GateState::Aborting(cancellation) => Some(cancellation.clone()),
                _ => None,
            }
        };
        let Some(cancellation) = cancellation else {
            return;
        };
        let signal = self.inner.controller.signal();
        if signal.aborted() {
            return;
        }
        self.inner
            .controller
            .abort(Some(maho_ai::utils::abort::AbortReason::new("AbortRequested", "Abort requested")));
        let _ = cancellation;
    }

    pub fn close(&self, error: impl Into<String>) {
        let message = error.into();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if matches!(*state, GateState::Closed(_)) {
                return;
            }
            *state = GateState::Closed(message.clone());
        }
        if !self.inner.controller.signal().aborted() {
            self.inner
                .controller
                .abort(Some(maho_ai::utils::abort::AbortReason::new("Error", message)));
        }
    }
}

pub fn create_gate() -> (Gate, GateControl) {
    let inner = Arc::new(GateInner { state: Mutex::new(GateState::Open), controller: AbortController::new() });
    (Gate { inner: inner.clone() }, GateControl { inner })
}
