//! Port of `omo-senpi/src/extension/idle-injection-coordinator.ts` at pin `77f3067f1`.
//!
//! The single injection queue for the parent session: task completions, team messages, DAG run
//! summaries and the ulw-loop continuation all enqueue here, and one deferred flush collapses
//! everything ready within a batch window into exactly one hidden `omo-senpi:wake` steer.
//!
//! Divergence from upstream (ledger N/A reasons):
//! - upstream's `#flush` rethrows a synchronous delivery throw; the `maho_ext_api` trait method
//!   `flush_on_idle` returns `usize`, so a native delivery failure is reported only through
//!   `on_delivery_failed` and cannot propagate to the caller.
//! - upstream's promise delivery is settled with `.then(...)`; here `Delivery::Pending` spawns the
//!   future on the current tokio runtime and needs a runtime to settle.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use maho_ext_api::{
    CustomMessage, DeliverAs, IdleInjection, IdleInjectionCoordinator as IdleInjectionCoordinatorTrait,
    IdleInjectionSource, JsonValue, ToolContent,
};

use crate::scheduler::{DeferredScheduler, DeferredTask, TurnBarrier};

pub const WAKE_CUSTOM_TYPE: &str = "omo-senpi:wake";
pub const DETAIL_SEPARATOR: &str = "\n\n";

fn source_rank(source: IdleInjectionSource) -> u8 {
    match source {
        IdleInjectionSource::TaskCompletion => 0,
        IdleInjectionSource::TeamMessage => 1,
        IdleInjectionSource::TeamLiveness => 2,
        IdleInjectionSource::BoulderContinuation => 3,
        IdleInjectionSource::UlwContinuation => 4,
        IdleInjectionSource::DagRun => 5,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleInjectionDetail {
    pub custom_type: String,
    pub details: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdleInjectionMessage {
    pub custom_type: &'static str,
    pub content: String,
    pub display: bool,
    pub details: Vec<IdleInjectionDetail>,
}

impl IdleInjectionMessage {
    pub fn to_custom_message(&self) -> CustomMessage {
        let details = self
            .details
            .iter()
            .map(|detail| serde_json::json!({
                "customType": detail.custom_type,
                "details": detail.details,
            }))
            .collect::<Vec<JsonValue>>();
        CustomMessage {
            custom_type: self.custom_type.to_owned(),
            content: vec![ToolContent::text(self.content.clone())],
            display: self.display,
            details: Some(JsonValue::Array(details)),
        }
    }
}

pub type DeliveryFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;

pub enum Delivery {
    Delivered,
    Failed(String),
    Pending(DeliveryFuture),
}

pub type IdleInjectionDelivery = Arc<dyn Fn(&IdleInjectionMessage, DeliverAs) -> Delivery + Send + Sync>;

struct Inner {
    deliver: IdleInjectionDelivery,
    flush: DeferredScheduler,
    soon: DeferredScheduler,
    pending: Mutex<IndexMap<String, IdleInjection>>,
    flush_scheduled: AtomicBool,
    soon_scheduled: AtomicBool,
    barrier: Arc<TurnBarrier>,
}

/// The concrete `IdleInjectionCoordinator` queue (`maho_ext_api::IdleInjectionCoordinator`).
#[derive(Clone)]
pub struct IdleInjectionQueue {
    inner: Arc<Inner>,
}

impl IdleInjectionQueue {
    pub fn new(deliver: IdleInjectionDelivery, flush: DeferredScheduler, soon: DeferredScheduler) -> Self {
        let barrier = flush.barrier();
        Self {
            inner: Arc::new(Inner {
                deliver,
                flush,
                soon,
                pending: Mutex::new(IndexMap::new()),
                flush_scheduled: AtomicBool::new(false),
                soon_scheduled: AtomicBool::new(false),
                barrier,
            }),
        }
    }

    /// Upstream default: the composition's batch window, with the idle-edge hand-off on the same
    /// scheduler.
    pub fn with_defaults(deliver: IdleInjectionDelivery, flush: DeferredScheduler) -> Self {
        let soon = DeferredScheduler::runtime(flush.barrier(), None);
        Self::new(deliver, flush, soon)
    }

    pub fn as_arc(self) -> Arc<dyn IdleInjectionCoordinatorTrait> {
        Arc::new(self)
    }

    /// Enters the turn barrier; a deferred pass cannot run while the guard is held.
    pub fn enter_turn(&self) -> crate::scheduler::TurnGuard {
        self.inner.barrier.enter()
    }

    /// Host ingress: holds the turn across the whole closure so consecutive coordinator calls inside
    /// it cannot be split by a deferred pass (see the scheduler module docs).
    pub fn run_in_turn<R>(&self, body: impl FnOnce() -> R) -> R {
        let _turn = self.enter_turn();
        body()
    }

    fn flush(&self, deliver_as: DeliverAs) -> usize {
        let ordered: Vec<IdleInjection> = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.is_empty() {
                return 0;
            }
            let mut ordered: Vec<IdleInjection> = pending.drain(..).map(|(_, injection)| injection).collect();
            ordered.sort_by_key(|injection| source_rank(injection.source));
            ordered
        };
        let collapsed = ordered.len();
        let message = IdleInjectionMessage {
            custom_type: WAKE_CUSTOM_TYPE,
            content: ordered.iter().map(|injection| injection.content.as_str()).collect::<Vec<_>>().join(DETAIL_SEPARATOR),
            display: false,
            details: ordered
                .iter()
                .filter_map(|injection| {
                    injection.custom_type.as_ref().map(|custom_type| IdleInjectionDetail {
                        custom_type: custom_type.clone(),
                        details: injection.details.clone(),
                    })
                })
                .collect(),
        };
        let deliver = Arc::clone(&self.inner.deliver);
        match catch_unwind(AssertUnwindSafe(|| deliver(&message, deliver_as))) {
            Err(payload) => {
                let reason = panic_reason(&payload);
                fail_all(&ordered, &reason);
                std::panic::resume_unwind(payload);
            }
            Ok(Delivery::Failed(reason)) => fail_all(&ordered, &reason),
            Ok(Delivery::Delivered) => flush_all(&ordered),
            Ok(Delivery::Pending(future)) => match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        match future.await {
                            Ok(()) => flush_all(&ordered),
                            Err(reason) => fail_all(&ordered, &reason),
                        }
                    });
                }
                Err(_) => fail_all(&ordered, "no async runtime to settle the deferred injection delivery"),
            },
        }
        collapsed
    }
}

impl IdleInjectionCoordinatorTrait for IdleInjectionQueue {
    fn enqueue(&self, injection: IdleInjection) {
        let _turn = self.enter_turn();
        self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(injection.key.clone(), injection);
    }

    fn schedule_flush(&self) {
        let _turn = self.enter_turn();
        if self.inner.flush_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        let task: DeferredTask = Box::new(move || {
            let _turn = this.enter_turn();
            this.inner.flush_scheduled.store(false, Ordering::SeqCst);
            this.flush(DeliverAs::Steer);
        });
        self.inner.flush.schedule(task);
    }

    fn flush_soon(&self) {
        let _turn = self.enter_turn();
        if self.inner.soon_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        let task: DeferredTask = Box::new(move || {
            let _turn = this.enter_turn();
            this.inner.soon_scheduled.store(false, Ordering::SeqCst);
            this.flush(DeliverAs::Steer);
        });
        self.inner.soon.schedule(task);
    }

    fn flush_on_idle(&self) -> usize {
        let _turn = self.enter_turn();
        self.flush(DeliverAs::Steer)
    }

    fn pending_count(&self) -> usize {
        let _turn = self.enter_turn();
        self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn remove(&self, key: &str) -> bool {
        let _turn = self.enter_turn();
        self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner).shift_remove(key).is_some()
    }
}

fn flush_all(ordered: &[IdleInjection]) {
    for injection in ordered {
        if let Some(callback) = &injection.on_flushed {
            callback();
        }
    }
}

fn fail_all(ordered: &[IdleInjection], reason: &str) {
    for injection in ordered {
        if let Some(callback) = &injection.on_delivery_failed {
            callback(reason);
        }
    }
}

fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "injection delivery panicked".to_owned()
    }
}
