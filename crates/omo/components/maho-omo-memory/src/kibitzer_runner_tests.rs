//! Deterministic regressions for the async recall runner.
//!
//! Every case drives the real `KibitzerWakeRunner` seam with a recording child and a manual spawn
//! queue. No runtime, no sleep, no thread: the futures involved resolve from polling alone, so one
//! bounded poll loop is a complete scheduler for them.

use super::*;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;

use memory_core::recall::RecallNudge;

use crate::kibitzer_child::{JudgeSettle, KibitzerChildObservation};
use crate::kibitzer_contract::{KibitzerWakeRequest, KibitzerWakeStatus};

type NudgeListener = Arc<dyn Fn(RecallNudge) + Send + Sync>;
type ToolListener = Arc<dyn Fn(KibitzerChildObservation) + Send + Sync>;
/// One armed settlement. Each `settle()` call owns its OWN cell, so releasing one can never resolve
/// another - the one-shot contract the runner's steer-into-running-turn path depends on.
type SettleCell = Arc<Mutex<Option<JudgeSettle>>>;

struct RecordingChild {
    nudges: Arc<Mutex<Vec<NudgeListener>>>,
    tools: Arc<Mutex<Vec<ToolListener>>>,
    settles: Mutex<VecDeque<SettleCell>>,
    seq: AtomicUsize,
    settle_seq: AtomicUsize,
    trigger_seq: AtomicUsize,
    settle: JudgeSettle,
    aborted: AtomicUsize,
    disposed: AtomicUsize,
    fail_follow_up: AtomicBool,
    dispose_error: AtomicBool,
}

impl RecordingChild {
    fn armed_settles(&self) -> usize {
        self.settles.lock().unwrap().len()
    }
    fn release_settle(&self, index: usize) {
        let cell = self.settles.lock().unwrap().get(index).cloned();
        if let Some(cell) = cell {
            *cell.lock().unwrap() = Some(self.settle.clone());
        }
    }
    fn release_last_settle(&self) {
        let armed = self.armed_settles();
        assert!(armed > 0, "no settlement was armed");
        self.release_settle(armed - 1);
    }
    fn fire_nudge(&self, index: usize, nudge: RecallNudge) {
        let listener = self.nudges.lock().unwrap().get(index).cloned();
        if let Some(listener) = listener {
            listener(nudge);
        }
    }
    fn fire_tool(&self, index: usize, event: KibitzerChildObservation) {
        let listener = self.tools.lock().unwrap().get(index).cloned();
        if let Some(listener) = listener {
            listener(event);
        }
    }
    fn fire_all_tools(&self, event: KibitzerChildObservation) {
        let listeners: Vec<ToolListener> = self.tools.lock().unwrap().clone();
        for listener in listeners {
            listener(event.clone());
        }
    }
    fn tool_listeners(&self) -> usize {
        self.tools.lock().unwrap().len()
    }
    fn nudge_listeners(&self) -> usize {
        self.nudges.lock().unwrap().len()
    }
}

impl KibitzerChild for RecordingChild {
    fn steer<'a>(&'a self, _text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.trigger_seq.store(self.seq.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn follow_up<'a>(&'a self, _text: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.trigger_seq.store(self.seq.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        let fail = self.fail_follow_up.load(Ordering::SeqCst);
        Box::pin(async move { if fail { Err("trigger failed".into()) } else { Ok(()) } })
    }
    fn abort(&self) {
        self.aborted.fetch_add(1, Ordering::SeqCst);
    }
    fn subscribe_nudges(&self, listener: NudgeListener) -> Box<dyn FnOnce() + Send> {
        self.nudges.lock().unwrap().push(Arc::clone(&listener));
        let registry = Arc::clone(&self.nudges);
        Box::new(move || registry.lock().unwrap().retain(|candidate| !Arc::ptr_eq(candidate, &listener)))
    }
    fn subscribe_observations(&self, listener: ToolListener) -> Box<dyn FnOnce() + Send> {
        self.tools.lock().unwrap().push(Arc::clone(&listener));
        let registry = Arc::clone(&self.tools);
        Box::new(move || registry.lock().unwrap().retain(|candidate| !Arc::ptr_eq(candidate, &listener)))
    }
    fn settle(&self) -> Pin<Box<dyn Future<Output = JudgeSettle> + Send>> {
        self.settle_seq.store(self.seq.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        let cell: SettleCell = Arc::new(Mutex::new(None));
        self.settles.lock().unwrap().push_back(Arc::clone(&cell));
        Box::pin(std::future::poll_fn(move |_cx| match cell.lock().unwrap().clone() {
            Some(settle) => Poll::Ready(settle),
            None => Poll::Pending,
        }))
    }
    fn dispose<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        self.disposed.fetch_add(1, Ordering::SeqCst);
        let error = self.dispose_error.load(Ordering::SeqCst);
        Box::pin(async move { if error { Err("teardown failed".into()) } else { Ok(()) } })
    }
}

struct RecordingSpawner {
    child: Arc<RecordingChild>,
    last_generation: AtomicUsize,
}

impl KibitzerChildSpawner for RecordingSpawner {
    fn spawn<'a>(&'a self, input: KibitzerChildSpawnInput) -> Pin<Box<dyn Future<Output = Result<Arc<dyn KibitzerChild>, crate::kibitzer_sidecar_model::KibitzerSidecarStartError>> + Send + 'a>> {
        self.last_generation.store(input.generation as usize, Ordering::SeqCst);
        let child = Arc::clone(&self.child);
        Box::pin(async move { Ok(child as Arc<dyn KibitzerChild>) })
    }
}

/// A `KibitzerWakeSpawn` that queues detached futures instead of running them, so teardown work is
/// observed only when the test drains the queue.
#[derive(Default)]
struct ManualSpawn {
    queue: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>,
}

impl ManualSpawn {
    fn spawner(self: &Arc<Self>) -> KibitzerWakeSpawn {
        let me = Arc::clone(self);
        Arc::new(move |future| me.queue.lock().unwrap().push(future))
    }
    fn drain(&self) {
        loop {
            let next = { self.queue.lock().unwrap().pop() };
            match next {
                Some(future) => {
                    let _ = poll_ready(future);
                }
                None => break,
            }
        }
    }
}

/// One poll with a no-op waker; the caller decides whether Pending is expected.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    use std::task::{Context as TaskContext, Waker};
    let waker = Waker::noop();
    let mut cx = TaskContext::from_waker(&waker);
    future.poll(&mut cx)
}

/// Drives an owned boxed future to Ready within a deterministic poll budget. `?Sized` is what lets
/// the SAME helper take `runner.start(..)` and the spawn queue's `Pin<Box<dyn Future>>` directly.
fn poll_ready<F: Future + ?Sized>(mut future: Pin<Box<F>>) -> F::Output {
    for _ in 0..64 {
        if let Poll::Ready(value) = poll_once(future.as_mut()) {
            return value;
        }
    }
    panic!("future did not resolve within the deterministic poll budget");
}

fn candidates() -> CollectedRecallCandidates {
    CollectedRecallCandidates {
        session_id: "s1".into(),
        identity: "agent".into(),
        candidates: vec![memory_core::recall::RecallCandidate {
            path: "a.md".into(),
            description: String::new(),
            excerpt: String::new(),
            score: 1.0,
        }],
        surfaced: std::collections::BTreeSet::new(),
        max_items: 2,
        transcript: vec![],
    }
}

fn request(cancel: bool, budget: usize) -> KibitzerWakeRequest {
    KibitzerWakeRequest {
        session_id: "s1".into(),
        candidates: candidates(),
        cancel: Arc::new(move || cancel),
        max_tool_budget: budget,
    }
}

fn runner(settle: JudgeSettle) -> (ResidentKibitzerRunner, Arc<RecordingChild>, Arc<RecordingSpawner>, Arc<ManualSpawn>) {
    let child = Arc::new(RecordingChild {
        nudges: Arc::new(Mutex::new(Vec::new())),
        tools: Arc::new(Mutex::new(Vec::new())),
        settles: Mutex::new(VecDeque::new()),
        seq: AtomicUsize::new(0),
        settle_seq: AtomicUsize::new(0),
        trigger_seq: AtomicUsize::new(0),
        settle,
        aborted: AtomicUsize::new(0),
        disposed: AtomicUsize::new(0),
        fail_follow_up: AtomicBool::new(false),
        dispose_error: AtomicBool::new(false),
    });
    let spawner = Arc::new(RecordingSpawner { child: Arc::clone(&child), last_generation: AtomicUsize::new(0) });
    let manual = Arc::new(ManualSpawn::default());
    let runner = ResidentKibitzerRunner::new(
        Arc::clone(&spawner) as Arc<dyn KibitzerChildSpawner>,
        manual.spawner(),
        KibitzerEventCaps::default(),
        None,
        Arc::new(|_| {}),
    );
    (runner, child, spawner, manual)
}

/// Starts one wake, proves the turn is LIVE while its settlement is held, then releases exactly that
/// settlement and drives the wake to completion.
fn run_wake(runner: &ResidentKibitzerRunner, child: &RecordingChild, cancel: bool, budget: usize) -> KibitzerWakeResult {
    let mut wake = runner.start(request(cancel, budget));
    assert!(poll_once(wake.as_mut()).is_pending(), "the turn stays live until its settlement is released");
    child.release_last_settle();
    poll_ready(wake)
}

#[test]
fn given_a_first_wake_when_run_then_settle_is_armed_before_the_trigger() {
    let (runner, child, _spawner, _manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let result = run_wake(&runner, &child, false, 8);
    assert_eq!(result.status, KibitzerWakeStatus::Completed);
    let settle_seq = child.settle_seq.load(Ordering::SeqCst);
    let trigger_seq = child.trigger_seq.load(Ordering::SeqCst);
    assert!(settle_seq >= 1 && trigger_seq >= 1 && settle_seq < trigger_seq, "settle ({settle_seq}) must precede trigger ({trigger_seq})");
}

#[test]
fn given_the_tool_budget_exceeded_when_a_tool_event_arrives_then_the_child_is_aborted_at_once() {
    let (runner, child, _spawner, _manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let mut wake = runner.start(request(false, 1));
    assert!(poll_once(wake.as_mut()).is_pending(), "the budget is charged on a LIVE turn");
    child.fire_tool(0, KibitzerChildObservation::ToolStart { tool_call_id: "call-1".into(), name: "read".into() });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 0);
    child.fire_tool(0, KibitzerChildObservation::ToolStart { tool_call_id: "call-2".into(), name: "grep".into() });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 1);
    child.release_last_settle();
    assert_eq!(poll_ready(wake).status, KibitzerWakeStatus::Cancelled);
}

#[test]
fn given_a_running_turn_when_steered_then_the_accumulator_is_preserved() {
    let (runner, child, _spawner, _manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let mut first = runner.start(request(false, 8));
    assert!(poll_once(first.as_mut()).is_pending(), "the first turn is still running");
    child.fire_nudge(0, RecallNudge { path: "a.md".into(), hint: "before steer".into() });
    let mut second = runner.start(request(false, 8));
    assert!(poll_once(second.as_mut()).is_pending(), "the steer joins the running turn");
    assert_eq!(child.armed_settles(), 2, "each start arms its own settlement");
    child.release_settle(1);
    let steered = poll_ready(second);
    assert_eq!(steered.nudges.len(), 1);
    assert_eq!(steered.nudges[0].hint, "before steer");
    assert!(poll_once(first.as_mut()).is_pending(), "the steer's settlement is its own one-shot");
    child.release_settle(0);
    let _ = poll_ready(first);
}

#[test]
fn given_a_cancel_flag_when_run_then_status_is_cancelled() {
    let (runner, child, _spawner, _manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    assert_eq!(run_wake(&runner, &child, true, 8).status, KibitzerWakeStatus::Cancelled);
}

#[test]
fn given_a_failed_settle_when_run_then_status_is_failed() {
    let (runner, child, _spawner, _manual) = runner(JudgeSettle { completed: false, cancelled: false, failure_message: Some("503 upstream".into()) });
    assert_eq!(run_wake(&runner, &child, false, 8).status, KibitzerWakeStatus::Failed);
}

#[test]
fn given_a_reseed_when_the_next_wake_runs_then_the_generation_is_monotonic() {
    let (runner, child, spawner, manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let _ = run_wake(&runner, &child, false, 8);
    assert_eq!(spawner.last_generation.load(Ordering::SeqCst), 1);
    runner.request_reseed("s1");
    manual.drain();
    let _ = run_wake(&runner, &child, false, 8);
    assert_eq!(spawner.last_generation.load(Ordering::SeqCst), 2);
    assert_eq!(runner.generation("s1"), 2);
}

#[test]
fn given_a_first_trigger_failure_when_run_then_the_child_is_disposed_and_unsubscribed() {
    let (runner, child, _spawner, manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    child.fail_follow_up.store(true, Ordering::SeqCst);
    assert_eq!(poll_ready(runner.start(request(false, 8))).status, KibitzerWakeStatus::Failed);
    manual.drain();
    assert_eq!(child.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(child.tool_listeners(), 0);
    assert_eq!(child.nudge_listeners(), 0);
}

#[test]
fn given_a_dispose_error_when_reseeded_then_the_dispose_is_still_attempted() {
    let (runner, child, _spawner, manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let _ = run_wake(&runner, &child, false, 8);
    child.dispose_error.store(true, Ordering::SeqCst);
    runner.request_reseed("s1");
    manual.drain();
    assert_eq!(child.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(child.tool_listeners(), 0);
}

#[test]
fn given_a_disposed_child_when_a_tool_event_arrives_then_the_listener_is_gone() {
    let (runner, child, _spawner, manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let _ = run_wake(&runner, &child, false, 8);
    assert_eq!(child.tool_listeners(), 1);
    runner.dispose("s1");
    manual.drain();
    assert_eq!(child.tool_listeners(), 0);
    assert_eq!(child.nudge_listeners(), 0);
    child.fire_all_tools(KibitzerChildObservation::ToolStart { tool_call_id: "call-1".into(), name: "read".into() });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 0);
}

#[test]
fn given_a_live_turn_when_only_non_start_observations_arrive_then_no_charge_until_the_second_start() {
    let (runner, child, _spawner, _manual) = runner(JudgeSettle { completed: true, cancelled: false, failure_message: None });
    let mut wake = runner.start(request(false, 1));
    assert!(poll_once(wake.as_mut()).is_pending(), "the budget is charged on a LIVE turn");

    child.fire_tool(0, KibitzerChildObservation::ToolEnd {
        tool_call_id: "call-0".into(),
        name: "read".into(),
        is_error: false,
        terminate: false,
        refusal: None,
    });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 0);
    child.fire_tool(0, KibitzerChildObservation::MessageEnd {
        message: maho_ext_api::AgentMessage::Llm(maho_ext_api::Message::User(maho_ext_api::UserMessage {
            content: maho_ext_api::UserContent::Text("hello".into()),
            timestamp: 0,
        })),
    });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 0);

    child.fire_tool(0, KibitzerChildObservation::ToolStart { tool_call_id: "call-1".into(), name: "read".into() });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 0);
    child.fire_tool(0, KibitzerChildObservation::ToolStart { tool_call_id: "call-2".into(), name: "grep".into() });
    assert_eq!(child.aborted.load(Ordering::SeqCst), 1);

    child.release_last_settle();
    assert_eq!(poll_ready(wake).status, KibitzerWakeStatus::Cancelled);
}
