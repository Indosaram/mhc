//! Port of senpi `packages/agent/src/harness/context.ts` plus the context primitive it
//! re-exports from `@earendil-works/chord` (`packages/chord/src/context/index.ts`).
//!
//! chord's `Context` is an immutable linked value map: deriving a child never mutates the parent,
//! and a key present in a child shadows the parent's entry for that key (including an explicit
//! `undefined`). The abort signal is one reserved key in that map, so `withoutAbortSignal` is a
//! plain value derivation rather than a special case.

use std::any::{Any, TypeId};
use std::future::Future;
use std::marker::PhantomData;
use std::sync::{Arc, LazyLock};

use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};

use crate::harness::telemetry::{TelemetryContext, noop_telemetry_context};

/// `ContextKey<T>`: an identity token for one context value slot.
pub struct ContextKey<T> {
    token: &'static str,
    type_id: TypeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T: 'static> ContextKey<T> {
    pub const fn new(token: &'static str) -> Self {
        Self { token, type_id: TypeId::of::<T>(), _marker: PhantomData }
    }

    /// The debug token (`key.token.description` in TS).
    pub fn token(&self) -> &'static str {
        self.token
    }
}

/// `createContextKey(description)`.
pub const fn create_context_key<T: 'static>(description: &'static str) -> ContextKey<T> {
    ContextKey::new(description)
}

/// The reserved abort-signal slot (`ABORT_SIGNAL_CONTEXT_KEY`).
pub static ABORT_SIGNAL_CONTEXT_KEY: ContextKey<Option<AbortSignal>> =
    create_context_key("chord.abortSignal");

type ValueCell = Arc<dyn Any + Send + Sync>;

enum ContextInner {
    Empty {
        name: &'static str,
    },
    Value {
        parent: Context,
        token: &'static str,
        type_id: TypeId,
        value: Option<ValueCell>,
    },
}

/// Immutable context value map. Cloning shares the parent chain.
#[derive(Clone)]
pub struct Context {
    inner: Arc<ContextInner>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_repr())
    }
}

impl Context {
    fn empty(name: &'static str) -> Self {
        Self { inner: Arc::new(ContextInner::Empty { name }) }
    }

    /// `context.value(key)`.
    pub fn value<T: Clone + Send + Sync + 'static>(&self, key: &ContextKey<T>) -> Option<T> {
        let mut current = self;
        loop {
            match current.inner.as_ref() {
                ContextInner::Empty { .. } => return None,
                ContextInner::Value { parent, token, type_id, value } => {
                    if *token == key.token && *type_id == key.type_id {
                        return value.as_ref().and_then(|cell| cell.downcast_ref::<T>()).cloned();
                    }
                    current = parent;
                }
            }
        }
    }

    /// `context.abortSignal`.
    pub fn abort_signal(&self) -> Option<AbortSignal> {
        self.value(&ABORT_SIGNAL_CONTEXT_KEY).flatten()
    }

    /// `context.abortSignal?.aborted`.
    pub fn is_aborted(&self) -> bool {
        self.abort_signal().is_some_and(|signal| signal.aborted())
    }

    fn to_string_repr(&self) -> String {
        match self.inner.as_ref() {
            ContextInner::Empty { name } => (*name).to_owned(),
            ContextInner::Value { parent, token, .. } => {
                format!("{}.WithValue({token})", parent.to_string_repr())
            }
        }
    }
}

impl std::fmt::Display for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_repr())
    }
}

/// `[Context BACKGROUND_CONTEXT]`.
pub static BACKGROUND_CONTEXT: LazyLock<Context> =
    LazyLock::new(|| Context::empty("[Context BACKGROUND_CONTEXT]"));

/// `[Context TODO_CONTEXT]`.
pub static TODO_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context::empty("[Context TODO_CONTEXT]"));

/// `BACKGROUND_CONTEXT` as a callable, mirroring the TS const import.
pub fn background_context() -> Context {
    BACKGROUND_CONTEXT.clone()
}

/// `TODO_CONTEXT` as a callable, mirroring the TS const import.
pub fn todo_context() -> Context {
    TODO_CONTEXT.clone()
}

/// `withContextValue(key, value, parent)`.
pub fn with_context_value<T: Clone + Send + Sync + 'static>(
    key: &ContextKey<T>,
    value: T,
    parent: &Context,
) -> Context {
    Context {
        inner: Arc::new(ContextInner::Value {
            parent: parent.clone(),
            token: key.token,
            type_id: key.type_id,
            value: Some(Arc::new(value) as ValueCell),
        }),
    }
}

/// Combine two signals into one that aborts with whichever fires first (`AbortSignal.any`).
pub fn combine_abort_signals(first: &AbortSignal, second: &AbortSignal) -> AbortSignal {
    let controller = AbortController::new();
    let combined = controller.signal();
    if first.aborted() {
        controller.abort(first.reason());
        return combined;
    }
    if second.aborted() {
        controller.abort(second.reason());
        return combined;
    }
    let watchers: Vec<AbortSignal> = vec![first.clone(), second.clone()];
    let aborting = controller.clone();
    tokio::spawn(async move {
        let first = watchers[0].clone();
        let second = watchers[1].clone();
        let reason = tokio::select! {
            biased;
            () = first.cancelled() => first.reason(),
            () = second.cancelled() => second.reason(),
        };
        if !aborting.signal().aborted() {
            aborting.abort(reason);
        }
    });
    combined
}

/// `withAbortSignal(signal, context)`: derive a context cancelled by either signal.
pub fn with_abort_signal(signal: AbortSignal, context: &Context) -> Context {
    let parent = context.abort_signal();
    let combined = match parent {
        None => signal,
        Some(parent) => combine_abort_signals(&parent, &signal),
    };
    with_context_value(&ABORT_SIGNAL_CONTEXT_KEY, Some(combined), context)
}

/// `withoutAbortSignal(context)`: retain every value except caller cancellation.
pub fn without_abort_signal(context: &Context) -> Context {
    with_context_value(&ABORT_SIGNAL_CONTEXT_KEY, None, context)
}

/// Handle returned by [`with_cancel`].
#[derive(Clone)]
pub struct CancelHandle {
    controller: AbortController,
}

impl CancelHandle {
    /// `cancel(reason)`.
    pub fn cancel(&self, reason: Option<AbortReason>) {
        self.controller.abort(reason);
    }
}

/// `withCancel(context)`: an independently cancellable child context.
pub fn with_cancel(context: &Context) -> (Context, CancelHandle) {
    let controller = AbortController::new();
    let derived = with_abort_signal(controller.signal(), context);
    (derived, CancelHandle { controller })
}

/// `awaitWithContext(promise, context)`: stop waiting when the context aborts.
///
/// TS keeps observing the abandoned promise; Rust drops the unpolled future, which subsumes the
/// observable behavior (see `maho_ai::utils::abort::race_with_abort_signal`).
pub async fn await_with_context<T>(
    operation: impl Future<Output = T>,
    context: &Context,
) -> Result<T, AbortReason> {
    match context.abort_signal() {
        None => Ok(operation.await),
        Some(signal) => maho_ai::utils::abort::race_with_abort_signal(operation, &signal).await,
    }
}

static TELEMETRY_CONTEXT_KEY: ContextKey<TelemetryContext> = create_context_key("pi.telemetryContext");

/// `getTelemetryContext(context)`: the telemetry parent attached to a context, or the shared no-op.
pub fn get_telemetry_context(context: &Context) -> TelemetryContext {
    context.value(&TELEMETRY_CONTEXT_KEY).unwrap_or_else(noop_telemetry_context)
}

/// `withTelemetryContext(telemetryContext, context)`.
pub fn with_telemetry_context(telemetry_context: TelemetryContext, context: &Context) -> Context {
    with_context_value(&TELEMETRY_CONTEXT_KEY, telemetry_context, context)
}
