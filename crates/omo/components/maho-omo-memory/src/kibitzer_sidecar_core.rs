//! One resident sidecar's shared state record (latest `kibitzer/sidecar-core.ts`).
//!
//! Everything `createKibitzerSidecar` keeps for the lifetime of a bound session lives on this record
//! so the lifecycle modules (`sidecar-turn`, `sidecar-admission`, `sidecar-recovery`, `sidecar-wake`)
//! can read and advance it. The record adds no locking of its own: every transition still runs
//! through the per-session chain `serialized`.
//!
//! # Scope
//!
//! This module ports ONLY the state record, the queued serialization (`serialized`) and the idle
//! signal (`when_idle`) - `sidecar-core.ts` and nothing else. Admission, wake triggering, deadline
//! callbacks, recovery and the public composition are their own upstream files and later bounded
//! owners; this record exposes exactly the fields those owners advance, and none of their behaviour
//! is stubbed here.
//!
//! # Deviations (each an N/A, because the Rust crate has no direct counterpart)
//!
//! * The per-session mutex is upstream's PROMISE CHAIN, not a lock: `serialized` runs each queued
//!   transition after the preceding one REGARDLESS of whether it failed, and every caller still
//!   receives its own failure. The port uses a FIFO [`tokio::sync::Mutex`] as the chain, so a
//!   transition returning `Err` releases the chain and the next queued transition runs while the
//!   caller keeps its own error. The guard is an ASYNC mutex guard, never a `std::sync::MutexGuard`,
//!   so no std guard is ever held across an `.await`.
//! * The live `offered` / `surfaced` / `searched` membership sets and the current-wake `accepted` /
//!   `budget` slots are NOT duplicated here: they are the ONE registry-provided
//!   [`KibitzerSessionResources`] handle carried on the options. Upstream keeps
//!   `offered`/`surfaced`/`accepted`/`budget` on this record and `searchedPaths` on the child's tool
//!   object; the Rust producer unifies them, so this record keeps only its own `offered_at_wake` and
//!   `delivered` sets.
//! * `options.logger?.warn(message, { sessionId, ...details })` becomes the crate's plain
//!   `warn: Arc<dyn Fn(&str) + Send + Sync>`; the structured object is rendered as a JSON suffix,
//!   because the Rust crate has no `ComponentLogger` port.
//! * `AbortController` becomes [`AdmissionCancel`], a cancellation flag the wake-slot wait polls; the
//!   default `now`/`random` come from the system clock, because the crate has no `Math.random`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memory_core::recall::{RecallCandidate, RecallNudge};
use serde_json::Value;

use crate::kibitzer_child::KibitzerChild;
use crate::kibitzer_contract::{
    KibitzerSidecarState, KibitzerSidecarTimers, KibitzerToolBudget, KIBITZER_RESEED_FRACTION,
    KIBITZER_SIDECAR_MAX_TOKENS, KIBITZER_WAKE_DEADLINE_MS, KIBITZER_WAKE_TOOL_BUDGET, WarnFn,
};
use crate::kibitzer_events::{
    create_kibitzer_event_stream, KibitzerEvent, KibitzerEventCaps, KibitzerEventStream,
};
use crate::kibitzer_prompt::KibitzerSidecarDigest;
use crate::kibitzer_prompt_blocks::KibitzerFieldCaps;
use crate::kibitzer_session_resources::KibitzerSessionResources;
use crate::kibitzer_sidecar_outcome::{KibitzerCursorSpan, KibitzerWakeAbort, KibitzerWakeUsage};
use crate::kibitzer_wake_policy::AcceptedNudgeCooldown;
use crate::kibitzer_wake_slot::KibitzerWakeLease;

/// The injectable configuration the state record reads: the exact subset of the public sidecar
/// options `createSidecarCore` uses. The composition owner projects the public options onto this;
/// the record never reads the child factory, the delivery callback, the wake slot or the wake
/// runner, which belong to the later units.
#[derive(Clone)]
pub struct KibitzerSidecarCoreOptions {
    pub session_id: String,
    /// The ONE registry-provided live-resource handle: `offered` / `surfaced` / `searched`
    /// membership and the current-wake `accepted` / `budget` slots. The record never copies these;
    /// it stores this clone (which shares the slots and live sets) and never builds its own.
    pub resources: KibitzerSessionResources,
    /// `memory.recall.tool_budget`; `KIBITZER_WAKE_TOOL_BUDGET` when absent.
    pub tool_budget: Option<usize>,
    /// The quiet period a steer re-arms; `KIBITZER_WAKE_DEADLINE_MS` when absent.
    pub wake_deadline_ms: Option<i64>,
    /// `memory.recall.sidecar_max_tokens`; `KIBITZER_SIDECAR_MAX_TOKENS` when absent.
    pub sidecar_max_tokens: Option<i64>,
    /// `memory.recall.event_caps`; the stream defaults when absent.
    pub event_caps: Option<KibitzerEventCaps>,
    /// The clock; the system clock when absent.
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// The jitter source; a system-clock-derived source when absent.
    pub random: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    /// The injectable timers; the runtime's unref'd timers when absent.
    pub timers: Option<Arc<dyn KibitzerSidecarTimers>>,
    /// `options.logger?.warn`; absent means the record stays silent.
    pub warn: Option<WarnFn>,
}

/// Events and candidates one envelope carried; kept until the child is known to have read them.
#[derive(Clone, Debug, PartialEq)]
pub struct Payload {
    pub events: Vec<KibitzerEvent>,
    pub digest: Option<KibitzerSidecarDigest>,
    pub candidates: Vec<RecallCandidate>,
    pub cursors: Option<KibitzerCursorSpan>,
}

/// One message the sidecar sent; `consumed` is set by the child's own `message_end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub text: String,
    pub payload: Payload,
    pub steered: bool,
    /// Confirmed through the child's own `message_end` for the user message carrying `text`.
    pub consumed: bool,
}

/// The child's own event subscriptions, drained once on disposal.
pub type Unsubscribes = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

/// The resident child one sidecar keeps for a bound session (upstream `Child`).
///
/// `tools` / `searchedPaths` are NOT duplicated here: the child's nudge-eligible searched set is the
/// shared [`KibitzerSessionResources::searched`] field, and the member tools are built by the CLI
/// child factory. `usage_tokens` and `chars_sent` are STORED here, but no unit populates them yet -
/// the child ABI DOES expose the observation channel
/// (`KibitzerChild::subscribe_observations` -> `KibitzerChildObservation::MessageEnd { message }`,
/// carrying the native `AgentMessage`), so the source of consumption and provider usage exists; what
/// remains is the CONSUMER WIRING in the turn unit's `observe`, which projects that `MessageEnd` into
/// `Envelope.consumed`, `usage_tokens` and `usage`. Until that wiring lands, a stored value is not a
/// wired one.
///
/// `Child` is `Clone` so the wake can take a SNAPSHOT and report OUTSIDE the record lock (the
/// `onWake` callback may reenter the sidecar, and a record lock held across the call would
/// deadlock). A clone shares every mutable field through `Arc`, so all snapshots see the same
/// counters and the same unsubscribe list.
#[derive(Clone)]
pub struct Child {
    pub handle: Arc<dyn KibitzerChild>,
    pub generation: u64,
    /// The child's own event subscriptions; called on disposal so a torn-down child stops reporting.
    /// `Arc`-shared so a clone taken to report OUTSIDE the record lock owns the SAME list: draining
    /// it once in `dispose_child` runs each unsubscribe exactly once, never once per snapshot.
    pub unsubscribes: Unsubscribes,
    /// `usage.input + usage.cacheRead` of the newest assistant message; `None` until usage is seen.
    /// `Arc`-shared: every snapshot reads the same counter.
    pub usage_tokens: Arc<Mutex<Option<i64>>>,
    /// Every character the sidecar sent, for the char/4 fallback.
    /// `Arc`-shared.
    pub chars_sent: Arc<Mutex<i64>>,
}

/// One tracked wake (upstream `Turn`): the seed/followUp that opened it plus every steer it absorbed.
pub struct Turn {
    pub wake: u64,
    pub generation: u64,
    pub max_items: usize,
    /// Set at admission, before any child I/O: the wake's whole life is measured from here.
    pub started_at: i64,
    /// `started_at + KIBITZER_WAKE_MAX_TOTAL_MS`: the ceiling no steer re-arm may push past.
    pub total_deadline_at: i64,
    /// The CURRENT wake's accepted-nudge list, captured from `resources.accepted.current()` at
    /// admission; a steer keeps it.
    pub accepted: Arc<Mutex<Vec<RecallNudge>>>,
    /// The CURRENT wake's tool budget, captured from `resources.budget_slot.current()` at admission.
    pub budget: Arc<dyn KibitzerToolBudget>,
    pub envelopes: Mutex<Vec<Envelope>>,
    /// The machine-wide lease this turn holds; `None` once released.
    pub lease: Mutex<Option<KibitzerWakeLease>>,
    /// Time the wake spent waiting for its lease.
    pub slot_wait_ms: AtomicI64,
    pub candidate_count: AtomicUsize,
    pub tool_starts: AtomicUsize,
    pub tool_ends: AtomicUsize,
    pub abort: Mutex<Option<KibitzerWakeAbort>>,
    /// The armed deadline timer handle, if any.
    pub deadline: Mutex<Option<u64>>,
    /// The deadline epoch. A deadline callback captures the epoch it was armed under and no-ops
    /// unless it still matches: the arming path bumps this BEFORE it registers the timer, and
    /// `clear_deadline` bumps it BEFORE it clears the handle, so an immediate or superseded callback
    /// (and one racing a clear) can never abort a turn whose deadline has since been cleared or
    /// re-armed.
    pub deadline_epoch: AtomicU64,
    pub model: Mutex<Option<String>>,
    pub usage: Mutex<Option<KibitzerWakeUsage>>,
    /// Resolved when the turn settles; `when_idle` awaits it after the queue drains.
    pub settled: Arc<Settlement>,
}

/// `Omit<KibitzerReseedInput, "maxItems" | "toolBudget">`: the state a restart must not lose, owned
/// so the record can hold it; the prompt renderer's borrowed input is built from this at reseed.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingReseed {
    pub session_id: String,
    pub last_cursor: usize,
    pub task_summary: String,
    pub rejected_paths: Vec<String>,
    pub delivered_paths: Vec<String>,
    pub caps: KibitzerFieldCaps,
    pub max_chars: Option<usize>,
}

/// The settlement signal of one turn (upstream `Turn.settled`): resolves once the turn has ended.
pub struct Settlement {
    done: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Settlement {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { done: AtomicBool::new(false), notify: tokio::sync::Notify::new() })
    }

    /// Marks the turn settled and wakes every waiter. Idempotent.
    pub fn settle(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_settled(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Resolves once settled. Register-before-check, so a settle racing the wait is never missed.
    pub async fn wait(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.done.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

/// The `AbortController` port: the cancellation FLAG the in-flight wake-slot wait polls. Setting it
/// does NOT itself wake a parked wait - the wait observes the flag at its next poll - so the
/// responsive acquisition (waking the wait the moment the flag flips) is the admission / wake-slot
/// unit's obligation, not this flag's.
#[derive(Clone)]
pub struct AdmissionCancel {
    flag: Arc<AtomicBool>,
}

impl AdmissionCancel {
    pub fn new() -> Self {
        Self { flag: Arc::new(AtomicBool::new(false)) }
    }

    pub fn abort(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// The raw signal, for a `KibitzerWakeSlot::acquire(cancellation)` closure.
    pub fn signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

impl Default for AdmissionCancel {
    fn default() -> Self {
        Self::new()
    }
}

/// The record's mutable half: every field a transition advances. Lock it for short, non-await
/// sections only (clone the handles out, drop the guard, then await).
pub struct SidecarRecord {
    pub state: KibitzerSidecarState,
    pub child: Option<Child>,
    pub active_turn: Option<Arc<Turn>>,
    pub wake_seq: u64,
    pub generations: u64,
    pub consecutive_failures: u64,
    pub backoff_timer: Option<u64>,
    /// The lease wait in flight, if any; shutdown aborts it instead of queueing behind it.
    pub admission: Option<AdmissionCancel>,
    pub pending_reseed: Option<PendingReseed>,
    pub task_summary: Option<String>,
    /// Payloads no child has confirmed reading: replayed by the next envelope, oldest first.
    pub carry: Vec<Payload>,
    /// Wake number each offered path was last offered at (`offered` itself lives on `resources`).
    pub offered_at_wake: BTreeMap<String, u64>,
    /// Paths already delivered to the parent; never nudged again.
    pub delivered: BTreeSet<String>,
}

/// One sidecar's shared state (upstream `SidecarCore`). Not `Clone`: it is shared as `Arc`.
pub struct SidecarCore {
    pub session_id: String,
    pub options: Arc<KibitzerSidecarCoreOptions>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub timers: Arc<dyn KibitzerSidecarTimers>,
    pub tool_budget: usize,
    pub wake_deadline_ms: i64,
    pub reseed_at_tokens: i64,
    pub cooldown: Mutex<AcceptedNudgeCooldown>,
    pub stream: Mutex<KibitzerEventStream>,
    /// The ONE registry-provided live-resource handle: `offered` / `surfaced` / `searched`
    /// membership and the current-wake `accepted` / `budget` slots. The record never copies these.
    pub resources: KibitzerSessionResources,
    /// Set the instant shutdown is requested, BEFORE it reaches the chain: no wake may start after it.
    pub closing: AtomicBool,
    /// The per-session chain: every transition runs through it, in order.
    chain: tokio::sync::Mutex<()>,
    /// The mutable record; lock it for short, non-await sections only.
    pub record: Mutex<SidecarRecord>,
}

impl SidecarCore {
    /// Runs `task` as the next queued transition, after the preceding one whatever its result, and
    /// returns `task`'s own value to THIS caller. A transition returning `Err` releases the chain, so
    /// the queue keeps going; nothing is swallowed and nothing is retried. Do not call `serialized`
    /// from inside a transition (it would deadlock on the chain), and do not call `when_idle` from
    /// inside one either.
    pub async fn serialized<F, Fut, T>(&self, task: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = self.chain.lock().await;
        task().await
    }

    /// Resolves once every queued transition has run AND the running turn, if any, has settled - not
    /// merely when the state is `Idle`. It re-checks after each settlement, so a turn started by a
    /// transition that ran while this waited is awaited too.
    pub async fn when_idle(&self) {
        loop {
            // Wait behind every transition currently queued or running (the FIFO chain), then release.
            {
                let _guard = self.chain.lock().await;
            }
            let turn = {
                let record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
                record.active_turn.clone()
            };
            match turn {
                None => return,
                Some(turn) => turn.settled.wait().await,
            }
        }
    }

    /// `options.logger?.warn(message, { sessionId, ...details })`: silent when no warn sink is set.
    pub fn warn(&self, message: &str, details: Option<Value>) {
        let Some(warn) = &self.options.warn else { return };
        match details {
            Some(details) => warn(&format!("{message} {details}")),
            None => warn(message),
        }
    }

    /// Marks shutdown requested. Call this (and [`Self::abort_admission`]) SYNCHRONOUSLY, before the
    /// queued shutdown transition, so a parked wake-slot wait observes the cancellation at its next
    /// poll instead of after the bounded wait.
    pub fn mark_closing(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> KibitzerSidecarState {
        self.record.lock().unwrap_or_else(PoisonError::into_inner).state
    }

    /// `Disposed` is TERMINAL for the bound session: a late offer must never create another child.
    pub fn is_disposed(&self) -> bool {
        self.state() == KibitzerSidecarState::Disposed
    }

    /// Registers the lease wait in flight and hands back its cancel flag; the admission unit passes
    /// [`AdmissionCancel::signal`] to the wake slot's `acquire`.
    pub fn begin_admission(&self) -> AdmissionCancel {
        let cancel = AdmissionCancel::new();
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        record.admission = Some(cancel.clone());
        cancel
    }

    /// Clears the lease wait once it returned.
    pub fn end_admission(&self) {
        let mut record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
        record.admission = None;
    }

    /// Aborts the in-flight lease wait, if any. Does not take the chain, so shutdown can call it
    /// before queueing its transition.
    pub fn abort_admission(&self) {
        let cancel = {
            let record = self.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.admission.clone()
        };
        if let Some(cancel) = cancel {
            cancel.abort();
        }
    }
}

/// Builds the per-session core (upstream `createSidecarCore`), applying the pinned defaults.
pub fn create_sidecar_core(options: KibitzerSidecarCoreOptions) -> Arc<SidecarCore> {
    let session_id = options.session_id.clone();
    let now = options.now.clone().unwrap_or_else(default_now);
    let random = options.random.clone().unwrap_or_else(default_random);
    let timers = options
        .timers
        .clone()
        .unwrap_or_else(|| Arc::new(RuntimeTimers::new()) as Arc<dyn KibitzerSidecarTimers>);
    let tool_budget = options.tool_budget.unwrap_or(KIBITZER_WAKE_TOOL_BUDGET);
    let wake_deadline_ms = options.wake_deadline_ms.unwrap_or(KIBITZER_WAKE_DEADLINE_MS);
    let sidecar_max_tokens = options.sidecar_max_tokens.unwrap_or(KIBITZER_SIDECAR_MAX_TOKENS);
    let reseed_at_tokens = ((sidecar_max_tokens as f64) * KIBITZER_RESEED_FRACTION).floor() as i64;
    let cooldown = AcceptedNudgeCooldown::new(Arc::clone(&now));
    let stream = create_kibitzer_event_stream(options.event_caps.unwrap_or_default());
    let resources = options.resources.clone();
    Arc::new(SidecarCore {
        session_id,
        options: Arc::new(options),
        now,
        random,
        timers,
        tool_budget,
        wake_deadline_ms,
        reseed_at_tokens,
        cooldown: Mutex::new(cooldown),
        stream: Mutex::new(stream),
        resources,
        closing: AtomicBool::new(false),
        chain: tokio::sync::Mutex::new(()),
        record: Mutex::new(SidecarRecord {
            state: KibitzerSidecarState::Idle,
            child: None,
            active_turn: None,
            wake_seq: 0,
            generations: 0,
            consecutive_failures: 0,
            backoff_timer: None,
            admission: None,
            pending_reseed: None,
            task_summary: None,
            carry: Vec::new(),
            offered_at_wake: BTreeMap::new(),
            delivered: BTreeSet::new(),
        }),
    })
}

/// The default timers: the runtime's timers, spawned on the host executor. `set` must run inside a
/// tokio runtime (the production host has one); `clear` aborts a pending timer.
pub struct RuntimeTimers {
    next: AtomicU64,
    handles: Arc<Mutex<BTreeMap<u64, tokio::task::JoinHandle<()>>>>,
}

impl RuntimeTimers {
    pub fn new() -> Self {
        Self { next: AtomicU64::new(0), handles: Arc::new(Mutex::new(BTreeMap::new())) }
    }
}

impl Default for RuntimeTimers {
    fn default() -> Self {
        Self::new()
    }
}

impl KibitzerSidecarTimers for RuntimeTimers {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, ms: i64) -> u64 {
        let handle = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let delay = Duration::from_millis(ms.max(0) as u64);
        // Register BEFORE the spawned task can run its removal: take the guard first, insert the
        // join handle, then drop the guard. A zero-delay task that removes before this insertion
        // would otherwise leave a finished handle stored forever.
        let mut handles = self.handles.lock().unwrap_or_else(PoisonError::into_inner);
        let task_handles = Arc::clone(&self.handles);
        let join = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // Deregister first, then run the callback WITHOUT holding the guard (the lock is a
            // statement-scoped temporary, released before `callback()`).
            task_handles.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle);
            callback();
        });
        handles.insert(handle, join);
        drop(handles);
        handle
    }

    fn clear(&self, handle: u64) {
        if let Some(join) = self.handles.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle) {
            join.abort();
        }
    }
}

fn default_now() -> Arc<dyn Fn() -> i64 + Send + Sync> {
    Arc::new(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or(0))
}

fn default_random() -> Arc<dyn Fn() -> f64 + Send + Sync> {
    Arc::new(|| {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.subsec_nanos()).unwrap_or(0);
        (nanos as f64) / 1_000_000_000.0
    })
}

/// Upstream `textOf`: a string is itself; an array contributes only its `type: "text"` block texts,
/// concatenated with no separator; anything else is empty.
pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let block = block.as_object()?;
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                block.get("text").and_then(Value::as_str)
            })
            .collect::<String>(),
        _ => String::new(),
    }
}

/// Upstream `isRecord`: an object, never an array or a scalar.
pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

/// Upstream `numberOf`: a finite JSON number, else `None`.
pub fn number_of(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

/// Upstream `describe`: the error's own message.
pub fn describe(error: &dyn std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;

    use crate::kibitzer_child::{JudgeSettle, KibitzerChildObservation};

    /// A no-op child handle: the shared-snapshot contract under test is about the `Child` wrapper,
    /// not about driving a real child, so every native method is inert here.
    struct StubChild;

    impl KibitzerChild for StubChild {
        fn steer<'a>(&'a self, _text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
        fn follow_up<'a>(&'a self, _text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
        fn abort(&self) {}
        fn subscribe_nudges(&self, _listener: Arc<dyn Fn(RecallNudge) + Send + Sync>) -> Box<dyn FnOnce() + Send> {
            Box::new(|| {})
        }
        fn subscribe_observations(
            &self,
            _listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>,
        ) -> Box<dyn FnOnce() + Send> {
            Box::new(|| {})
        }
        fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>> {
            Box::pin(async { JudgeSettle { completed: true, cancelled: false, failure_message: None } })
        }
        fn dispose<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A child with no armed unsubscribes.
    fn fixture_child() -> Child {
        fixture_child_with_unsubscribes(Vec::new())
    }

    /// A child carrying `unsubscribes` on the shared `Arc`, exactly as the seed constructor builds it.
    fn fixture_child_with_unsubscribes(unsubscribes: Vec<Box<dyn FnOnce() + Send>>) -> Child {
        Child {
            handle: Arc::new(StubChild),
            generation: 1,
            unsubscribes: Arc::new(Mutex::new(unsubscribes)),
            usage_tokens: Arc::new(Mutex::new(None)),
            chars_sent: Arc::new(Mutex::new(0)),
        }
    }

    #[test]
    fn given_a_child_when_cloned_then_every_shared_handle_is_the_same_allocation() {
        let child = fixture_child();
        let snapshot = child.clone();

        assert!(Arc::ptr_eq(&child.handle, &snapshot.handle), "the handle is shared");
        assert!(Arc::ptr_eq(&child.unsubscribes, &snapshot.unsubscribes), "the unsubscribe list is shared");
        assert!(Arc::ptr_eq(&child.usage_tokens, &snapshot.usage_tokens), "the usage counter is shared");
        assert!(Arc::ptr_eq(&child.chars_sent, &snapshot.chars_sent), "the char counter is shared");
        assert_eq!(child.generation, snapshot.generation);
    }

    #[test]
    fn given_a_clone_when_a_counter_is_written_then_the_other_snapshot_observes_it() {
        let child = fixture_child();
        let snapshot = child.clone();

        *snapshot.usage_tokens.lock().unwrap_or_else(PoisonError::into_inner) = Some(1_234);
        assert_eq!(*child.usage_tokens.lock().unwrap_or_else(PoisonError::into_inner), Some(1_234));

        *child.chars_sent.lock().unwrap_or_else(PoisonError::into_inner) = 7;
        assert_eq!(*snapshot.chars_sent.lock().unwrap_or_else(PoisonError::into_inner), 7);
    }

    #[test]
    fn given_a_clone_when_the_shared_unsubscribes_are_drained_then_each_runs_exactly_once() {
        let runs = Arc::new(AtomicUsize::new(0));
        let first: Box<dyn FnOnce() + Send> = {
            let runs = Arc::clone(&runs);
            Box::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
            })
        };
        let second: Box<dyn FnOnce() + Send> = {
            let runs = Arc::clone(&runs);
            Box::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
            })
        };
        let child = fixture_child_with_unsubscribes(vec![first, second]);
        let snapshot = child.clone();

        // `dispose_child` drains the SHARED list once (`std::mem::take` over the same `Arc`).
        let drained = std::mem::take(&mut *snapshot.unsubscribes.lock().unwrap_or_else(PoisonError::into_inner));
        assert_eq!(drained.len(), 2);
        for unsubscribe in drained {
            unsubscribe();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2, "each unsubscribe runs exactly once");

        // The original observes the SAME list, already drained: a second drain is empty, so no
        // `FnOnce` can run again and no unsubscribe action is duplicated.
        let again = std::mem::take(&mut *child.unsubscribes.lock().unwrap_or_else(PoisonError::into_inner));
        assert!(again.is_empty(), "a second drain of the shared list is empty");
        assert_eq!(runs.load(Ordering::SeqCst), 2, "no unsubscribe runs twice across snapshots");
    }
}
