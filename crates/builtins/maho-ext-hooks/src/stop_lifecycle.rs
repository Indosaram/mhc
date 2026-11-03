//! Port of `hooks/stop-lifecycle.ts` (senpi 903b8336).
//!
//! Dispatches the Stop hook when the session actually stops. A turn that ends while a
//! background wake source is live (subagent task, DAG run, terminal monitor, background bash
//! session, detached eval cell, loop-guard hold) has handed the session to work that wakes it
//! again, so its Stop waits: the wake turn's own end reports it, or the drain does when the
//! work clears without waking the session.
//!
//! The turn tracker that keys Stop records stays in `index.rs`'s pending state (it is reset by
//! the same input handler upstream's `resetTurn` serves); the dispatch closure reads it.

use maho_ext_api::{
    EventKind, EventResult, ExtensionApi, ExtensionContext, ExtensionEvent, ExtensionFailure,
    ExtensionFuture, NotificationType,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    time::Duration,
};

/// Background work usually clears its wake source and wakes the session in the same
/// completion handler, in either order. A drained live set therefore waits this long for
/// the wake turn before Stop reports the session quiet; a turn that starts first cancels it.
pub const STOP_DRAIN_GRACE_MS: u64 = 2_000;

/// `WAKE_SOURCE_STATE_EVENT` (`monitor-state-event.ts`).
pub const WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";

/// A pending question is the user's turn, which is exactly what Stop reports.
const USER_TURN_WAKE_SOURCE: &str = "ask-user";

const STALE_EXTENSION_CONTEXT_ERROR_PREFIX: &str =
    "This extension ctx is stale after session replacement or reload.";

/// `isWakeSourceStateEvent`: `{ source: non-empty string, activeCount: finite number }`.
pub fn is_wake_source_state_event(value: &Value) -> bool {
    value.is_object()
        && value
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| !source.is_empty())
        && value
            .get("activeCount")
            .and_then(Value::as_f64)
            .is_some_and(f64::is_finite)
}

/// What the lifecycle reads from the context of the turn whose Stop it holds.
pub trait StopContext: Clone + Send + Sync + 'static {
    /// `ctx.isIdle() && !ctx.hasPendingMessages()`; a retired context is never quiet.
    fn is_quiet(&self) -> bool;
    fn notify_error(&self, message: &str);
}

impl StopContext for ExtensionContext {
    fn is_quiet(&self) -> bool {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.is_idle() && !self.has_pending_messages().unwrap_or(false)
        }))
        .unwrap_or(false)
    }
    fn notify_error(&self, message: &str) {
        self.ui.notify(message, NotificationType::Error);
    }
}

/// Runs the Stop hook for one held or immediate stop and applies its result.
pub type DispatchStop<C> = Arc<dyn Fn(C, Value) -> ExtensionFuture<'static, ()> + Send + Sync>;

struct PendingStop<C> {
    ctx: C,
    input: Value,
}

struct State<C> {
    background_work: BTreeMap<String, f64>,
    pending: Option<PendingStop<C>>,
    drain_timer: Option<tokio::task::JoinHandle<()>>,
    /// Bumped on every arm and clear. `JoinHandle::abort` cannot stop a timer task that already
    /// finished its sleep, so a timer acts only while its generation is still current (upstream
    /// `clearTimeout` is atomic).
    drain_generation: u64,
    /// Bumped whenever the held Stop is replaced or dropped, so a quiet check made outside the
    /// lock cannot release a different Stop.
    pending_generation: u64,
}

struct Inner<C: StopContext> {
    state: Mutex<State<C>>,
    dispatch_stop: DispatchStop<C>,
    grace: Duration,
}

/// Handle on the registered Stop lifecycle.
pub struct StopLifecycle<C: StopContext> {
    inner: Arc<Inner<C>>,
}

impl<C: StopContext> Clone for StopLifecycle<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<C: StopContext> Inner<C> {
    fn lock(&self) -> MutexGuard<'_, State<C>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn dispatch_detached(&self, stop: PendingStop<C>) {
        let dispatch = (self.dispatch_stop)(stop.ctx.clone(), stop.input);
        let ctx = stop.ctx;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = dispatch.await {
                let message = error.to_string();
                if message.starts_with(STALE_EXTENSION_CONTEXT_ERROR_PREFIX) {
                    return;
                }
                ctx.notify_error(&format!("Stop hook dispatch failed: {message}"));
            }
        });
    }

    fn dispatch_pending_if_quiet(&self, generation: u64) {
        let (ctx, pending_generation) = {
            let mut state = self.lock();
            if state.drain_generation != generation {
                return;
            }
            state.drain_timer = None;
            if !state.background_work.is_empty() {
                return;
            }
            let Some(stop) = state.pending.as_ref() else {
                return;
            };
            (stop.ctx.clone(), state.pending_generation)
        };
        // The quiet check calls into the host, which may publish wake-source state synchronously,
        // so it runs without the state lock.
        // A wake turn that is already running or queued reports its own Stop when it ends.
        if !ctx.is_quiet() {
            return;
        }
        let stop = {
            let mut state = self.lock();
            if state.pending_generation != pending_generation
                || state.drain_timer.is_some()
                || !state.background_work.is_empty()
            {
                return;
            }
            state.pending.take()
        };
        if let Some(stop) = stop {
            self.dispatch_detached(stop);
        }
    }
}

fn clear_drain_timer<C>(state: &mut State<C>) {
    state.drain_generation = state.drain_generation.wrapping_add(1);
    if let Some(timer) = state.drain_timer.take() {
        timer.abort();
    }
}

fn set_pending<C>(state: &mut State<C>, pending: Option<PendingStop<C>>) {
    state.pending_generation = state.pending_generation.wrapping_add(1);
    state.pending = pending;
}

impl<C: StopContext> StopLifecycle<C> {
    pub fn new(dispatch_stop: DispatchStop<C>) -> Self {
        Self::with_grace(dispatch_stop, Duration::from_millis(STOP_DRAIN_GRACE_MS))
    }

    /// The lifecycle under an explicit drain grace (tests shorten it).
    pub fn with_grace(dispatch_stop: DispatchStop<C>, grace: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    background_work: BTreeMap::new(),
                    pending: None,
                    drain_timer: None,
                    drain_generation: 0,
                    pending_generation: 0,
                }),
                dispatch_stop,
                grace,
            }),
        }
    }

    /// `pi.events.on(WAKE_SOURCE_STATE_EVENT)`.
    pub fn on_wake_source_state(&self, data: &Value) {
        if !is_wake_source_state_event(data) {
            return;
        }
        let source = data["source"].as_str().unwrap_or_default();
        if source == USER_TURN_WAKE_SOURCE {
            return;
        }
        let active_count = data["activeCount"].as_f64().unwrap_or_default();
        let mut state = self.inner.lock();
        if active_count > 0.0 {
            state
                .background_work
                .insert(source.to_owned(), active_count);
        } else {
            state.background_work.remove(source);
        }
        clear_drain_timer(&mut state);
        if !state.background_work.is_empty() || state.pending.is_none() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak: Weak<Inner<C>> = Arc::downgrade(&self.inner);
        let grace = self.inner.grace;
        let generation = state.drain_generation;
        state.drain_timer = Some(runtime.spawn(async move {
            tokio::time::sleep(grace).await;
            if let Some(inner) = weak.upgrade() {
                inner.dispatch_pending_if_quiet(generation);
            }
        }));
    }

    /// `pi.on("agent_end")`: hold the Stop while background work is live, else dispatch it.
    pub async fn on_agent_end(&self, ctx: C, input: Value) -> Result<(), ExtensionFailure> {
        {
            let mut state = self.inner.lock();
            if !state.background_work.is_empty() {
                set_pending(&mut state, Some(PendingStop { ctx, input }));
                return Ok(());
            }
            clear_drain_timer(&mut state);
            set_pending(&mut state, None);
        }
        (self.inner.dispatch_stop)(ctx, input).await
    }

    /// `pi.on("agent_start")` and `pi.on("session_shutdown")`: drop a held Stop.
    pub fn drop_pending(&self) {
        let mut state = self.inner.lock();
        clear_drain_timer(&mut state);
        set_pending(&mut state, None);
    }

    pub fn holds_stop(&self) -> bool {
        self.inner.lock().pending.is_some()
    }

    pub fn drain_armed(&self) -> bool {
        self.inner.lock().drain_timer.is_some()
    }
}

pub type BuildStopInput = Arc<dyn Fn(&ExtensionEvent, &ExtensionContext) -> Option<Value> + Send + Sync>;

/// Registers the Stop lifecycle on `api`. `build_input` turns an `agent_end` into the Stop hook
/// input; `dispatch_stop` runs and applies the hook.
pub fn register_stop_lifecycle(
    api: &mut ExtensionApi,
    build_input: BuildStopInput,
    dispatch_stop: DispatchStop<ExtensionContext>,
) -> StopLifecycle<ExtensionContext> {
    let lifecycle = StopLifecycle::new(dispatch_stop);
    let weak = Arc::downgrade(&lifecycle.inner);
    let subscription = api.events.on(
        WAKE_SOURCE_STATE_EVENT,
        Arc::new(move |data| {
            if let Some(inner) = weak.upgrade() {
                StopLifecycle { inner }.on_wake_source_state(data);
            }
        }),
    );
    // The subscription lives as long as the registered handlers that own it.
    let subscription = Arc::new(Mutex::new(Some(subscription)));
    {
        let lifecycle = lifecycle.clone();
        let subscription = Arc::clone(&subscription);
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |event, ctx| {
                let lifecycle = lifecycle.clone();
                let _retained = &subscription;
                let input = build_input(event, ctx);
                let ctx = ctx.clone();
                Box::pin(async move {
                    if let Some(input) = input {
                        lifecycle.on_agent_end(ctx, input).await?;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
    for kind in [EventKind::AgentStart, EventKind::SessionShutdown] {
        let lifecycle = lifecycle.clone();
        api.on(
            kind,
            Arc::new(move |_, _| {
                lifecycle.drop_pending();
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
    }
    lifecycle
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone)]
    struct Ctx {
        quiet: Arc<AtomicBool>,
        errors: Arc<Mutex<Vec<String>>>,
    }

    impl StopContext for Ctx {
        fn is_quiet(&self) -> bool {
            self.quiet.load(Ordering::SeqCst)
        }
        fn notify_error(&self, message: &str) {
            self.errors.lock().unwrap().push(message.to_owned());
        }
    }

    struct Fixture {
        ctx: Ctx,
        payloads: Arc<Mutex<Vec<Value>>>,
        dispatched: Arc<tokio::sync::Notify>,
        lifecycle: StopLifecycle<Ctx>,
    }

    fn fixture(grace_ms: u64) -> Fixture {
        let payloads: Arc<Mutex<Vec<Value>>> = Arc::default();
        let dispatched = Arc::new(tokio::sync::Notify::new());
        let recorded = Arc::clone(&payloads);
        let notify = Arc::clone(&dispatched);
        let lifecycle = StopLifecycle::with_grace(
            Arc::new(move |_ctx: Ctx, input| {
                recorded.lock().unwrap().push(input);
                notify.notify_one();
                Box::pin(async { Ok(()) })
            }),
            Duration::from_millis(grace_ms),
        );
        Fixture {
            ctx: Ctx {
                quiet: Arc::new(AtomicBool::new(true)),
                errors: Arc::default(),
            },
            payloads,
            dispatched,
            lifecycle,
        }
    }

    fn stop() -> Value {
        json!({"event":"Stop","hook_event_name":"Stop","session_id":"s"})
    }

    fn publish(fixture: &Fixture, source: &str, active_count: u32) {
        fixture
            .lifecycle
            .on_wake_source_state(&json!({"source": source, "activeCount": active_count}));
    }

    async fn settled(fixture: &Fixture) {
        tokio::time::timeout(Duration::from_secs(5), fixture.dispatched.notified())
            .await
            .expect("stop dispatch");
    }

    #[tokio::test]
    async fn holds_stop_while_a_background_wake_source_is_live_and_reports_it_once_drained_while_idle()
     {
        let fixture = fixture(10);
        publish(&fixture, "terminal-monitors", 1);
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        assert!(fixture.payloads.lock().unwrap().is_empty());
        assert!(fixture.lifecycle.holds_stop());
        publish(&fixture, "terminal-monitors", 0);
        assert!(fixture.lifecycle.drain_armed());
        settled(&fixture).await;
        assert_eq!(fixture.payloads.lock().unwrap().clone(), vec![stop()]);
        assert!(!fixture.lifecycle.holds_stop());
    }

    #[tokio::test]
    async fn reports_stop_at_the_turn_end_when_only_a_pending_ask_user_question_is_live() {
        let fixture = fixture(10);
        publish(&fixture, "ask-user", 1);
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        assert_eq!(fixture.payloads.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn drops_the_held_stop_when_the_wake_turn_starts_so_that_turn_reports_exactly_one_stop() {
        let fixture = fixture(60_000);
        publish(&fixture, "senpi-task", 1);
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        assert!(fixture.payloads.lock().unwrap().is_empty());
        publish(&fixture, "senpi-task", 0);
        assert!(fixture.lifecycle.drain_armed());
        fixture.lifecycle.drop_pending();
        assert!(!fixture.lifecycle.drain_armed());
        assert!(!fixture.lifecycle.holds_stop());
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        assert_eq!(fixture.payloads.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn keeps_stop_held_across_a_wake_turn_that_ends_with_the_work_still_live() {
        let fixture = fixture(10);
        publish(&fixture, "terminal-background-sessions", 1);
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        fixture.lifecycle.drop_pending();
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        assert!(fixture.payloads.lock().unwrap().is_empty());
        assert!(fixture.lifecycle.holds_stop());
        assert!(!fixture.lifecycle.drain_armed());
    }

    #[tokio::test]
    async fn a_busy_session_at_drain_leaves_the_stop_to_the_wake_turn() {
        let fixture = fixture(1);
        publish(&fixture, "senpi-codemode", 1);
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        fixture.ctx.quiet.store(false, Ordering::SeqCst);
        publish(&fixture, "senpi-codemode", 0);
        let lifecycle = fixture.lifecycle.clone();
        tokio::time::timeout(Duration::from_secs(5), async move {
            while lifecycle.drain_armed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drain timer fired");
        assert!(fixture.payloads.lock().unwrap().is_empty());
        assert!(fixture.lifecycle.holds_stop());
    }

    #[test]
    fn wake_source_events_are_validated() {
        assert!(is_wake_source_state_event(
            &json!({"source":"x","activeCount":0})
        ));
        assert!(!is_wake_source_state_event(
            &json!({"source":"","activeCount":1})
        ));
        assert!(!is_wake_source_state_event(&json!({"source":"x"})));
        assert!(!is_wake_source_state_event(
            &json!({"source":"x","activeCount":"1"})
        ));
        assert!(!is_wake_source_state_event(&json!(null)));
    }

    #[tokio::test]
    async fn a_superseded_drain_timer_that_already_slept_neither_dispatches_nor_clears_the_new_one()
    {
        let fixture = fixture(60_000);
        publish(&fixture, "senpi-task", 1);
        fixture
            .lifecycle
            .on_agent_end(fixture.ctx.clone(), stop())
            .await
            .unwrap();
        publish(&fixture, "senpi-task", 0);
        let stale = fixture.lifecycle.inner.lock().drain_generation;
        // A second drain re-arms; the first timer's generation is now stale.
        publish(&fixture, "terminal-monitors", 0);
        assert!(fixture.lifecycle.drain_armed());
        // The stale timer wakes after its sleep (its abort came too late) and runs its body.
        fixture.lifecycle.inner.dispatch_pending_if_quiet(stale);
        assert!(fixture.payloads.lock().unwrap().is_empty());
        assert!(fixture.lifecycle.holds_stop());
        assert!(fixture.lifecycle.drain_armed());
        let current = fixture.lifecycle.inner.lock().drain_generation;
        fixture.lifecycle.inner.dispatch_pending_if_quiet(current);
        settled(&fixture).await;
        assert_eq!(fixture.payloads.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn quiet_check_runs_without_the_state_lock() {
        #[derive(Clone)]
        struct Reentrant(Arc<Mutex<Option<StopLifecycle<Reentrant>>>>);
        impl StopContext for Reentrant {
            fn is_quiet(&self) -> bool {
                // A host that publishes wake-source state while answering would deadlock here
                // if the lifecycle held its state lock.
                let lifecycle = self.0.lock().unwrap().clone().unwrap();
                lifecycle.holds_stop()
            }
            fn notify_error(&self, _: &str) {}
        }
        let dispatched = Arc::new(tokio::sync::Notify::new());
        let notify = Arc::clone(&dispatched);
        let lifecycle = StopLifecycle::with_grace(
            Arc::new(move |_ctx: Reentrant, _| {
                notify.notify_one();
                Box::pin(async { Ok(()) })
            }),
            Duration::from_millis(1),
        );
        let ctx = Reentrant(Arc::new(Mutex::new(Some(lifecycle.clone()))));
        lifecycle.on_wake_source_state(&json!({"source":"senpi-task","activeCount":1}));
        lifecycle.on_agent_end(ctx.clone(), stop()).await.unwrap();
        lifecycle.on_wake_source_state(&json!({"source":"senpi-task","activeCount":0}));
        tokio::time::timeout(Duration::from_secs(5), dispatched.notified())
            .await
            .expect("stop dispatch");
        assert!(!lifecycle.holds_stop());
        ctx.0.lock().unwrap().take();
    }
}
