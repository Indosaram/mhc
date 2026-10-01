//! Port of senpi `packages/agent/src/harness/events.ts` and the `HarnessEvent` union from
//! `agent-harness.ts`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use maho_ai::types::{AssistantMessage, BoxFuture, DeferredHandle, ToolResultMessage, Usage};
use tokio::sync::{Notify, oneshot};

use crate::harness::context::Context;
use crate::harness::session::types::{Entry, JsonValue, OperationError, UsageRow};
use crate::types::AgentMessage;

#[derive(Debug, Clone, PartialEq)]
pub struct LaneQueuedItem {
    pub entry_id: String,
    pub kind: String,
    pub item_type: String,
    pub message: Option<AgentMessage>,
    pub custom_type: Option<String>,
    pub data: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunEndPayload {
    pub run_id: String,
    pub from_tip_id: Option<String>,
    pub tip_id: Option<String>,
    pub ended_at: i64,
    pub status: String,
    pub error: Option<OperationError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NavigationEndPayload {
    pub run_id: String,
    pub from_tip_id: Option<String>,
    pub tip_id: Option<String>,
    pub ended_at: i64,
    pub status: String,
    pub error: Option<OperationError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HandlerErrorPayload {
    pub error: String,
    pub stack: Option<String>,
    pub kind: String,
    pub hook: Option<String>,
    pub event: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEventPayload {
    RunStart { run_id: String, started_at: i64 },
    RunResume { run_id: String },
    RunSuspend { run_id: String, deferred: DeferredHandle, poll: u32 },
    OperationAbort { operation_id: String, steer: Vec<AgentMessage>, follow_up: Vec<AgentMessage> },
    RunEnd(RunEndPayload),
    Fault { code: String, message: String },
    HandlerError(HandlerErrorPayload),
    TurnStart { run_id: String, turn_id: String },
    TurnEnd { run_id: String, turn_id: String, message: Box<AssistantMessage>, tool_results: Vec<ToolResultMessage> },
    RetryScheduled {
        run_id: String,
        step: String,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        not_before: i64,
        error_message: String,
    },
    RetryStart { run_id: String, step: String, attempt: u32 },
    RetryEnd { run_id: String, step: String, attempt: u32, success: bool, final_error: Option<String> },
    MessageStart { run_id: Option<String>, message: AgentMessage },
    MessageUpdate { run_id: String, message: AgentMessage, event: Box<maho_ai::types::AssistantMessageEvent>, frame: Option<maho_ai::utils::assistant_message_frame::AssistantMessageFrame> },
    MessageEnd { run_id: Option<String>, message: AgentMessage, entry_id: Option<String> },
    ToolStart { run_id: String, turn_id: String, tool_call_id: String, tool_name: String },
    ToolUpdate { run_id: String, turn_id: String, tool_call_id: String, tool_name: String },
    ToolEnd { run_id: String, turn_id: String, tool_call_id: String, tool_name: String, is_error: bool, terminate: bool },
    EntryAdded { entry: Box<Entry> },
    QueueUpdate { queues: Vec<LaneQueuedItem> },
    ValueUpdate { value: String, name: Option<String>, target_id: Option<String>, label: Option<String> },
    ConfigUpdate { property: String, value: JsonValue, previous: JsonValue },
    CompactionStart { run_id: String, reason: String, started_at: i64 },
    CompactionEnd { run_id: String, reason: String, ended_at: i64, status: String, entry_id: Option<String> },
    NavigationStart { run_id: String, target_id: Option<String>, started_at: i64 },
    NavigationEnd(NavigationEndPayload),
    LaneCreated { at: Option<String> },
    Usage { lane: String, row: UsageRow, totals: Usage },
}

impl HarnessEventPayload {
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::RunStart { .. } => "run_start",
            Self::RunResume { .. } => "run_resume",
            Self::RunSuspend { .. } => "run_suspend",
            Self::OperationAbort { .. } => "operation_abort",
            Self::RunEnd(_) => "run_end",
            Self::Fault { .. } => "fault",
            Self::HandlerError(_) => "handler_error",
            Self::TurnStart { .. } => "turn_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::RetryScheduled { .. } => "retry_scheduled",
            Self::RetryStart { .. } => "retry_start",
            Self::RetryEnd { .. } => "retry_end",
            Self::MessageStart { .. } => "message_start",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageEnd { .. } => "message_end",
            Self::ToolStart { .. } => "tool_start",
            Self::ToolUpdate { .. } => "tool_update",
            Self::ToolEnd { .. } => "tool_end",
            Self::EntryAdded { .. } => "entry_added",
            Self::QueueUpdate { .. } => "queue_update",
            Self::ValueUpdate { .. } => "value_update",
            Self::ConfigUpdate { .. } => "config_update",
            Self::CompactionStart { .. } => "compaction_start",
            Self::CompactionEnd { .. } => "compaction_end",
            Self::NavigationStart { .. } => "navigation_start",
            Self::NavigationEnd(_) => "navigation_end",
            Self::LaneCreated { .. } => "lane_created",
            Self::Usage { .. } => "usage",
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::RunStart { run_id, .. }
            | Self::RunResume { run_id }
            | Self::RunSuspend { run_id, .. }
            | Self::TurnStart { run_id, .. }
            | Self::TurnEnd { run_id, .. }
            | Self::RetryScheduled { run_id, .. }
            | Self::RetryStart { run_id, .. }
            | Self::RetryEnd { run_id, .. }
            | Self::MessageUpdate { run_id, .. }
            | Self::ToolStart { run_id, .. }
            | Self::ToolUpdate { run_id, .. }
            | Self::ToolEnd { run_id, .. }
            | Self::CompactionStart { run_id, .. }
            | Self::CompactionEnd { run_id, .. }
            | Self::NavigationStart { run_id, .. } => Some(run_id),
            Self::RunEnd(payload) => Some(&payload.run_id),
            Self::NavigationEnd(payload) => Some(&payload.run_id),
            Self::MessageStart { run_id, .. } | Self::MessageEnd { run_id, .. } => run_id.as_deref(),
            _ => None,
        }
    }

    pub fn config_property(&self) -> Option<&str> {
        match self {
            Self::ConfigUpdate { property, .. } => Some(property),
            _ => None,
        }
    }

    pub fn config_active_tools(&self) -> Option<Vec<String>> {
        match self {
            Self::ConfigUpdate { property, value, .. } if property == "activeTools" => value
                .as_array()
                .map(|values| values.iter().filter_map(|value| value.as_str().map(ToOwned::to_owned)).collect()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HarnessEvent {
    pub lane: Option<String>,
    pub recovery: Option<bool>,
    pub payload: HarnessEventPayload,
}

impl HarnessEvent {
    pub fn new(payload: HarnessEventPayload, lane: Option<String>) -> Self {
        Self { lane, recovery: None, payload }
    }

    pub fn event_type(&self) -> &'static str {
        self.payload.event_type()
    }

    pub fn run_id(&self) -> Option<&str> {
        self.payload.run_id()
    }

    pub fn run_start(run_id: impl Into<String>, started_at: i64, lane: impl Into<String>) -> Self {
        Self::new(HarnessEventPayload::RunStart { run_id: run_id.into(), started_at }, Some(lane.into()))
    }

    pub fn handler_error(error: impl Into<String>, kind: &str, event: &str, lane: Option<String>) -> Self {
        Self::new(
            HarnessEventPayload::HandlerError(HandlerErrorPayload {
                error: error.into(),
                stack: None,
                kind: kind.to_owned(),
                hook: None,
                event: Some(event.to_owned()),
            }),
            lane,
        )
    }

    pub fn handler_error_message(&self) -> Option<&str> {
        match &self.payload {
            HarnessEventPayload::HandlerError(payload) => Some(&payload.error),
            _ => None,
        }
    }

    pub fn navigation_end(run_id: impl Into<String>, status: &str, ended_at: i64, lane: impl Into<String>) -> Self {
        Self::new(
            HarnessEventPayload::NavigationEnd(NavigationEndPayload {
                run_id: run_id.into(),
                from_tip_id: None,
                tip_id: None,
                ended_at,
                status: status.to_owned(),
                error: None,
            }),
            Some(lane.into()),
        )
    }

    pub fn queue_update(queues: Vec<LaneQueuedItem>, lane: impl Into<String>) -> Self {
        Self::new(HarnessEventPayload::QueueUpdate { queues }, Some(lane.into()))
    }

    pub fn queued_entry_ids(&self) -> Vec<String> {
        match &self.payload {
            HarnessEventPayload::QueueUpdate { queues } => {
                queues.iter().map(|item| item.entry_id.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn config_update(property: &str, value: JsonValue, previous: JsonValue, lane: Option<String>) -> Self {
        Self::new(HarnessEventPayload::ConfigUpdate { property: property.to_owned(), value, previous }, lane)
    }

    pub fn value_update(value: &str, name: Option<String>, lane: Option<String>) -> Self {
        Self::new(HarnessEventPayload::ValueUpdate { value: value.to_owned(), name, target_id: None, label: None }, lane)
    }

    pub fn lane_created(at: Option<String>, lane: Option<String>) -> Self {
        Self::new(HarnessEventPayload::LaneCreated { at }, lane)
    }

    pub fn fault(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(HarnessEventPayload::Fault { code: code.into(), message: message.into() }, None)
    }
}

pub type UntypedEventListener = Arc<dyn Fn(HarnessEvent, Context) -> BoxFuture<'static, ()> + Send + Sync>;
pub type EventListener = UntypedEventListener;
pub type EventFilter = Arc<dyn Fn(&HarnessEvent) -> bool + Send + Sync>;
pub type ResnapshotCapture<T> =
    Arc<dyn Fn(Context, Arc<dyn Fn() + Send + Sync>) -> BoxFuture<'static, T> + Send + Sync>;
pub type SnapshotCapture<T> = Arc<dyn Fn(Context) -> BoxFuture<'static, T> + Send + Sync>;
pub type WatcherErrorFn = Arc<dyn Fn(String, HarnessEvent, Context) -> BoxFuture<'static, ()> + Send + Sync>;
pub type WatchListenerFilter = Arc<dyn Fn(&HarnessEvent) -> bool + Send + Sync>;

type Job = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

struct BusInner {
    listeners: Mutex<BTreeMap<String, Vec<UntypedEventListener>>>,
    watch_listeners: Mutex<Vec<UntypedEventListener>>,
    closed_error: Mutex<Option<String>>,
    queue: Mutex<VecDeque<Job>>,
    notify: Notify,
}

pub struct HarnessEventBus {
    inner: Arc<BusInner>,
}

impl Default for HarnessEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for HarnessEventBus {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl HarnessEventBus {
    pub fn new() -> Self {
        let inner = Arc::new(BusInner {
            listeners: Mutex::new(BTreeMap::new()),
            watch_listeners: Mutex::new(Vec::new()),
            closed_error: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        });
        let worker = inner.clone();
        tokio::spawn(async move {
            loop {
                let job = {
                    let mut queue = worker.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    queue.pop_front()
                };
                match job {
                    Some(job) => job().await,
                    None => worker.notify.notified().await,
                }
            }
        });
        Self { inner }
    }

    pub fn closed_error(&self) -> Option<String> {
        self.inner.closed_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub fn on(&self, event_type: &str, listener: UntypedEventListener) -> Unsubscribe {
        if let Some(error) = self.closed_error() {
            panic!("{error}");
        }
        self.inner
            .listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(event_type.to_owned())
            .or_default()
            .push(listener.clone());
        let inner = self.inner.clone();
        let event_type = event_type.to_owned();
        Unsubscribe::new(move || {
            let mut listeners = inner.listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = listeners.get_mut(&event_type) {
                entry.retain(|candidate| !Arc::ptr_eq(candidate, &listener));
            }
        })
    }

    pub async fn emit(&self, event: HarnessEvent, context: Context) {
        self.emit_batch(vec![event], context).await;
    }

    pub async fn emit_batch(&self, events: Vec<HarnessEvent>, context: Context) {
        self.begin_emit_batch(events, context).await;
    }

    /// Bind current recipients synchronously, then append one contiguous batch to the delivery tail.
    pub fn begin_emit_batch(&self, events: Vec<HarnessEvent>, context: Context) -> BoxFuture<'static, ()> {
        if self.closed_error().is_some() || events.is_empty() {
            return Box::pin(async {});
        }
        let bound: Vec<(HarnessEvent, Vec<UntypedEventListener>)> =
            events.into_iter().map(|event| (event.clone(), self.snapshot_recipients(&event))).collect();
        let inner = self.inner.clone();
        let (done_tx, done_rx) = oneshot::channel();
        self.push_job(Box::new(move || {
            Box::pin(async move {
                for (payload, recipients) in bound {
                    deliver(&inner, payload, &recipients, true, context.clone()).await;
                }
                let _ = done_tx.send(());
            })
        }));
        Box::pin(async move {
            let _ = done_rx.await;
        })
    }

    pub fn watch<T: Clone + Send + Sync + 'static>(
        &self,
        snapshot: T,
        filter: EventFilter,
        resnapshot: Option<ResnapshotCapture<T>>,
    ) -> Arc<BufferedEventWatcher<T>> {
        if let Some(error) = self.closed_error() {
            panic!("{error}");
        }
        self.install_watcher(Some(snapshot), filter, resnapshot)
    }

    pub async fn watch_from_snapshot<T: Clone + Send + Sync + 'static>(
        &self,
        capture: SnapshotCapture<T>,
        filter: EventFilter,
        context: Context,
    ) -> Arc<BufferedEventWatcher<T>> {
        if let Some(error) = self.closed_error() {
            panic!("{error}");
        }
        let capture_for_resnapshot = capture.clone();
        let resnapshot: ResnapshotCapture<T> = Arc::new(move |capture_context, mark_boundary| {
            let capture = capture_for_resnapshot.clone();
            Box::pin(async move {
                let snapshot = capture(capture_context).await;
                mark_boundary();
                snapshot
            })
        });
        let watcher = self.install_watcher(None, filter, Some(resnapshot));
        let snapshot = capture(context).await;
        watcher.set_snapshot(snapshot);
        watcher
    }

    pub fn close(&self, error: impl Into<String>) {
        {
            let mut closed = self.inner.closed_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if closed.is_none() {
                *closed = Some(error.into());
            }
        }
        let inner = self.inner.clone();
        self.push_job(Box::new(move || {
            Box::pin(async move {
                inner.listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
                inner.watch_listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
            })
        }));
    }

    fn push_job(&self, job: Job) {
        {
            let mut queue = self.inner.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            queue.push_back(job);
        }
        self.inner.notify.notify_waiters();
    }

    fn install_watcher<T: Clone + Send + Sync + 'static>(
        &self,
        snapshot: Option<T>,
        filter: EventFilter,
        resnapshot: Option<ResnapshotCapture<T>>,
    ) -> Arc<BufferedEventWatcher<T>> {
        let watcher = Arc::new(BufferedEventWatcher::new(snapshot, resnapshot));
        watcher.set_bus(self.clone());

        let on_error: WatcherErrorFn = {
            let bus = self.clone();
            Arc::new(move |error, event, context| {
                let bus = bus.clone();
                Box::pin(async move {
                    if event.event_type() == "handler_error" {
                        return;
                    }
                    let lane = event.lane.clone();
                    bus.emit(HarnessEvent::handler_error(error, "event", event.event_type(), lane), context).await;
                })
            })
        };
        watcher.set_on_error(on_error);

        let watch_listener: UntypedEventListener = {
            let watcher = watcher.clone();
            Arc::new(move |event, context| {
                let watcher = watcher.clone();
                let filter = filter.clone();
                Box::pin(async move {
                    if filter(&event) {
                        watcher.push(event, context);
                    }
                })
            })
        };
        self.inner
            .watch_listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(watch_listener.clone());
        let inner = self.inner.clone();
        watcher.set_unsubscribe(Arc::new(move || {
            inner.watch_listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).retain(|candidate| {
                !Arc::ptr_eq(candidate, &watch_listener)
            });
        }));
        watcher
    }

    fn snapshot_recipients(&self, event: &HarnessEvent) -> Vec<UntypedEventListener> {
        let mut recipients = Vec::new();
        {
            let listeners = self.inner.listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = listeners.get(event.event_type()) {
                recipients.extend(entry.iter().cloned());
            }
        }
        recipients.extend(
            self.inner.watch_listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().cloned(),
        );
        recipients
    }
}

async fn deliver(
    inner: &Arc<BusInner>,
    event: HarnessEvent,
    recipients: &[UntypedEventListener],
    report_errors: bool,
    context: Context,
) {
    for listener in recipients {
        let cloned = event.clone();
        let result = std::panic::AssertUnwindSafe(listener(cloned, context.clone())).catch_unwind().await;
        if let Err(payload) = result {
            if !report_errors || event.event_type() == "handler_error" {
                continue;
            }
            let message = panic_message(&payload);
            let lane = event.lane.clone();
            let handler_error = HarnessEvent::handler_error(message, "event", event.event_type(), lane);
            let recipients = snapshot_recipients_of(inner, &handler_error);
            Box::pin(deliver(inner, handler_error, &recipients, false, context.clone())).await;
        }
    }
}

fn snapshot_recipients_of(inner: &Arc<BusInner>, event: &HarnessEvent) -> Vec<UntypedEventListener> {
    let mut recipients = Vec::new();
    {
        let listeners = inner.listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = listeners.get(event.event_type()) {
            recipients.extend(entry.iter().cloned());
        }
    }
    recipients
        .extend(inner.watch_listeners.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().cloned());
    recipients
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    "unknown error".to_owned()
}

pub struct Unsubscribe {
    callback: Arc<dyn Fn() + Send + Sync>,
}

impl Unsubscribe {
    fn new(callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self { callback: Arc::new(callback) }
    }

    pub fn call(&self) {
        (self.callback)();
    }
}

struct WatcherInner<T> {
    snapshot: Mutex<Option<T>>,
    resnapshot_callback: Mutex<Option<ResnapshotCapture<T>>>,
    bus: Mutex<Option<HarnessEventBus>>,
    on_error: Mutex<Option<WatcherErrorFn>>,
    buffer: Mutex<Vec<(HarnessEvent, Context, u64)>>,
    listener: Mutex<Option<UntypedEventListener>>,
    unsubscribe_callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    queue: Mutex<VecDeque<(HarnessEvent, Context, u64)>>,
    notify: Notify,
    epoch: Mutex<u64>,
    resnapshot_state: Mutex<Option<ResnapshotState>>,
    boundary: Notify,
    state: Mutex<WatcherState>,
}

struct ResnapshotState {
    phase: ResnapshotPhase,
    held: Vec<(HarnessEvent, Context)>,
}

impl<T> WatcherInner<T> {
    fn mark_resnapshot_boundary(&self) {
        {
            let mut state = self.resnapshot_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(state) = state.as_mut().filter(|state| state.phase == ResnapshotPhase::Dropping) {
                state.phase = ResnapshotPhase::Holding;
            }
        }
        self.boundary.notify_waiters();
    }
}

#[derive(PartialEq)]
enum ResnapshotPhase {
    Dropping,
    Holding,
}

#[derive(PartialEq, Clone, Copy)]
enum WatcherState {
    Buffering,
    Started,
    Unsubscribed,
}

pub struct BufferedEventWatcher<T> {
    inner: Arc<WatcherInner<T>>,
}

impl<T: Clone + Send + Sync + 'static> BufferedEventWatcher<T> {
    fn new(snapshot: Option<T>, resnapshot_callback: Option<ResnapshotCapture<T>>) -> Self {
        let inner = Arc::new(WatcherInner {
            snapshot: Mutex::new(snapshot),
            resnapshot_callback: Mutex::new(resnapshot_callback),
            bus: Mutex::new(None),
            on_error: Mutex::new(None),
            buffer: Mutex::new(Vec::new()),
            listener: Mutex::new(None),
            unsubscribe_callback: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            epoch: Mutex::new(0),
            resnapshot_state: Mutex::new(None),
            boundary: Notify::new(),
            state: Mutex::new(WatcherState::Buffering),
        });
        let worker = inner.clone();
        tokio::spawn(async move {
            loop {
                let job = {
                    let mut queue = worker.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    queue.pop_front()
                };
                match job {
                    Some((event, context, epoch)) => deliver_watcher(&worker, event, context, epoch).await,
                    None => worker.notify.notified().await,
                }
            }
        });
        Self { inner }
    }

    pub fn snapshot(&self) -> T {
        self.inner
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("watch snapshot was not set")
    }

    fn set_snapshot(&self, snapshot: T) {
        *self.inner.snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(snapshot);
    }

    fn set_bus(&self, bus: HarnessEventBus) {
        *self.inner.bus.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(bus);
    }

    fn set_on_error(&self, on_error: WatcherErrorFn) {
        *self.inner.on_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(on_error);
    }

    fn state(&self) -> WatcherState {
        *self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub async fn start(&self, listener: UntypedEventListener) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != WatcherState::Buffering {
                panic!("WatchHandle.start() may be called only once");
            }
            *state = WatcherState::Started;
        }
        *self.inner.listener.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(listener);
        let buffered = {
            let mut buffer = self.inner.buffer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *buffer)
        };
        for (event, context, epoch) in buffered {
            self.enqueue(event, context, epoch);
        }
    }

    pub async fn resnapshot(&self, context: Context) -> T {
        if self.state() == WatcherState::Unsubscribed {
            panic!("WatchHandle is unsubscribed");
        }
        let raw = self
            .inner
            .resnapshot_callback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("WatchHandle does not support resnapshot");
        let bus = self.inner.bus.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let weak = Arc::downgrade(&self.inner);
        let marked = Arc::new(AtomicBool::new(false));
        let mark: Arc<dyn Fn() + Send + Sync> = {
            let marked = marked.clone();
            Arc::new(move || {
                if marked.swap(true, Ordering::SeqCst) {
                    panic!("Resnapshot boundary was already marked");
                }
                let weak = weak.clone();
                if let Some(bus) = bus.clone() {
                    bus.push_job(Box::new(move || {
                        Box::pin(async move {
                            if let Some(watcher) = weak.upgrade() {
                                watcher.mark_resnapshot_boundary();
                            }
                        })
                    }));
                }
            })
        };
        {
            let mut state = self.inner.resnapshot_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.is_some() {
                panic!("WatchHandle resnapshot is already in progress");
            }
            *state = Some(ResnapshotState { phase: ResnapshotPhase::Dropping, held: Vec::new() });
        }
        {
            let mut epoch = self.inner.epoch.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *epoch += 1;
        }
        let snapshot = raw(context, mark).await;
        if !marked.load(Ordering::SeqCst) {
            panic!("Resnapshot capture did not mark its boundary");
        }
        loop {
            {
                let state = self.inner.resnapshot_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let holding = match state.as_ref() {
                    None => true,
                    Some(state) => state.phase == ResnapshotPhase::Holding,
                };
                if holding {
                    break;
                }
            }
            self.inner.boundary.notified().await;
        }
        self.set_snapshot(snapshot.clone());
        let held = {
            let mut state = self.inner.resnapshot_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.take().map(|state| state.held).unwrap_or_default()
        };
        for (event, context) in held {
            self.push(event, context);
        }
        snapshot
    }



    pub fn unsubscribe(&self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state == WatcherState::Unsubscribed {
                return;
            }
            *state = WatcherState::Unsubscribed;
        }
        self.inner.buffer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
        *self.inner.listener.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let callback = self.inner.unsubscribe_callback.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(callback) = callback {
            callback();
        }
    }

    fn set_unsubscribe(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self.inner.unsubscribe_callback.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(callback);
    }

    fn push(&self, event: HarnessEvent, context: Context) {
        if self.state() == WatcherState::Unsubscribed {
            return;
        }
        {
            let mut state = self.inner.resnapshot_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(state) = state.as_mut() {
                match state.phase {
                    ResnapshotPhase::Dropping => return,
                    ResnapshotPhase::Holding => {
                        state.held.push((event, context));
                        return;
                    }
                }
            }
        }
        if self.state() == WatcherState::Buffering {
            let epoch = *self.inner.epoch.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner
                .buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((event, context, epoch));
            return;
        }
        let epoch = *self.inner.epoch.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.enqueue(event, context, epoch);
    }

    fn enqueue(&self, event: HarnessEvent, context: Context, epoch: u64) {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back((event, context, epoch));
        self.inner.notify.notify_waiters();
    }
}

async fn deliver_watcher<T: Clone + Send + Sync + 'static>(
    inner: &Arc<WatcherInner<T>>,
    event: HarnessEvent,
    context: Context,
    epoch: u64,
) {
    if *inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) != WatcherState::Started {
        return;
    }
    let current = *inner.epoch.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if epoch != current {
        return;
    }
    let listener = inner.listener.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let Some(listener) = listener else {
        return;
    };
    let result = std::panic::AssertUnwindSafe(listener(event.clone(), context.clone())).catch_unwind().await;
    if let Err(payload) = result {
        let on_error = inner.on_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if let Some(on_error) = on_error {
            let _ = std::panic::AssertUnwindSafe(on_error(panic_message(&payload), event, context))
                .catch_unwind()
                .await;
        }
    }
}
