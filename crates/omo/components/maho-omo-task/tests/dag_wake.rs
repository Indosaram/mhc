use idle_injection::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_omo_task::dag_wake::{DAG_WAKE_MESSAGE_TYPE, DagWakeFailure, DagWakeRun, create_dag_wake};
use senpi_task::completion::ParentState;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Coordinator {
    injections: Mutex<Vec<IdleInjection>>,
    calls: Mutex<Vec<&'static str>>,
}
impl IdleInjectionCoordinator for Coordinator {
    fn enqueue(&self, injection: IdleInjection) -> bool {
        self.injections.lock().expect("injections").push(injection);
        self.calls.lock().expect("calls").push("enqueue");
        true
    }
    fn schedule_flush(&self) {
        self.calls.lock().expect("calls").push("schedule");
    }
    fn flush_soon(&self) {
        self.calls.lock().expect("calls").push("soon");
    }
    fn flush_on_idle(&self) -> usize {
        0
    }
    fn pending_count(&self) -> usize {
        0
    }
    fn remove(&self, _key: &str) -> bool {
        false
    }
}
fn run(id: &str) -> DagWakeRun<'_> {
    DagWakeRun {
        run_id: id,
        name: "release",
        parent_session_id: "parent",
    }
}
fn wake(state: ParentState, coordinator: Arc<Coordinator>) -> maho_omo_task::dag_wake::DagWake {
    let port: Arc<dyn IdleInjectionCoordinator> = coordinator;
    create_dag_wake(port, Arc::new(move || state))
}

#[test]
fn idle_terminal_adds_directive_and_preserves_idle_coordinator_order() {
    let coordinator = Arc::new(Coordinator::default());
    let mut wake = wake(ParentState::Idle, coordinator.clone());
    wake.on_run_event(
        run("run"),
        "dag.run.completed",
        Some(Default::default()),
        None,
    );
    assert_eq!(
        *coordinator.calls.lock().expect("calls"),
        ["enqueue", "soon"]
    );
    let injections = coordinator.injections.lock().expect("injections");
    assert_eq!(injections[0].source, IdleInjectionSource::DagRun);
    assert_eq!(
        injections[0].custom_type.as_deref(),
        Some(DAG_WAKE_MESSAGE_TYPE)
    );
    assert!(
        injections[0]
            .content
            .contains("\n\nDAG SUBAGENT COMPLETION - TREAT AS FALSE UNTIL YOU PROVE IT.")
    );
}

#[test]
fn streaming_paused_run_preserves_reason_without_terminal_directive() {
    let coordinator = Arc::new(Coordinator::default());
    let mut wake = wake(ParentState::Streaming, coordinator.clone());
    wake.on_run_event(
        run("run"),
        "dag.run.paused",
        None,
        Some(DagWakeFailure {
            code: "session_shutdown",
            message: "ignored",
            node_id: None,
        }),
    );
    assert_eq!(
        *coordinator.calls.lock().expect("calls"),
        ["enqueue", "schedule"]
    );
    let injections = coordinator.injections.lock().expect("injections");
    assert!(injections[0].content.contains("paused (session_shutdown)"));
    assert_eq!(
        injections[0].details.as_ref().expect("details")["reason"],
        "session_shutdown"
    );
    assert!(!injections[0].content.contains("DAG SUBAGENT COMPLETION"));
}

#[test]
fn transition_buffer_coalesces_by_run_until_matching_session_start() {
    let coordinator = Arc::new(Coordinator::default());
    let mut wake = wake(ParentState::SessionShutdown, coordinator.clone());
    wake.on_run_event(run("run"), "dag.run.paused", None, None);
    assert_eq!(wake.buffered_count("parent"), 1);
    wake.on_session_start(Some("foreign"));
    assert!(
        coordinator
            .injections
            .lock()
            .expect("injections")
            .is_empty()
    );
    wake.on_session_start(Some("parent"));
    wake.on_session_start(Some("parent"));
    assert_eq!(coordinator.injections.lock().expect("injections").len(), 1);
    assert_eq!(wake.buffered_count("parent"), 0);
}

/// A coordinator whose answer to `enqueue` is switchable, like a queue retired by a session
/// shutdown and live again after the next session start. `attempts` counts every hand-off;
/// `accepted` holds only the injections the queue actually took.
#[derive(Default)]
struct RefusingCoordinator {
    accepting: std::sync::atomic::AtomicBool,
    attempts: Mutex<Vec<String>>,
    accepted: Mutex<Vec<IdleInjection>>,
    calls: Mutex<Vec<&'static str>>,
}
impl IdleInjectionCoordinator for RefusingCoordinator {
    fn enqueue(&self, injection: IdleInjection) -> bool {
        self.attempts
            .lock()
            .expect("attempts")
            .push(injection.key.clone());
        if !self.accepting.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        self.accepted.lock().expect("accepted").push(injection);
        true
    }
    fn schedule_flush(&self) {
        self.calls.lock().expect("calls").push("schedule");
    }
    fn flush_soon(&self) {
        self.calls.lock().expect("calls").push("soon");
    }
    fn flush_on_idle(&self) -> usize {
        0
    }
    fn pending_count(&self) -> usize {
        self.accepted.lock().expect("accepted").len()
    }
    fn remove(&self, _key: &str) -> bool {
        false
    }
}
fn refusing_wake(
    state: ParentState,
    coordinator: Arc<RefusingCoordinator>,
) -> maho_omo_task::dag_wake::DagWake {
    let port: Arc<dyn IdleInjectionCoordinator> = coordinator;
    create_dag_wake(port, Arc::new(move || state))
}

#[test]
fn a_refused_idle_wake_stays_buffered_until_a_session_start_the_queue_accepts() {
    let coordinator = Arc::new(RefusingCoordinator::default());
    let mut wake = refusing_wake(ParentState::Idle, coordinator.clone());
    wake.on_run_event(
        run("run"),
        "dag.run.completed",
        Some(Default::default()),
        None,
    );
    // The refusal kept the wake; the flush still ran for whatever else is queued.
    assert_eq!(wake.buffered_count("parent"), 1);
    assert_eq!(
        *coordinator.attempts.lock().expect("attempts"),
        ["dag-run:run"]
    );
    assert!(coordinator.accepted.lock().expect("accepted").is_empty());
    assert_eq!(*coordinator.calls.lock().expect("calls"), ["soon"]);
    // A second refused wake for the same run collapses onto the buffered one.
    wake.on_run_event(
        run("run"),
        "dag.run.completed",
        Some(Default::default()),
        None,
    );
    assert_eq!(wake.buffered_count("parent"), 1);

    // A session start while the queue still refuses must not drop the wake.
    wake.on_session_start(Some("parent"));
    assert_eq!(wake.buffered_count("parent"), 1);
    assert!(coordinator.accepted.lock().expect("accepted").is_empty());

    // Once the queue accepts, the next session start replays it exactly once.
    coordinator
        .accepting
        .store(true, std::sync::atomic::Ordering::SeqCst);
    wake.on_session_start(Some("parent"));
    assert_eq!(wake.buffered_count("parent"), 0);
    {
        let accepted = coordinator.accepted.lock().expect("accepted");
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].key, "dag-run:run");
        assert_eq!(accepted[0].source, IdleInjectionSource::DagRun);
        assert!(
            accepted[0]
                .content
                .contains("DAG SUBAGENT COMPLETION - TREAT AS FALSE UNTIL YOU PROVE IT.")
        );
        assert!(
            accepted[0].on_delivery_failed.is_some(),
            "a wake with no retry owner after enqueue must register a delivery-failure callback"
        );
    }
    wake.on_session_start(Some("parent"));
    assert_eq!(coordinator.accepted.lock().expect("accepted").len(), 1);
}

#[test]
fn a_refused_streaming_wake_is_buffered_instead_of_dropped() {
    let coordinator = Arc::new(RefusingCoordinator::default());
    let mut wake = refusing_wake(ParentState::Streaming, coordinator.clone());
    wake.on_run_event(run("run"), "dag.run.paused", None, None);
    assert_eq!(wake.buffered_count("parent"), 1);
    assert_eq!(*coordinator.calls.lock().expect("calls"), ["schedule"]);
    coordinator
        .accepting
        .store(true, std::sync::atomic::Ordering::SeqCst);
    wake.on_session_start(Some("parent"));
    assert_eq!(wake.buffered_count("parent"), 0);
    let accepted = coordinator.accepted.lock().expect("accepted");
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].key, "dag-run:run");
    assert!(accepted[0].content.contains("paused"));
}
