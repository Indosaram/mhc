//! Deterministic shutdown-lifecycle regressions for the resident recall wiring.
//!
//! Every case drives the REAL `MemoryRecallWiring` hook surface. The fakes exist only to make the
//! lifecycle observable: a `ManualSpawn` queue (the scheduled cleanup runs exactly when the test
//! drains it, never on a real executor), a counting child spawner (a disposed sidecar must never
//! reach it), and a coordinator whose `remove` observer reads the REAL sidecar state. No sleep, no
//! thread, no runtime: every shutdown future here resolves on the FIRST poll (uncontended), and the
//! clock is fixed - a Pending result is treated as a defect, not a timing budget.

use super::*;

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use memory_core::identity::layout::build_identity_paths;
use memory_core::recall::{RecallCandidate, RecallNudge};

use crate::binding::MemorySessionBinding;
use crate::kibitzer_child::{JudgeSettle, KibitzerChild, KibitzerChildObservation, KibitzerChildSpawnInput};
use crate::kibitzer_contract::{KibitzerBufferedReason, KibitzerOfferResult, KibitzerSidecarState};
use crate::kibitzer_delivery::KibitzerCoordinatorEntry;
use crate::kibitzer_session_resources::KibitzerSessionResourceRegistry;
use crate::kibitzer_sidecar_admission::{BlockingAcquire, BlockingAcquireFuture};
use crate::kibitzer_sidecar_model::KibitzerSidecarStartError;

/// The session every case binds.
const SESSION: &str = "s1";

/// One poll with a no-op waker; the caller decides whether Pending is expected.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    use std::task::{Context as TaskContext, Waker};
    let waker = Waker::noop();
    let mut context = TaskContext::from_waker(&waker);
    future.poll(&mut context)
}

/// Polls an owned boxed future EXACTLY ONCE and asserts it is Ready. The shutdown lifecycle's
/// futures are uncontended (`serialized` is an uncontended `tokio::sync::Mutex`, and with no active
/// child or turn `shutdown()` has no real await), so a Pending result is a DEFECT - never a timing
/// budget. There is no spin, no retry and no polling luck.
fn poll_ready_once<F: Future + ?Sized>(mut future: Pin<Box<F>>) -> F::Output {
    match poll_once(future.as_mut()) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("a shutdown-lifecycle future must resolve on the FIRST poll; Pending is a defect"),
    }
}

/// The scheduled-cleanup queue: `spawn` queues, `drain` runs, and nothing runs until then.
#[derive(Default)]
struct ManualSpawn {
    queue: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>,
}

impl ManualSpawn {
    fn spawner(self: &Arc<Self>) -> KibitzerWakeSpawn {
        let me = Arc::clone(self);
        Arc::new(move |future| me.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(future))
    }

    fn queued(&self) -> usize {
        self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    fn drain(&self) {
        loop {
            let next = { self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop() };
            match next {
                Some(future) => {
                    let _ = poll_ready_once(future);
                }
                None => break,
            }
        }
    }
}

/// A counting spawner: a disposed sidecar must never reach it.
#[derive(Default)]
struct CountingSpawner {
    calls: AtomicUsize,
}

impl KibitzerChildSpawner for CountingSpawner {
    fn spawn<'a>(&'a self, _input: KibitzerChildSpawnInput) -> Pin<Box<dyn Future<Output = Result<Arc<dyn KibitzerChild>, KibitzerSidecarStartError>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Arc::new(StubChild) as Arc<dyn KibitzerChild>) })
    }
}

/// An inert child: the shutdown lifecycle under test never drives a real turn.
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
    fn subscribe_observations(&self, _listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>> {
        Box::pin(async { JudgeSettle { completed: true, cancelled: false, failure_message: None } })
    }
    fn dispose<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// A blocking executor that must never run: shutdown never acquires the wake lease.
struct PanicExecutor;

impl KibitzerBlockingExecutor for PanicExecutor {
    fn run_blocking(&self, _task: BlockingAcquire) -> BlockingAcquireFuture {
        panic!("the blocking executor must not run in the shutdown lifecycle unit")
    }
}

/// Inert timers: no timer is ever fired in these cases.
struct InertTimers;

impl KibitzerSidecarTimers for InertTimers {
    fn set(&self, _callback: Box<dyn FnOnce() + Send>, _ms: i64) -> u64 {
        0
    }
    fn clear(&self, _handle: u64) {}
}

/// Inert pending handoff: the pending write is not asserted in these cases.
struct InertPending;

impl KibitzerPendingPort for InertPending {
    fn write(&self, _session_id: &str, _nudges: &[RecallNudge]) -> std::io::Result<()> {
        Ok(())
    }
    fn delete(&self, _session_id: &str) {}
}

/// Inert prompt-drain pending port (never driven here).
struct InertPendingTake;

impl PendingNudgesPort for InertPendingTake {
    fn take(&self, _session_id: &str) -> Vec<RecallNudge> {
        Vec::new()
    }
}

/// The idle coordinator, recorded. Its `remove` observer reads the REAL sidecar state AT THE MOMENT
/// delivery retracts, so it proves shutdown completed first.
#[derive(Default)]
struct RecordingCoordinator {
    enqueued: Mutex<Vec<String>>,
    removed: Mutex<Vec<(String, KibitzerSidecarState)>>,
    sidecar: Mutex<Option<Arc<KibitzerSidecar>>>,
}

impl KibitzerIdleCoordinator for RecordingCoordinator {
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool {
        self.enqueued.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(entry.key);
        true
    }
    fn remove(&self, key: &str) {
        let observed = self
            .sidecar
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|sidecar| sidecar.state())
            .unwrap_or(KibitzerSidecarState::Idle);
        self.removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((key.to_string(), observed));
    }
}

struct Harness {
    wiring: Arc<MemoryRecallWiring>,
    spawner: Arc<CountingSpawner>,
    spawn: Arc<ManualSpawn>,
    coordinator: Option<Arc<RecordingCoordinator>>,
    context: MemoryIdentityContext,
    _root: tempfile::TempDir,
}

/// Builds the real wiring over fakes. `resolve_available` models a live vs. a pruned context;
/// `with_coordinator` installs the recording idle coordinator.
fn harness(resolve_available: bool, with_coordinator: bool) -> Harness {
    let root = tempfile::tempdir().unwrap();
    let paths = build_identity_paths(root.path(), "agent");
    let context = MemoryIdentityContext::new(
        "agent".to_string(),
        paths,
        MemorySessionBinding { identity: "agent".to_string(), repo_path_hash: "fixture".to_string(), bound_at: 0.0 },
    );
    let spawner = Arc::new(CountingSpawner::default());
    let spawn = Arc::new(ManualSpawn::default());
    let coordinator = if with_coordinator { Some(Arc::new(RecordingCoordinator::default())) } else { None };
    let registry = KibitzerSessionResourceRegistry::new();
    let ledger_directory = context.identity_paths.recall_ledger.clone();
    let resolve_context = {
        let context = context.clone();
        Arc::new(move |_session_id: &str| if resolve_available { Some(context.clone()) } else { None })
    };
    let options = MemoryRecallWiringOptions {
        resolve_context,
        resolve_settings: Arc::new(|| Ok(Value::Null)),
        env: Arc::new(|_name: &str| None),
        create_repo: Arc::new(|_identity: &MemoryIdentityContext| -> Result<GitMemoryRepo, String> {
            panic!("create_repo must not run in the shutdown lifecycle unit")
        }),
        ledger_for: Arc::new(move |_identity: &MemoryIdentityContext| RecallLedger::new(ledger_directory.clone())),
        pending_for: Arc::new(|_identity: &MemoryIdentityContext| Arc::new(InertPending) as Arc<dyn KibitzerPendingPort>),
        coordinator: coordinator.clone().map(|coordinator| coordinator as Arc<dyn KibitzerIdleCoordinator>),
        send_message: Arc::new(|_message: KibitzerSteerMessage| Ok(())),
        append_entry: Arc::new(|_kind: &str, _data: Value| {}),
        spawn: spawn.spawner(),
        timers: Arc::new(InertTimers),
        now_ms: Arc::new(|| 0),
        random: Arc::new(|| 0.0),
        warn: Arc::new(|_message: &str| {}),
        spawner: Arc::clone(&spawner) as Arc<dyn KibitzerChildSpawner>,
        executor: Arc::new(PanicExecutor),
        event_caps: Some(KibitzerEventCaps::default()),
        session_resources_for: registry.getter(),
        tool_budget: None,
        max_concurrent_wakes: Some(2),
        sidecar_max_tokens: None,
        drain_resolve_settings: Arc::new(|| Value::Null),
        drain_pending_for: Arc::new(|_identity: &MemoryIdentityContext| Arc::new(InertPendingTake) as Arc<dyn PendingNudgesPort>),
        drain_queued: None,
    };
    Harness { wiring: MemoryRecallWiring::new(options), spawner, spawn, coordinator, context, _root: root }
}

fn offer_input(path: &str) -> KibitzerOfferInput {
    KibitzerOfferInput {
        candidates: vec![RecallCandidate { path: path.to_string(), description: String::new(), excerpt: String::new(), score: 1.0 }],
        surfaced: BTreeSet::new(),
        max_items: 2,
        task_summary: None,
    }
}

fn sidecar_removed(harness: &Harness) -> bool {
    harness.wiring.sidecars.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(SESSION).is_none()
}

#[test]
fn given_a_resident_sidecar_when_shutdown_is_requested_then_a_late_offer_is_buffered_disposed_without_spawning() {
    let harness = harness(true, false);
    let sidecar = harness.wiring.sidecar_for(SESSION, &harness.context);
    assert_eq!(harness.spawner.calls.load(Ordering::SeqCst), 0, "lazy construction creates no child");

    harness.wiring.on_session_shutdown(SESSION);

    assert_eq!(harness.spawn.queued(), 1, "the async cleanup is queued, never run inline");
    assert!(sidecar_removed(&harness), "the synchronous half leaves the map immediately");

    // A FRESH candidate offered after the synchronous shutdown returned but BEFORE the queue drains.
    let offered = poll_ready_once(Box::pin(sidecar.offer(offer_input("fresh.md"))));
    assert_eq!(offered, Ok(KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Disposed }));
    assert_eq!(harness.spawner.calls.load(Ordering::SeqCst), 0, "a closing sidecar spawns no child");

    harness.spawn.drain();
    assert_eq!(sidecar.state(), KibitzerSidecarState::Disposed, "draining completes the terminal transition");
}

#[test]
fn given_the_context_is_no_longer_resolvable_when_shutdown_fires_then_the_cleanup_still_runs() {
    let harness = harness(false, false);
    let sidecar = harness.wiring.sidecar_for(SESSION, &harness.context);

    harness.wiring.on_session_shutdown(SESSION);
    harness.spawn.drain();

    assert_eq!(sidecar.state(), KibitzerSidecarState::Disposed, "cleanup does not consult the (absent) context");
}

#[test]
fn given_a_held_nudge_when_shutdown_runs_then_delivery_is_retracted_only_after_the_sidecar_is_disposed() {
    let harness = harness(true, true);
    let sidecar = harness.wiring.sidecar_for(SESSION, &harness.context);
    let coordinator = Arc::clone(harness.coordinator.as_ref().unwrap());
    *coordinator.sidecar.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&sidecar));

    harness.wiring.delivery().accept(SESSION, &harness.context, &[RecallNudge { path: "held.md".to_string(), hint: "held".to_string() }]);
    assert_eq!(coordinator.enqueued.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1, "the nudge is held on the coordinator");

    harness.wiring.on_session_shutdown(SESSION);
    assert!(coordinator.removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty(), "delivery is NOT retracted synchronously");

    harness.spawn.drain();
    let removed = coordinator.removed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(removed.len(), 1, "delivery retraction ran inside the scheduled cleanup");
    assert_eq!(removed[0].1, KibitzerSidecarState::Disposed, "the sidecar was already Disposed when delivery retracted");
}

#[test]
fn given_a_shut_down_session_when_a_tool_hook_fires_then_no_sidecar_is_resurrected() {
    let harness = harness(true, false);
    let sidecar = harness.wiring.sidecar_for(SESSION, &harness.context);
    let _ = sidecar.on_prompt("task", 0);
    let captured_before = sidecar.event_size();
    assert!(captured_before > 0, "the live sidecar captured the prompt");

    harness.wiring.on_session_shutdown(SESSION);
    harness.spawn.drain();

    harness.wiring.on_tool_call(SESSION, "call-1", "read", &serde_json::json!({ "file_path": "x.md" }), &[]);

    assert!(sidecar_removed(&harness), "a late tool hook never re-creates a terminal sidecar");
    assert_eq!(sidecar.event_size(), captured_before, "the disposed sidecar captured nothing");
}

/// A child that records every prompt the sidecar sends it, so a capture test can read the RENDERED
/// envelope the production renderer produced (there is no sidecar stream accessor).
struct CapturingChild {
    prompts: Mutex<Vec<String>>,
}

impl CapturingChild {
    fn last_prompt(&self) -> String {
        self.prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last().cloned().unwrap_or_default()
    }
}

impl KibitzerChild for CapturingChild {
    fn steer<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(text.to_string());
        Box::pin(async { Ok(()) })
    }
    fn follow_up<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(text.to_string());
        Box::pin(async { Ok(()) })
    }
    fn abort(&self) {}
    fn subscribe_nudges(&self, _listener: Arc<dyn Fn(RecallNudge) + Send + Sync>) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn subscribe_observations(&self, _listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>> {
        Box::pin(async { JudgeSettle { completed: true, cancelled: false, failure_message: None } })
    }
    fn dispose<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// A spawner that hands the SAME capturing child to the sidecar.
struct CapturingSpawner {
    child: Arc<CapturingChild>,
}

impl KibitzerChildSpawner for CapturingSpawner {
    fn spawn<'a>(&'a self, _input: KibitzerChildSpawnInput) -> Pin<Box<dyn Future<Output = Result<Arc<dyn KibitzerChild>, KibitzerSidecarStartError>> + Send + 'a>> {
        let child = Arc::clone(&self.child);
        Box::pin(async move { Ok(child as Arc<dyn KibitzerChild>) })
    }
}

/// The REAL wake-slot acquire, run in place: the task IS the production closure, so its verdict - and
/// the `PendingLease` it carries - is the real one; nothing private is fabricated.
struct RealTaskExecutor;

impl KibitzerBlockingExecutor for RealTaskExecutor {
    fn run_blocking(&self, task: BlockingAcquire) -> BlockingAcquireFuture {
        Box::pin(async move { task() })
    }
}

/// Builds a wiring whose sidecar can complete a real `offer` -> `seed` (a real wake-slot lease, a
/// capturing child) so the RENDERED seed prompt is observable. `create_repo` fails, so the prompt
/// hook's candidate collection returns `None` and no offer is auto-scheduled.
fn capture_harness(event_caps: Option<KibitzerEventCaps>) -> (Arc<MemoryRecallWiring>, MemoryIdentityContext, Arc<CapturingChild>, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let paths = build_identity_paths(root.path(), "agent");
    std::fs::create_dir_all(&paths.locks).unwrap();
    let context = MemoryIdentityContext::new(
        "agent".to_string(),
        paths,
        MemorySessionBinding { identity: "agent".to_string(), repo_path_hash: "fixture".to_string(), bound_at: 0.0 },
    );
    let child = Arc::new(CapturingChild { prompts: Mutex::new(Vec::new()) });
    let spawner: Arc<dyn KibitzerChildSpawner> = Arc::new(CapturingSpawner { child: Arc::clone(&child) });
    let spawn = Arc::new(ManualSpawn::default());
    let registry = KibitzerSessionResourceRegistry::new();
    let ledger_directory = context.identity_paths.recall_ledger.clone();
    let resolve_context = {
        let context = context.clone();
        Arc::new(move |_session_id: &str| Some(context.clone()))
    };
    let options = MemoryRecallWiringOptions {
        resolve_context,
        resolve_settings: Arc::new(|| Ok(Value::Null)),
        env: Arc::new(|_name: &str| None),
        create_repo: Arc::new(|_identity: &MemoryIdentityContext| -> Result<GitMemoryRepo, String> { Err("fixture: no repo".to_string()) }),
        ledger_for: Arc::new(move |_identity: &MemoryIdentityContext| RecallLedger::new(ledger_directory.clone())),
        pending_for: Arc::new(|_identity: &MemoryIdentityContext| Arc::new(InertPending) as Arc<dyn KibitzerPendingPort>),
        coordinator: None,
        send_message: Arc::new(|_message: KibitzerSteerMessage| Ok(())),
        append_entry: Arc::new(|_kind: &str, _data: Value| {}),
        spawn: spawn.spawner(),
        timers: Arc::new(InertTimers),
        now_ms: Arc::new(|| 0),
        random: Arc::new(|| 0.0),
        warn: Arc::new(|_message: &str| {}),
        spawner,
        executor: Arc::new(RealTaskExecutor),
        event_caps,
        session_resources_for: registry.getter(),
        tool_budget: None,
        max_concurrent_wakes: Some(2),
        sidecar_max_tokens: None,
        drain_resolve_settings: Arc::new(|| Value::Null),
        drain_pending_for: Arc::new(|_identity: &MemoryIdentityContext| Arc::new(InertPendingTake) as Arc<dyn PendingNudgesPort>),
        drain_queued: None,
    };
    (MemoryRecallWiring::new(options), context, child, root)
}

/// Drives ONE real `offer` (seed) and returns the rendered envelope the child received. Every await
/// on this path is uncontended, so the future resolves on the FIRST poll.
fn seeded_offer(sidecar: &Arc<KibitzerSidecar>, child: &CapturingChild, path: &str) -> String {
    let offered = poll_ready_once(Box::pin(sidecar.offer(offer_input(path))));
    assert!(matches!(offered, Ok(KibitzerOfferResult::Seeded { .. })), "the offer must seed: {offered:?}");
    child.last_prompt()
}

#[test]
fn given_a_branch_snapshot_when_the_prompt_hook_runs_then_the_assistant_event_precedes_the_prompt_event() {
    let (wiring, context, child, _root) = capture_harness(None);
    let entries = vec![serde_json::json!({ "type": "message", "message": { "role": "assistant", "content": "BRANCH-ASSISTANT-MARKER" } })];
    wiring.on_before_agent_start(SESSION, "CURRENT-PROMPT-MARKER", &entries);
    let sidecar = wiring.sidecar_for(SESSION, &context);
    let prompt = seeded_offer(&sidecar, &child, "cand.md");

    let events = &prompt[prompt.find("<events").expect("an events block")..];
    let assistant = events.find("BRANCH-ASSISTANT-MARKER").expect("the branch assistant event body");
    let current = events.find("CURRENT-PROMPT-MARKER").expect("the prompt event body");
    assert!(assistant < current, "the branch assistant event must precede the prompt event: {events}");
    assert!(events.contains("kind=\"assistant\"") && events.contains("kind=\"prompt\""), "both event kinds are on the wire: {events}");
}

#[test]
fn given_a_tool_call_and_result_when_the_tool_hooks_run_then_the_real_args_and_content_reach_the_seed_prompt() {
    let (wiring, context, child, _root) = capture_harness(None);
    let sidecar = wiring.sidecar_for(SESSION, &context);
    wiring.on_tool_call(SESSION, "call-1", "grep", &serde_json::json!({ "pattern": "TOOL-ARG-MARKER" }), &[]);
    let gate = crate::kibitzer_delivery::KibitzerToolResultGate { has_pending_messages: false, is_idle: true };
    wiring.on_tool_result(SESSION, "call-1", "grep", &serde_json::json!({}), &[maho_ext_api::ToolContent::text("RESULT-BODY-MARKER")], true, &[], &gate);
    let prompt = seeded_offer(&sidecar, &child, "cand.md");

    assert!(prompt.contains("kind=\"tool_call\"") && prompt.contains("tool=\"grep\""), "the tool_call event: {prompt}");
    assert!(prompt.contains("<args>") && prompt.contains("TOOL-ARG-MARKER"), "the REAL tool args reach the prompt: {prompt}");
    assert!(prompt.contains("kind=\"tool_result\"") && prompt.contains("<result>") && prompt.contains("RESULT-BODY-MARKER"), "the REAL tool content reaches the prompt: {prompt}");
    // The prompt wire carries no `error` attribute (render_event's contract), so the captured
    // `is_error` flag is stored on the event but never rendered into the envelope.
    assert!(!prompt.contains("error=\"true\""), "the prompt wire omits the error flag: {prompt}");
}

#[test]
fn given_a_small_tool_args_cap_when_the_tool_hook_runs_then_the_wiring_forwards_it_and_the_args_are_truncated() {
    let caps = KibitzerEventCaps { tool_args: 20, result_head: 20, assistant: 1500, prompt: 4000 };
    let (wiring, context, child, _root) = capture_harness(Some(caps));
    let sidecar = wiring.sidecar_for(SESSION, &context);
    wiring.on_tool_call(SESSION, "call-1", "grep", &serde_json::json!({ "pattern": "TOOL-ARG-MARKER-LONG" }), &[]);
    let prompt = seeded_offer(&sidecar, &child, "cand.md");

    assert!(!prompt.contains("TOOL-ARG-MARKER-LONG"), "the tool_args cap must truncate the stored body: {prompt}");
    assert!(prompt.contains("[+"), "the truncation marker proves the cap reached the stream: {prompt}");
}


