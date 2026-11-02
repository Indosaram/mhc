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
use idle_injection::{IdleInjection, IdleInjectionCoordinator as IdleInjectionCoordinatorTrait, IdleInjectionSource};
use maho_ext_api::{CustomMessage, DeliverAs, JsonValue, MessageDeliveryReceipt, ToolContent};

use crate::scheduler::{DeferredScheduler, DeferredTask, TurnBarrier};

pub const WAKE_CUSTOM_TYPE: &str = "omo-senpi:wake";
pub const DETAIL_SEPARATOR: &str = "\n\n";
pub const DEFAULT_COMPLETION_BATCH_LIMIT: usize = 24_000;
pub const RETIRED_ERROR_MESSAGE: &str = "idle-injection coordinator retired on session shutdown; injection not delivered";

fn read_configured_completion_batch_limit() -> usize {
    let loaded = omo_config_core::load_omo_config(&omo_config_core::LoadOmoConfigOptions::default());
    loaded
        .config
        .get("task")
        .and_then(|task| task.get("completion_batch_limit"))
        .and_then(JsonValue::as_u64)
        .map(|limit| usize::try_from(limit).unwrap_or(DEFAULT_COMPLETION_BATCH_LIMIT))
        .unwrap_or(DEFAULT_COMPLETION_BATCH_LIMIT)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn utf16_prefix(value: &str, limit: usize) -> &str {
    let mut units = 0;
    for (index, ch) in value.char_indices() {
        units += ch.len_utf16();
        if units > limit {
            return &value[..index];
        }
    }
    value
}

fn source_rank(source: IdleInjectionSource) -> u8 {
    match source {
        IdleInjectionSource::TaskStatus => 0,
        IdleInjectionSource::TaskCompletion => 0,
        IdleInjectionSource::WorkpoolAggregate => 0,
        IdleInjectionSource::TeamMessage => 1,
        IdleInjectionSource::TeamLiveness => 2,
        IdleInjectionSource::BoulderContinuation => 3,
        IdleInjectionSource::UlwContinuation => 4,
        IdleInjectionSource::DagRun => 5,
        IdleInjectionSource::Kibitzer => 6,
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
    Receipt(MessageDeliveryReceipt),
}

pub type IdleInjectionDelivery = Arc<dyn Fn(&IdleInjectionMessage, DeliverAs) -> Delivery + Send + Sync>;

struct Inner {
    deliver: IdleInjectionDelivery,
    flush: DeferredScheduler,
    soon: DeferredScheduler,
    pending: Mutex<IndexMap<String, IdleInjection>>,
    flush_scheduled: AtomicBool,
    soon_scheduled: AtomicBool,
    retired: AtomicBool,
    barrier: Arc<TurnBarrier>,
    batch_limit: Mutex<Option<usize>>,
    state_dir: Mutex<Option<PathBuf>>,
}

/// The concrete `IdleInjectionCoordinator` queue (`idle_injection::IdleInjectionCoordinator`).
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
                retired: AtomicBool::new(false),
                barrier,
                batch_limit: Mutex::new(None),
                state_dir: Mutex::new(None),
            }),
        }
    }

    pub fn set_batch_limit(&self, limit: usize) {
        *self.inner.batch_limit.lock().unwrap_or_else(PoisonError::into_inner) = Some(limit);
    }

    pub fn with_batch_limit(self, limit: usize) -> Self {
        self.set_batch_limit(limit);
        self
    }

    pub fn set_state_dir(&self, dir: impl Into<PathBuf>) {
        *self.inner.state_dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir.into());
    }

    pub fn with_state_dir(self, dir: impl Into<PathBuf>) -> Self {
        self.set_state_dir(dir);
        self
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

    pub fn barrier(&self) -> Arc<TurnBarrier> {
        Arc::clone(&self.inner.barrier)
    }

    /// Host ingress: holds the turn across the whole closure so consecutive coordinator calls inside
    /// it cannot be split by a deferred pass (see the scheduler module docs).
    pub fn run_in_turn<R>(&self, body: impl FnOnce() -> R) -> R {
        let _turn = self.enter_turn();
        body()
    }

    /// Re-arms a retired queue for the next generation. Upstream builds a fresh coordinator on every
    /// `register`; the native composition retains one across a reload, so the reload re-arms it
    /// instead of leaving the previous generation's retirement in force.
    pub fn rearm(&self) {
        self.inner.retired.store(false, Ordering::SeqCst);
    }

    fn flush(&self, deliver_as: DeliverAs) -> usize {
        let ordered: Vec<IdleInjection> = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.is_empty() {
                return 0;
            }
            // LATEST `#flush`: an all-passive queue is RETAINED - no delivery, no receipts - until a
            // non-passive entry joins; that entry then carries the batch out in source order.
            if pending.values().all(|injection| injection.passive == Some(true)) {
                return 0;
            }
            let mut ordered: Vec<IdleInjection> = pending.drain(..).map(|(_, injection)| injection).collect();
            ordered.sort_by_key(|injection| source_rank(injection.source));
            ordered
        };
        let collapsed = ordered.len();
        let batch_limit = self
            .inner
            .batch_limit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or_else(read_configured_completion_batch_limit);

        let total_unbounded_len = if ordered.is_empty() {
            0
        } else {
            let content_len: usize = ordered.iter().map(|inj| utf16_len(&inj.content)).sum();
            let sep_len = (ordered.len() - 1) * utf16_len(DETAIL_SEPARATOR);
            content_len + sep_len
        };

        let (content, fitting_count) = if total_unbounded_len <= batch_limit {
            let joined = ordered
                .iter()
                .map(|injection| injection.content.as_str())
                .collect::<Vec<_>>()
                .join(DETAIL_SEPARATOR);
            let count = ordered.len();
            (joined, count)
        } else {
            let state_dir = self
                .inner
                .state_dir
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("omo-completion-manifests"));
            let dir = if state_dir.ends_with("completion-manifests") {
                state_dir
            } else {
                state_dir.join("completion-manifests")
            };
            if let Err(error) = std::fs::create_dir_all(&dir) {
                eprintln!("failed to create completion manifest dir {}: {error}", dir.display());
            }
            let manifest_filename = format!("wake-manifest-{}.txt", uuid::Uuid::new_v4().simple());
            let manifest_path = dir.join(manifest_filename);
            let manifest_url = format!("local://{}", manifest_path.display());

            let mut fitting_k = 0;
            let sep_units = utf16_len(DETAIL_SEPARATOR);

            let mut current_content_units = 0;
            for (idx, injection) in ordered.iter().enumerate() {
                let next_k = idx + 1;
                let entry_units = utf16_len(&injection.content);
                let new_content_units = if idx == 0 {
                    entry_units
                } else {
                    current_content_units + sep_units + entry_units
                };

                let remaining = ordered.len() - next_k;
                if remaining == 0 {
                    if new_content_units <= batch_limit {
                        fitting_k = next_k;
                        current_content_units = new_content_units;
                    }
                    break;
                }
                let trailing_line = format!("{remaining} more task results: {manifest_url}");
                let trailing_units = sep_units + utf16_len(&trailing_line);

                if new_content_units + trailing_units <= batch_limit {
                    fitting_k = next_k;
                    current_content_units = new_content_units;
                } else {
                    break;
                }
            }

            let remaining_count = ordered.len() - fitting_k;
            let remaining_content = ordered[fitting_k..]
                .iter()
                .map(|inj| inj.content.as_str())
                .collect::<Vec<_>>()
                .join(DETAIL_SEPARATOR);

            if let Err(error) = std::fs::write(&manifest_path, &remaining_content) {
                eprintln!("failed to write completion manifest {}: {error}", manifest_path.display());
            }

            let trailing_line = format!("{remaining_count} more task results: {manifest_url}");
            let final_content = if fitting_k == 0 {
                if utf16_len(&trailing_line) <= batch_limit {
                    trailing_line
                } else {
                    utf16_prefix(&trailing_line, batch_limit).to_string()
                }
            } else {
                let prefix = ordered[..fitting_k]
                    .iter()
                    .map(|inj| inj.content.as_str())
                    .collect::<Vec<_>>()
                    .join(DETAIL_SEPARATOR);
                format!("{prefix}{DETAIL_SEPARATOR}{trailing_line}")
            };

            (final_content, fitting_k)
        };

        let message = IdleInjectionMessage {
            custom_type: WAKE_CUSTOM_TYPE,
            content,
            display: false,
            details: ordered[..fitting_count]
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
            Ok(Delivery::Receipt(receipt)) => {
                receipt.on_settled(move |outcome| match outcome {
                    Ok(()) => flush_all(&ordered),
                    Err(reason) => fail_all(&ordered, &reason),
                });
            }
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
    fn enqueue(&self, injection: IdleInjection) -> bool {
        let _turn = self.enter_turn();
        let mut pending = self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if self.inner.retired.load(Ordering::SeqCst) {
            return false;
        }
        pending.insert(injection.key.clone(), injection);
        true
    }

    fn retire(&self) {
        let _turn = self.enter_turn();
        let dropped: Vec<IdleInjection> = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner);
            self.inner.retired.store(true, Ordering::SeqCst);
            pending.drain(..).map(|(_, injection)| injection).collect()
        };
        // Ownership never transferred for a refused injection; a queued one is handed back to its
        // producer as a delivery failure so a completion caught inside the batch window is recorded
        // as undelivered and redelivered after the reload.
        for injection in &dropped {
            if let Some(callback) = &injection.on_delivery_failed {
                callback(RETIRED_ERROR_MESSAGE);
            }
        }
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
