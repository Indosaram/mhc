//! Port of senpi `packages/agent/src/harness/context.ts` plus the context primitive it
//! re-exports from `@earendil-works/chord` (`packages/chord/src/context/index.ts`).
//!
//! chord's `Context` is an immutable linked value map: deriving a child never mutates the parent,
//! and a key present in a child shadows the parent's entry for that key (including an explicit
//! `undefined`). The abort signal is one reserved key in that map, so `withoutAbortSignal` is a
//! plain value derivation rather than a special case.

use std::future::Future;
use std::sync::{Arc, LazyLock};

use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};

use crate::harness::telemetry::{TelemetryContext, noop_telemetry_context};

pub use maho_chord::{context::create_context_key, types::ContextKey};

/// The reserved abort-signal slot (`ABORT_SIGNAL_CONTEXT_KEY`).
pub static ABORT_SIGNAL_CONTEXT_KEY: LazyLock<ContextKey<Option<AbortSignal>>> =
    LazyLock::new(|| create_context_key("chord.abortSignal"));

/// Immutable context value map. Cloning shares the parent chain.
#[derive(Clone)]
pub struct Context {
    inner: maho_chord::types::ContextRef,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_repr())
    }
}

impl Context {
    pub fn as_chord(&self) -> maho_chord::types::ContextRef {
        self.inner.clone()
    }

    /// `context.value(key)`.
    pub fn value<T: Clone + Send + Sync + 'static>(&self, key: &ContextKey<T>) -> Option<T> {
        maho_chord::context::context_value(&self.inner, key)
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
        self.inner.to_string()
    }
}

impl std::fmt::Display for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_repr())
    }
}

/// `[Context BACKGROUND_CONTEXT]`.
pub static BACKGROUND_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context {
    inner: maho_chord::context::BACKGROUND_CONTEXT.clone(),
});

/// `[Context TODO_CONTEXT]`.
pub static TODO_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context {
    inner: maho_chord::context::TODO_CONTEXT.clone(),
});

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
        inner: maho_chord::context::with_context_value(key, value, &parent.inner),
    }
}

/// Combine two signals into one that aborts with whichever fires first (`AbortSignal.any`).
pub fn combine_abort_signals(first: &AbortSignal, second: &AbortSignal) -> AbortSignal {
    let controller = AbortController::new();
    let combined = controller.signal();
    if let Some((source, order)) = [first, second].into_iter()
        .filter_map(|signal| signal.cancellation_order().map(|order| (signal, order)))
        .min_by_key(|(_, order)| *order)
    {
        controller.abort_arc_with_order(source.reason_arc(), Some(order));
        return combined;
    }
    let settle = {
        let first = first.downgrade();
        let second = second.downgrade();
        move || {
            let source = [first.upgrade(), second.upgrade()]
                .into_iter().flatten()
                .filter_map(|signal| signal.cancellation_order().map(|order| (signal, order)))
                .min_by_key(|(_, order)| *order);
            if let Some((source, order)) = source {
                controller.abort_arc_with_order(source.reason_arc(), Some(order));
            }
        }
    };
    let on_first = settle.clone();
    first.add_abort_listener(move |_| on_first());
    let on_second = settle.clone();
    second.add_abort_listener(move |_| on_second());
    settle();
    combined
}

/// `withAbortSignal(signal, context)`: derive a context cancelled by either signal.
pub fn with_abort_signal(signal: AbortSignal, context: &Context) -> Context {
    let parent = context.abort_signal();
    let combined = match parent {
        None => signal,
        Some(parent) => combine_abort_signals(&parent, &signal),
    };
    let derived = with_context_value(&ABORT_SIGNAL_CONTEXT_KEY, Some(combined.clone()), context);
    Context {
        inner: maho_chord::context::with_abort_signal(
            Arc::new(ChordAbortSignal(combined)),
            &derived.inner,
        ),
    }
}

struct ChordAbortSignal(AbortSignal);

impl maho_chord::types::AbortSignal for ChordAbortSignal {
    fn aborted(&self) -> bool {
        self.0.aborted()
    }

    fn reason(&self) -> Option<Arc<dyn std::error::Error + Send + Sync>> {
        self.0.reason_arc().map(|reason| reason as Arc<dyn std::error::Error + Send + Sync>)
    }

    fn cancellation_order(&self) -> Option<u64> {
        self.0.cancellation_order()
    }

    fn cancelled(&self) -> maho_chord::types::BoxFuture<'_, ()> {
        Box::pin(self.0.cancelled())
    }
}

/// `withoutAbortSignal(context)`: retain every value except caller cancellation.
pub fn without_abort_signal(context: &Context) -> Context {
    let derived = with_context_value(&ABORT_SIGNAL_CONTEXT_KEY, None, context);
    Context {
        inner: maho_chord::context::without_abort_signal(&derived.inner),
    }
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

static TELEMETRY_CONTEXT_KEY: LazyLock<ContextKey<TelemetryContext>> =
    LazyLock::new(|| create_context_key("pi.telemetryContext"));

/// `getTelemetryContext(context)`: the telemetry parent attached to a context, or the shared no-op.
pub fn get_telemetry_context(context: &Context) -> TelemetryContext {
    context
        .value(&TELEMETRY_CONTEXT_KEY)
        .unwrap_or_else(noop_telemetry_context)
}

/// `telemetryContext: getTelemetryContext(context)` as carried on provider request options.
pub fn provider_telemetry_context(
    context: &Context,
) -> Option<maho_ai::types::ProviderTelemetryContext> {
    Some(Arc::new(get_telemetry_context(context)))
}

/// `withTelemetryContext(telemetryContext, context)`.
pub fn with_telemetry_context(telemetry_context: TelemetryContext, context: &Context) -> Context {
    with_context_value(&TELEMETRY_CONTEXT_KEY, telemetry_context, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_description_keys_do_not_read_or_shadow_each_other() {
        // Given two independently created keys of the same type and description.
        let first = create_context_key::<String>("same");
        let second = create_context_key::<String>("same");
        let parent = with_context_value(&first, "first".into(), &BACKGROUND_CONTEXT);
        assert_eq!(parent.value(&second), None);

        // When the second key is installed in a child.
        let child = with_context_value(&second, "second".into(), &parent);

        // Then each identity retains its own value, and the parent is unchanged.
        assert_eq!(child.value(&first).as_deref(), Some("first"));
        assert_eq!(child.value(&second).as_deref(), Some("second"));
        assert_eq!(parent.value(&first).as_deref(), Some("first"));
        assert_eq!(parent.value(&second), None);
    }

    #[test]
    fn bridge_retains_reason_identity_and_global_cancellation_order() {
        // Given a live adapter and an independent native Chord cancellation.
        let controller = AbortController::new();
        let context = with_abort_signal(controller.signal(), &BACKGROUND_CONTEXT);
        let signal = context.as_chord().abort_signal().expect("adapter");
        let (native, cancel) = maho_chord::context::with_cancel(&maho_chord::context::BACKGROUND_CONTEXT);
        // When the external source fires before the native source.
        controller.abort(Some(AbortReason::new("Error", "external")));
        cancel.cancel();
        // Then repeated adapter reads retain the source identity and comparable history.
        let first = signal.reason().expect("reason");
        let second = signal.reason().expect("reason");
        let original = controller.signal().reason_arc().expect("source reason");
        let original: Arc<dyn std::error::Error + Send + Sync> = original;
        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&first, &original));
        assert!(signal.cancellation_order().expect("external order") < native.abort_signal().expect("native").cancellation_order().expect("native order"));
    }

    #[test]
    fn nested_live_signals_keep_first_abort_without_executor_polling() {
        // Given nested combinations created before either source fires.
        let parent = AbortController::new();
        let child = AbortController::new();
        let context = with_abort_signal(parent.signal(), &BACKGROUND_CONTEXT);
        let context = with_abort_signal(child.signal(), &context);
        let shared = context.as_chord().abort_signal().expect("shared");
        // When the child fires first and both sources have fired before observation.
        child.abort(Some(AbortReason::new("Error", "child")));
        parent.abort(Some(AbortReason::new("Error", "parent")));
        // Then both adapter surfaces retain the child reason and original order.
        assert_eq!(context.abort_signal().expect("combined").reason(), child.signal().reason());
        let expected: Arc<dyn std::error::Error + Send + Sync> = child.signal().reason_arc().expect("child reason");
        assert!(Arc::ptr_eq(&shared.reason().expect("reason"), &expected));
        assert_eq!(shared.cancellation_order(), child.signal().cancellation_order());
    }

    #[test]
    fn already_aborted_combination_preserves_global_history_when_child_fired_first() {
        // Given two previously aborted inputs in reverse historical order.
        let parent = AbortController::new();
        let child = AbortController::new();
        child.abort(Some(AbortReason::new("Error", "child")));
        parent.abort(Some(AbortReason::new("Error", "parent")));
        // When they are combined parent-first.
        let combined = combine_abort_signals(&parent.signal(), &child.signal());
        // Then the earlier cancellation retains its identity regardless of input order.
        assert!(Arc::ptr_eq(&combined.reason_arc().expect("combined"), &child.signal().reason_arc().expect("child")));
        assert_eq!(combined.cancellation_order(), child.signal().cancellation_order());
    }

    #[tokio::test]
    async fn chord_consumers_observe_harness_values_and_cancellation() {
        let key = create_context_key::<String>("shared");
        let controller = AbortController::new();
        let parent = with_context_value(&key, "value".into(), &BACKGROUND_CONTEXT);
        let context = with_abort_signal(controller.signal(), &parent);
        let shared = context.as_chord();
        let signal = shared.abort_signal().expect("shared signal");
        let cancelled = signal.cancelled();

        controller.abort(None);
        tokio::time::timeout(std::time::Duration::from_secs(2), cancelled)
            .await
            .expect("cancellation");

        assert!(signal.aborted());
        assert_eq!(
            maho_chord::context::context_value(&shared, &key).as_deref(),
            Some("value")
        );
        let cleanup = without_abort_signal(&context);
        assert!(cleanup.as_chord().abort_signal().is_none());
        assert!(cleanup.abort_signal().is_none());
        assert_eq!(cleanup.value(&key).as_deref(), Some("value"));
        assert!(!parent.is_aborted());
    }
}
