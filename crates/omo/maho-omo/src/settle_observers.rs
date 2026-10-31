//! Port of `omo-senpi/src/extension/settle-observers.ts` (omo v5.1.52).
//!
//! A settle subscription owns observation, not the work it observes. Expiry or retirement stops
//! new phases and ends the wait; it never terminalizes a reflection or signals its child.
//!
//! PERF-DIVERGENCE: upstream keeps observing the abandoned promise after the race is lost. A Rust
//! handler future borrows the event and context, so it cannot be detached; it is dropped at its next
//! await point instead. Work that must outlive the observer (durable writes) is expected to be
//! spawned by the handler itself and retried from durable state on the next settle, as upstream says.
//! The native `ExtensionHandlerOptions` has no `hook_label`, so the label is fixed to `onSettled`
//! unless the handler names a phase through the observer.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};
use maho_ext_api::{
    ComponentLogger, EventResult, ExtensionContext, ExtensionEvent, ExtensionFailure,
    ExtensionHandler,
};

pub const SETTLE_OBSERVER_BUDGET: Duration = Duration::from_millis(2_000);
const DEFAULT_HOOK_LABEL: &str = "onSettled";
const RETIRED: &str = "Settle observer retired";
const EXHAUSTED: &str = "Settle observer budget exhausted";

/// What an `agent_settled` handler may use to cooperate with the bounded observation.
#[derive(Clone)]
pub struct SettleObserver {
    signal: AbortSignal,
    hook: Arc<Mutex<String>>,
}

impl SettleObserver {
    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// Names the current await and refuses to begin another phase after retirement.
    pub fn phase(&self, label: &str) -> Result<(), ExtensionFailure> {
        if let Some(reason) = self.signal.reason() {
            return Err(ExtensionFailure::new(reason.message));
        }
        *self.hook.lock().unwrap_or_else(PoisonError::into_inner) = label.to_owned();
        Ok(())
    }
}

tokio::task_local! {
    static CURRENT: SettleObserver;
}

/// The observer of the `agent_settled` handler running on this task, if any.
pub fn current_settle_observer() -> Option<SettleObserver> {
    CURRENT.try_with(Clone::clone).ok()
}

type Pending = Arc<Mutex<BTreeMap<u64, (AbortController, Option<String>)>>>;

#[derive(Clone)]
pub struct SettleObservers {
    pending: Pending,
    next: Arc<Mutex<u64>>,
    logger: Arc<dyn ComponentLogger>,
    budget: Duration,
}

fn session_id_of(context: &ExtensionContext) -> Option<String> {
    Some(context.session_manager.session_id().to_owned())
}

impl SettleObservers {
    pub fn new(logger: Arc<dyn ComponentLogger>) -> Self {
        Self::with_budget(logger, SETTLE_OBSERVER_BUDGET)
    }

    pub fn with_budget(logger: Arc<dyn ComponentLogger>, budget: Duration) -> Self {
        Self {
            pending: Arc::default(),
            next: Arc::default(),
            logger,
            budget,
        }
    }

    /// Aborts every observer owned by `session_id`, or all of them when it is unknown.
    pub fn retire(&self, session_id: Option<&str>) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        for (controller, owner) in pending.values() {
            if session_id.is_none() || owner.as_deref() == session_id {
                controller.abort(Some(AbortReason::new("AbortError", RETIRED)));
            }
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Wraps one `agent_settled` handler in a bounded observation.
    pub fn wrap(
        &self,
        component: &str,
        hook_label: Option<&str>,
        handler: ExtensionHandler,
    ) -> ExtensionHandler {
        let this = self.clone();
        let component = component.to_owned();
        let label = hook_label.unwrap_or(DEFAULT_HOOK_LABEL).to_owned();
        Arc::new(
            move |event: &mut ExtensionEvent, context: &ExtensionContext| {
                let this = this.clone();
                let component = component.clone();
                let label = label.clone();
                let handler = handler.clone();
                let session_id = session_id_of(context);
                Box::pin(async move {
                    this.observe(&component, label, handler, event, context, session_id)
                        .await
                })
            },
        )
    }

    async fn observe(
        &self,
        component: &str,
        label: String,
        handler: ExtensionHandler,
        event: &mut ExtensionEvent,
        context: &ExtensionContext,
        session_id: Option<String>,
    ) -> Result<EventResult, ExtensionFailure> {
        let controller = AbortController::new();
        let signal = controller.signal();
        let hook = Arc::new(Mutex::new(label));
        let observer = SettleObserver {
            signal: signal.clone(),
            hook: hook.clone(),
        };
        let id = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            *next += 1;
            *next
        };
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, (controller, session_id.clone()));
        let started = Instant::now();
        let outcome = {
            let work = CURRENT.scope(observer, handler(event, context));
            tokio::select! {
                biased;
                () = signal.cancelled() => None,
                () = tokio::time::sleep(self.budget) => {
                    let current = hook.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    self.logger.warn(
                        "extension_handler_stalled",
                        Some(&serde_json::json!({
                            "event": "extension_handler_stalled", "extension": "omo.js",
                            "component": component, "hook": current, "handlerEvent": "agent_settled",
                            "stage": "timeout", "durationMs": started.elapsed().as_millis() as u64,
                            "sessionId": session_id,
                        })),
                    );
                    if let Some((controller, _)) = self.pending.lock().unwrap_or_else(PoisonError::into_inner).get(&id) {
                        controller.abort(Some(AbortReason::new("TimeoutError", EXHAUSTED)));
                    }
                    None
                }
                result = work => Some(result),
            }
        };
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        match outcome {
            Some(Err(_)) if signal.aborted() => Ok(EventResult::None),
            Some(result) => result,
            None => Ok(EventResult::None),
        }
    }
}
