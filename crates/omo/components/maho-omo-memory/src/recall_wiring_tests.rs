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

use memory_core::git::GitCommitAuthor;
use memory_core::identity::layout::build_identity_paths;
use memory_core::recall::{RecallCandidate, RecallNudge};

use crate::binding::MemorySessionBinding;
use crate::kibitzer_child::{
    JudgeSettle, KibitzerChild, KibitzerChildObservation, KibitzerChildSpawnFuture,
    KibitzerChildSpawnInput,
};
use crate::kibitzer_contract::{KibitzerBufferedReason, KibitzerOfferResult, KibitzerSidecarState};
use crate::kibitzer_delivery::{KibitzerCoordinatorEntry, KibitzerPendingPort};
use crate::kibitzer_session_resources::KibitzerSessionResourceRegistry;
use crate::kibitzer_sidecar_admission::{BlockingAcquire, BlockingAcquireFuture};
use crate::recall_drain::PendingNudgesPort;

/// The session every case binds.
const SESSION: &str = "s1";

/// The resident sidecar, which these cases always expect to exist: recall is enabled by default, so a
/// `None` here is a defect in the case's own setup, not a condition to branch on.
fn resident(wiring: &MemoryRecallWiring, context: &MemoryIdentityContext) -> Arc<KibitzerSidecar> {
    wiring
        .sidecar_for(SESSION, context)
        .expect("recall is enabled by default, so the sidecar must be built")
}

fn tool_call_offer_harness() -> (
    Arc<MemoryRecallWiring>,
    MemoryIdentityContext,
    Arc<ManualSpawn>,
    tempfile::TempDir,
) {
    let (mut wiring, context, _child, root) = capture_harness(None);
    let repo = GitMemoryRepo::open(context.identity_paths.repo.clone(), "agent").unwrap();
    repo.init(None).unwrap();
    let notes = context.identity_paths.repo.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("qa.md"), "---\ndescription: deploy gate\n---\nThe note records the canary rollback gate and the release order.\n").unwrap();
    repo.commit_write(
        &["notes/qa.md"],
        "fixture corpus",
        &GitCommitAuthor {
            agent_id: "agent".into(),
            author_name: "agent".into(),
            author_email: None,
        },
    )
    .unwrap();
    let repo_directory = context.identity_paths.repo.clone();
    let spawn = Arc::new(ManualSpawn::default());
    let options = &mut Arc::get_mut(&mut wiring)
        .expect("fixture wiring is unshared")
        .options;
    options.create_repo = Arc::new(move |_identity| {
        GitMemoryRepo::open(repo_directory.clone(), "agent").map_err(|error| error.to_string())
    });
    options.spawn = spawn.spawner();
    options.offload_blocking = spawn.blocking();
    (wiring, context, spawn, root)
}

#[test]
fn matching_tool_call_offers_without_a_prompt_candidate() {
    let (wiring, context, spawn, _root) = tool_call_offer_harness();
    let _sidecar = resident(&wiring, &context);
    Arc::clone(&wiring).on_before_agent_start(
        SESSION,
        "unrelated chatter with no corpus token",
        0,
        &[],
    );
    // The handler hands its collection to the blocking seam instead of running it inline; draining
    // runs it to completion, which is where the (absent) candidate set is decided.
    spawn.drain_blocking();
    spawn.drain();
    assert_eq!(spawn.queued(), 0);
    wiring.on_tool_call(
        SESSION,
        "call-1",
        "grep",
        &serde_json::json!({ "pattern": "canary rollback gate" }),
        &BranchTail {
            len: 0,
            entries: &[],
            summary: &[],
        },
    );
    assert_eq!(spawn.queued(), 1);
}

#[test]
fn matching_tool_call_after_shutdown_does_not_schedule_an_offer() {
    let (wiring, context, spawn, _root) = tool_call_offer_harness();
    let _sidecar = resident(&wiring, &context);
    wiring.on_session_shutdown(SESSION);
    assert_eq!(spawn.queued(), 1);
    wiring.on_tool_call(
        SESSION,
        "call-1",
        "grep",
        &serde_json::json!({ "pattern": "canary rollback gate" }),
        &BranchTail {
            len: 0,
            entries: &[],
            summary: &[],
        },
    );
    assert_eq!(spawn.queued(), 1);
}

#[test]
fn tool_hooks_reuse_the_turn_recall_settings_and_a_config_edit_applies_next_turn() {
    let (mut wiring, context, spawn, _root) = tool_call_offer_harness();
    // The sidecar is built BEFORE the counting resolver is installed, so the counts below measure the
    // tool hooks and the collection alone.
    let _sidecar = resident(&wiring, &context);
    let disabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let resolves = Arc::new(AtomicUsize::new(0));
    {
        let (disabled, resolves) = (Arc::clone(&disabled), Arc::clone(&resolves));
        Arc::get_mut(&mut wiring)
            .expect("fixture wiring is unshared")
            .options
            .resolve_settings = Arc::new(move || {
            resolves.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({ "recall": { "enabled": !disabled.load(Ordering::SeqCst) } }))
        });
    }
    let call = |wiring: &MemoryRecallWiring| {
        wiring.on_tool_call(
            SESSION,
            "call-1",
            "grep",
            &serde_json::json!({ "pattern": "canary rollback gate" }),
            &BranchTail {
                len: 0,
                entries: &[],
                summary: &[],
            },
        )
    };
    // A tool hook with no turn-cached value resolves fresh.
    call(&wiring);
    assert_eq!((resolves.load(Ordering::SeqCst), spawn.queued()), (1, 1));
    Arc::clone(&wiring).on_before_agent_start(
        SESSION,
        "unrelated chatter with no corpus token",
        0,
        &[],
    );
    // The handler must NOT do the collection inline: that work is a git read plus a corpus scan on
    // the thread that paints and reads input. It hands the collection to the blocking seam and
    // returns; the resolve happens only when that task actually runs.
    assert_eq!(resolves.load(Ordering::SeqCst), 1);
    // Two blocking tasks are pending: the sidecar's owner acquire plus its first sweep, and the
    // recall collection this case is about. The count is asserted after draining the sidecar's own
    // pair, so the collection is what remains.
    spawn.drain_blocking();
    spawn.drain();
    assert_eq!(resolves.load(Ordering::SeqCst), 2);
    // A mid-turn config edit does not reach this turn's tool hooks, which never re-resolve.
    disabled.store(true, Ordering::SeqCst);
    call(&wiring);
    call(&wiring);
    assert_eq!((resolves.load(Ordering::SeqCst), spawn.queued()), (2, 2));
    // The next turn resolves the edit; its tool hooks then see recall disabled.
    Arc::clone(&wiring).on_before_agent_start(
        SESSION,
        "unrelated chatter with no corpus token",
        0,
        &[],
    );
    spawn.drain_blocking();
    spawn.drain();
    call(&wiring);
    assert_eq!((resolves.load(Ordering::SeqCst), spawn.queued()), (3, 0));
}

/// One poll with a no-op waker; the caller decides whether Pending is expected.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    use std::task::{Context as TaskContext, Waker};
    let waker = Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    future.poll(&mut context)
}

/// Polls an owned boxed future EXACTLY ONCE and asserts it is Ready. The shutdown lifecycle's
/// futures are uncontended (`serialized` is an uncontended `tokio::sync::Mutex`, and with no active
/// child or turn `shutdown()` has no real await), so a Pending result is a DEFECT - never a timing
/// budget. There is no spin, no retry and no polling luck.
fn poll_ready_once<F: Future + ?Sized>(mut future: Pin<Box<F>>) -> F::Output {
    match poll_once(future.as_mut()) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!(
            "a shutdown-lifecycle future must resolve on the FIRST poll; Pending is a defect"
        ),
    }
}

/// The scheduled-cleanup queue: `spawn` queues, `drain` runs, and nothing runs until then.
#[derive(Default)]
struct ManualSpawn {
    queue: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>,
    blocking_queue: Mutex<Vec<BlockingTask>>,
}

impl ManualSpawn {
    fn spawner(self: &Arc<Self>) -> KibitzerWakeSpawn {
        let me = Arc::clone(self);
        Arc::new(move |future| {
            me.queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(future)
        })
    }

    fn queued(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn blocking(self: &Arc<Self>) -> Arc<dyn Fn(BlockingTask) + Send + Sync> {
        let me = Arc::clone(self);
        Arc::new(move |task| {
            me.blocking_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(task)
        })
    }

    fn blocking_queued(&self) -> usize {
        self.blocking_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn drain_blocking(&self) {
        loop {
            let next = {
                self.blocking_queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop()
            };
            match next {
                Some(task) => task(),
                None => break,
            }
        }
    }

    fn drain(&self) {
        loop {
            let next = {
                self.queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop()
            };
            match next {
                Some(future) => {
                    poll_ready_once(future);
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
    fn spawn<'a>(&'a self, _input: KibitzerChildSpawnInput) -> KibitzerChildSpawnFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Arc::new(StubChild) as Arc<dyn KibitzerChild>) })
    }
}

/// An inert child: the shutdown lifecycle under test never drives a real turn.
struct StubChild;

impl KibitzerChild for StubChild {
    fn steer<'a>(
        &'a self,
        _text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
    fn follow_up<'a>(
        &'a self,
        _text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
    fn abort(&self) {}
    fn subscribe_nudges(
        &self,
        _listener: Arc<dyn Fn(RecallNudge) + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn subscribe_observations(
        &self,
        _listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>> {
        Box::pin(async {
            JudgeSettle {
                completed: true,
                cancelled: false,
                failure_message: None,
            }
        })
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
        self.enqueued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry.key);
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
        self.removed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((key.to_string(), observed));
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
        MemorySessionBinding {
            identity: "agent".to_string(),
            repo_path_hash: "fixture".to_string(),
            bound_at: 0.0,
        },
    );
    let spawner = Arc::new(CountingSpawner::default());
    let spawn = Arc::new(ManualSpawn::default());
    let coordinator = if with_coordinator {
        Some(Arc::new(RecordingCoordinator::default()))
    } else {
        None
    };
    let registry = KibitzerSessionResourceRegistry::new();
    let ledger_directory = context.identity_paths.recall_ledger.clone();
    let resolve_context = {
        let context = context.clone();
        Arc::new(move |_session_id: &str| {
            if resolve_available {
                Some(context.clone())
            } else {
                None
            }
        })
    };
    let options = MemoryRecallWiringOptions {
        resolve_context,
        resolve_settings: Arc::new(|| Ok(Value::Null)),
        env: Arc::new(|_name: &str| None),
        create_repo: Arc::new(
            |_identity: &MemoryIdentityContext| -> Result<GitMemoryRepo, String> {
                panic!("create_repo must not run in the shutdown lifecycle unit")
            },
        ),
        ledger_for: Arc::new(move |_identity: &MemoryIdentityContext| {
            RecallLedger::new(ledger_directory.clone())
        }),
        pending_for: Arc::new(|_identity: &MemoryIdentityContext| {
            Arc::new(InertPending) as Arc<dyn KibitzerPendingPort>
        }),
        coordinator: coordinator
            .clone()
            .map(|coordinator| coordinator as Arc<dyn KibitzerIdleCoordinator>),
        send_message: Arc::new(|_message: KibitzerSteerMessage| Ok(())),
        append_entry: Arc::new(|_kind: &str, _data: Value| {}),
        spawn: spawn.spawner(),
        offload_blocking: spawn.blocking(),
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
        drain_pending_for: Arc::new(|_identity: &MemoryIdentityContext| {
            Arc::new(InertPendingTake) as Arc<dyn PendingNudgesPort>
        }),
        drain_queued: None,
    };
    Harness {
        wiring: MemoryRecallWiring::new(options),
        spawner,
        spawn,
        coordinator,
        context,
        _root: root,
    }
}

fn offer_input(path: &str) -> KibitzerOfferInput {
    KibitzerOfferInput {
        candidates: vec![RecallCandidate {
            path: path.to_string(),
            description: String::new(),
            excerpt: String::new(),
            score: 1.0,
        }],
        surfaced: BTreeSet::new(),
        max_items: 2,
        task_summary: None,
    }
}

fn sidecar_removed(harness: &Harness) -> bool {
    harness
        .wiring
        .sidecars
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(SESSION)
        .is_none()
}

#[test]
fn given_a_resident_sidecar_when_shutdown_is_requested_then_a_late_offer_is_buffered_disposed_without_spawning()
 {
    let harness = harness(true, false);
    let sidecar = resident(&harness.wiring, &harness.context);
    assert_eq!(
        harness.spawner.calls.load(Ordering::SeqCst),
        0,
        "lazy construction creates no child"
    );

    harness.wiring.on_session_shutdown(SESSION);

    assert_eq!(
        harness.spawn.queued(),
        1,
        "the async cleanup is queued, never run inline"
    );
    assert!(
        sidecar_removed(&harness),
        "the synchronous half leaves the map immediately"
    );

    // A FRESH candidate offered after the synchronous shutdown returned but BEFORE the queue drains.
    let offered = poll_ready_once(Box::pin(sidecar.offer(offer_input("fresh.md"))));
    assert_eq!(
        offered,
        Ok(KibitzerOfferResult::Buffered {
            reason: KibitzerBufferedReason::Disposed
        })
    );
    assert_eq!(
        harness.spawner.calls.load(Ordering::SeqCst),
        0,
        "a closing sidecar spawns no child"
    );

    harness.spawn.drain();
    assert_eq!(
        sidecar.state(),
        KibitzerSidecarState::Disposed,
        "draining completes the terminal transition"
    );
}

#[test]
fn given_the_context_is_no_longer_resolvable_when_shutdown_fires_then_the_cleanup_still_runs() {
    let harness = harness(false, false);
    let sidecar = resident(&harness.wiring, &harness.context);

    harness.wiring.on_session_shutdown(SESSION);
    harness.spawn.drain();

    assert_eq!(
        sidecar.state(),
        KibitzerSidecarState::Disposed,
        "cleanup does not consult the (absent) context"
    );
}

#[test]
fn given_a_held_nudge_when_shutdown_runs_then_delivery_is_retracted_only_after_the_sidecar_is_disposed()
 {
    let harness = harness(true, true);
    let sidecar = resident(&harness.wiring, &harness.context);
    let coordinator = Arc::clone(harness.coordinator.as_ref().unwrap());
    *coordinator
        .sidecar
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&sidecar));

    harness.wiring.delivery().accept(
        SESSION,
        &harness.context,
        &[RecallNudge {
            path: "held.md".to_string(),
            hint: "held".to_string(),
            source: None,
        }],
    );
    assert_eq!(
        coordinator
            .enqueued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1,
        "the nudge is held on the coordinator"
    );

    harness.wiring.on_session_shutdown(SESSION);
    assert!(
        coordinator
            .removed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "delivery is NOT retracted synchronously"
    );

    harness.spawn.drain();
    let removed = coordinator
        .removed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        removed.len(),
        1,
        "delivery retraction ran inside the scheduled cleanup"
    );
    assert_eq!(
        removed[0].1,
        KibitzerSidecarState::Disposed,
        "the sidecar was already Disposed when delivery retracted"
    );
}

#[test]
fn given_a_shut_down_session_when_a_tool_hook_fires_then_no_sidecar_is_resurrected() {
    let harness = harness(true, false);
    let sidecar = resident(&harness.wiring, &harness.context);
    let _ = sidecar.on_prompt("task", 0);
    let captured_before = sidecar.event_size();
    assert!(captured_before > 0, "the live sidecar captured the prompt");

    harness.wiring.on_session_shutdown(SESSION);
    harness.spawn.drain();

    harness.wiring.on_tool_call(
        SESSION,
        "call-1",
        "read",
        &serde_json::json!({ "file_path": "x.md" }),
        &BranchTail {
            len: 0,
            entries: &[],
            summary: &[],
        },
    );

    assert!(
        sidecar_removed(&harness),
        "a late tool hook never re-creates a terminal sidecar"
    );
    assert_eq!(
        sidecar.event_size(),
        captured_before,
        "the disposed sidecar captured nothing"
    );
}

/// A child that records every prompt the sidecar sends it, so a capture test can read the RENDERED
/// envelope the production renderer produced (there is no sidecar stream accessor).
struct CapturingChild {
    prompts: Mutex<Vec<String>>,
}

impl CapturingChild {
    fn last_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

impl KibitzerChild for CapturingChild {
    fn steer<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(text.to_string());
        Box::pin(async { Ok(()) })
    }
    fn follow_up<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(text.to_string());
        Box::pin(async { Ok(()) })
    }
    fn abort(&self) {}
    fn subscribe_nudges(
        &self,
        _listener: Arc<dyn Fn(RecallNudge) + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn subscribe_observations(
        &self,
        _listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send> {
        Box::new(|| {})
    }
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>> {
        Box::pin(async {
            JudgeSettle {
                completed: true,
                cancelled: false,
                failure_message: None,
            }
        })
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
    fn spawn<'a>(&'a self, _input: KibitzerChildSpawnInput) -> KibitzerChildSpawnFuture<'a> {
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
fn capture_harness(
    event_caps: Option<KibitzerEventCaps>,
) -> (
    Arc<MemoryRecallWiring>,
    MemoryIdentityContext,
    Arc<CapturingChild>,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let paths = build_identity_paths(root.path(), "agent");
    std::fs::create_dir_all(&paths.locks).unwrap();
    let context = MemoryIdentityContext::new(
        "agent".to_string(),
        paths,
        MemorySessionBinding {
            identity: "agent".to_string(),
            repo_path_hash: "fixture".to_string(),
            bound_at: 0.0,
        },
    );
    let child = Arc::new(CapturingChild {
        prompts: Mutex::new(Vec::new()),
    });
    let spawner: Arc<dyn KibitzerChildSpawner> = Arc::new(CapturingSpawner {
        child: Arc::clone(&child),
    });
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
        create_repo: Arc::new(
            |_identity: &MemoryIdentityContext| -> Result<GitMemoryRepo, String> {
                Err("fixture: no repo".to_string())
            },
        ),
        ledger_for: Arc::new(move |_identity: &MemoryIdentityContext| {
            RecallLedger::new(ledger_directory.clone())
        }),
        pending_for: Arc::new(|_identity: &MemoryIdentityContext| {
            Arc::new(InertPending) as Arc<dyn KibitzerPendingPort>
        }),
        coordinator: None,
        send_message: Arc::new(|_message: KibitzerSteerMessage| Ok(())),
        append_entry: Arc::new(|_kind: &str, _data: Value| {}),
        spawn: spawn.spawner(),
        offload_blocking: spawn.blocking(),
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
        drain_pending_for: Arc::new(|_identity: &MemoryIdentityContext| {
            Arc::new(InertPendingTake) as Arc<dyn PendingNudgesPort>
        }),
        drain_queued: None,
    };
    (MemoryRecallWiring::new(options), context, child, root)
}

/// Drives ONE real `offer` (seed) and returns the rendered envelope the child received. Every await
/// on this path is uncontended, so the future resolves on the FIRST poll.
fn seeded_offer(sidecar: &Arc<KibitzerSidecar>, child: &CapturingChild, path: &str) -> String {
    let offered = poll_ready_once(Box::pin(sidecar.offer(offer_input(path))));
    assert!(
        matches!(offered, Ok(KibitzerOfferResult::Seeded { .. })),
        "the offer must seed: {offered:?}"
    );
    child.last_prompt()
}

#[test]
fn given_a_branch_snapshot_when_the_prompt_hook_runs_then_the_assistant_event_precedes_the_prompt_event()
 {
    let (wiring, context, child, _root) = capture_harness(None);
    let entries = vec![
        serde_json::json!({ "type": "message", "message": { "role": "assistant", "content": "BRANCH-ASSISTANT-MARKER" } }),
    ];
    Arc::clone(&wiring).on_before_agent_start(
        SESSION,
        "CURRENT-PROMPT-MARKER",
        entries.len(),
        &entries,
    );
    let sidecar = resident(&wiring, &context);
    let prompt = seeded_offer(&sidecar, &child, "cand.md");

    let events = &prompt[prompt.find("<events").expect("an events block")..];
    let assistant = events
        .find("BRANCH-ASSISTANT-MARKER")
        .expect("the branch assistant event body");
    let current = events
        .find("CURRENT-PROMPT-MARKER")
        .expect("the prompt event body");
    assert!(
        assistant < current,
        "the branch assistant event must precede the prompt event: {events}"
    );
    assert!(
        events.contains("kind=\"assistant\"") && events.contains("kind=\"prompt\""),
        "both event kinds are on the wire: {events}"
    );
}

#[test]
fn given_a_tool_call_and_result_when_the_tool_hooks_run_then_the_real_args_and_content_reach_the_seed_prompt()
 {
    let (wiring, context, child, _root) = capture_harness(None);
    let sidecar = resident(&wiring, &context);
    wiring.on_tool_call(
        SESSION,
        "call-1",
        "grep",
        &serde_json::json!({ "pattern": "TOOL-ARG-MARKER" }),
        &BranchTail {
            len: 0,
            entries: &[],
            summary: &[],
        },
    );
    let gate = crate::kibitzer_delivery::KibitzerToolResultGate {
        has_pending_messages: false,
        is_idle: true,
    };
    wiring.on_tool_result(
        SESSION,
        &KibitzerToolResultEvent {
            tool_call_id: "call-1",
            tool_name: "grep",
            input: &serde_json::json!({}),
            content: &[maho_ext_api::ToolContent::text("RESULT-BODY-MARKER")],
            is_error: true,
        },
        &BranchTail {
            len: 0,
            entries: &[],
            summary: &[],
        },
        &gate,
    );
    let prompt = seeded_offer(&sidecar, &child, "cand.md");

    assert!(
        prompt.contains("kind=\"tool_call\"") && prompt.contains("tool=\"grep\""),
        "the tool_call event: {prompt}"
    );
    assert!(
        prompt.contains("<args>") && prompt.contains("TOOL-ARG-MARKER"),
        "the REAL tool args reach the prompt: {prompt}"
    );
    assert!(
        prompt.contains("kind=\"tool_result\"")
            && prompt.contains("<result>")
            && prompt.contains("RESULT-BODY-MARKER"),
        "the REAL tool content reaches the prompt: {prompt}"
    );
    // The prompt wire carries no `error` attribute (render_event's contract), so the captured
    // `is_error` flag is stored on the event but never rendered into the envelope.
    assert!(
        !prompt.contains("error=\"true\""),
        "the prompt wire omits the error flag: {prompt}"
    );
}

#[test]
fn given_a_small_tool_args_cap_when_the_tool_hook_runs_then_the_wiring_forwards_it_and_the_args_are_truncated()
 {
    let caps = KibitzerEventCaps {
        tool_args: 20,
        result_head: 20,
        assistant: 1500,
        prompt: 4000,
    };
    let (wiring, context, child, _root) = capture_harness(Some(caps));
    let sidecar = resident(&wiring, &context);
    wiring.on_tool_call(
        SESSION,
        "call-1",
        "grep",
        &serde_json::json!({ "pattern": "TOOL-ARG-MARKER-LONG" }),
        &BranchTail {
            len: 0,
            entries: &[],
            summary: &[],
        },
    );
    let prompt = seeded_offer(&sidecar, &child, "cand.md");

    assert!(
        !prompt.contains("TOOL-ARG-MARKER-LONG"),
        "the tool_args cap must truncate the stored body: {prompt}"
    );
    assert!(
        prompt.contains("[+"),
        "the truncation marker proves the cap reached the stream: {prompt}"
    );
}

/// The recall gate and the ownership side effects, driven through the real hook surface.
mod sidecar_ownership {
    use super::*;

    fn disabled_harness() -> (
        Arc<MemoryRecallWiring>,
        MemoryIdentityContext,
        Arc<ManualSpawn>,
        tempfile::TempDir,
    ) {
        let (mut wiring, context, _child, root) = capture_harness(None);
        Arc::get_mut(&mut wiring)
            .expect("fixture wiring is unshared")
            .options
            .resolve_settings =
            Arc::new(|| Ok(serde_json::json!({ "recall": { "enabled": false } })));
        let spawn = {
            let options = &mut Arc::get_mut(&mut wiring)
                .expect("fixture wiring is unshared")
                .options;
            let spawn = Arc::new(ManualSpawn::default());
            options.spawn = spawn.spawner();
            options.offload_blocking = spawn.blocking();
            spawn
        };
        (wiring, context, spawn, root)
    }

    #[test]
    fn a_disabled_recall_never_creates_or_owns_a_sidecar() {
        let (wiring, context, spawn, _root) = disabled_harness();

        let sidecar = wiring.sidecar_for(SESSION, &context);

        assert!(
            sidecar.is_none(),
            "the getter returns undefined before anything is built, as upstream does"
        );
        assert!(!wiring.sidecar_owner.owns(SESSION));
        assert_eq!(
            spawn.blocking_queued(),
            0,
            "no owner acquire and no sweep are scheduled for a disabled identity"
        );
        assert!(
            !context
                .identity_paths
                .locks
                .join("recall-sidecar.MQ.lock")
                .exists(),
            "no owner lock file is written"
        );
    }

    #[test]
    fn enabling_recall_after_disabled_owns_and_sweeps_the_backlog() {
        let (mut wiring, context, spawn, _root) = disabled_harness();
        let expired = crate::sidecar_sweep::sidecar_dir(&context.identity_paths.recall, "old");
        std::fs::create_dir_all(&expired).unwrap();
        assert!(wiring.sidecar_for(SESSION, &context).is_none());
        assert_eq!(spawn.blocking_queued(), 0);

        Arc::get_mut(&mut wiring)
            .expect("fixture wiring is unshared")
            .options
            .resolve_settings =
            Arc::new(|| Ok(serde_json::json!({ "recall": { "enabled": true } })));
        let sidecar = wiring.sidecar_for(SESSION, &context);

        assert!(sidecar.is_some(), "enabling recall builds the sidecar");
        assert!(wiring.sidecar_owner.owns(SESSION));
        assert_eq!(
            spawn.blocking_queued(),
            2,
            "the owner acquire and the first sweep are queued on the blocking seam"
        );
    }

    #[test]
    fn the_owner_acquire_and_the_sweep_are_offloaded_not_spawned() {
        let (wiring, context, spawn, _root) = tool_call_offer_harness();
        let _sidecar = resident(&wiring, &context);

        assert_eq!(
            spawn.queued(),
            0,
            "neither the acquire nor the sweep may run on the async seam, which keeps polling on \
             the thread that paints and reads input"
        );
        assert_eq!(spawn.blocking_queued(), 2);
    }

    #[test]
    fn shutdown_releases_ownership_and_sweeps_after_the_sidecar_is_disposed() {
        let (wiring, context, spawn, _root) = tool_call_offer_harness();
        let sidecar = resident(&wiring, &context);
        assert!(wiring.sidecar_owner.owns(SESSION));

        wiring.on_session_shutdown(SESSION);

        assert!(
            !wiring.sidecar_owner.owns(SESSION),
            "ownership ends synchronously with the shutdown call"
        );
        assert_eq!(
            spawn.queued(),
            1,
            "the async half is the sidecar disposal, as before"
        );
        spawn.drain_blocking();
        spawn.drain();
        assert_eq!(
            sidecar.state(),
            KibitzerSidecarState::Disposed,
            "the sidecar itself is still shut down by the caller"
        );
    }
}

/// The REAL `append_wake_line` against a tempdir plus the reporter the wiring installs, so the record
/// chain is exercised end to end: a line lands in `sidecars/<encoded>/wakes.ndjson`.
mod wake_log_wiring {
    use super::*;
    use crate::kibitzer_sidecar_outcome::{KibitzerWakeOutcome, KibitzerWakeStatus};
    use crate::sidecar_wake_log::{AppendEntry, WakeReporter};
    use std::path::Path;

    fn record_outcome(session_id: &str, status: KibitzerWakeStatus) -> KibitzerWakeOutcome {
        KibitzerWakeOutcome {
            session_id: session_id.to_string(),
            wake: 7,
            generation: 2,
            status,
            cause: None,
            reason: None,
            configuration: None,
            model: None,
            nudges: Vec::new(),
            candidate_count: 1,
            steered: 0,
            tool_calls: 0,
            duration_ms: 12,
            slot_wait_ms: 3,
            cursors: None,
            context_tokens: None,
            usage: None,
            diagnostic: false,
        }
    }

    fn report(root: &tempfile::TempDir, session_id: &str, reporter: &WakeReporter) {
        let recall = root.path().join("runtime/recall");
        let append = |recall: &Path, session: &str, line: &str| {
            append_wake_line(recall, session, line).expect("the tempdir is writable");
        };
        let warn = |_: &str| {};
        let now = || 1_700_000_000_000i64;
        let entries: AppendEntry = Arc::new(|_: &str, _: Value| {});
        reporter.report(
            &record_outcome(session_id, KibitzerWakeStatus::Completed),
            &crate::sidecar_wake_log::WakeLogDeps {
                recall: &recall,
                append: &append,
                warn: &warn,
                now_ms: &now,
            },
            &entries,
            Some("recall"),
        );
    }

    fn lines(root: &tempfile::TempDir, session_id: &str) -> Vec<String> {
        let recall = root.path().join("runtime/recall");
        let path = crate::sidecar_wake_log::wakes_file(&recall, session_id);
        match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().map(str::to_string).collect(),
            Err(error) => panic!("{} unreadable: {error}", path.display()),
        }
    }

    #[test]
    fn a_reported_wake_appends_one_line_to_the_session_log() {
        let root = tempfile::tempdir().unwrap();
        let reporter = WakeReporter::new();
        report(&root, SESSION, &reporter);

        let lines = lines(&root, SESSION);
        assert_eq!(lines.len(), 1);
        let record: Value = serde_json::from_str(&lines[0]).expect("one JSON line");
        assert_eq!(record["sessionId"], SESSION);
        assert_eq!(record["wake"], 7);
        assert_eq!(record["status"], "completed");
    }

    #[test]
    fn repeated_wakes_append_rather_than_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let reporter = WakeReporter::new();
        report(&root, SESSION, &reporter);
        report(&root, SESSION, &reporter);
        report(&root, SESSION, &reporter);

        assert_eq!(
            lines(&root, SESSION).len(),
            3,
            "the log is append-only across wakes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_wake_log_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let reporter = WakeReporter::new();
        report(&root, SESSION, &reporter);

        let recall = root.path().join("runtime/recall");
        let path = crate::sidecar_wake_log::wakes_file(&recall, SESSION);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "upstream writes the record with mode 0o600");
    }

    #[test]
    fn two_sessions_write_independent_logs() {
        let root = tempfile::tempdir().unwrap();
        let reporter = WakeReporter::new();
        report(&root, "s1", &reporter);
        report(&root, "s2", &reporter);

        assert_eq!(lines(&root, "s1").len(), 1);
        assert_eq!(lines(&root, "s2").len(), 1);
    }
}
