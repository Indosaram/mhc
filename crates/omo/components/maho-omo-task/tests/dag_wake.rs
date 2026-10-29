use std::sync::{Arc, Mutex};
use idle_injection::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_omo_task::dag_wake::{create_dag_wake, DagWakeRun, DagWakeFailure, DAG_WAKE_MESSAGE_TYPE};
use senpi_task::completion::ParentState;

#[derive(Default)]
struct Coordinator { injections: Mutex<Vec<IdleInjection>>, calls: Mutex<Vec<&'static str>> }
impl IdleInjectionCoordinator for Coordinator {
    fn enqueue(&self, injection: IdleInjection) -> bool { self.injections.lock().expect("injections").push(injection); self.calls.lock().expect("calls").push("enqueue"); true }
    fn schedule_flush(&self) { self.calls.lock().expect("calls").push("schedule"); }
    fn flush_soon(&self) { self.calls.lock().expect("calls").push("soon"); }
    fn flush_on_idle(&self) -> usize { 0 }
    fn pending_count(&self) -> usize { 0 }
    fn remove(&self, _key: &str) -> bool { false }
}
fn run(id: &str) -> DagWakeRun<'_> { DagWakeRun { run_id: id, name: "release", parent_session_id: "parent" } }
fn wake(state: ParentState, coordinator: Arc<Coordinator>) -> maho_omo_task::dag_wake::DagWake {
    let port: Arc<dyn IdleInjectionCoordinator> = coordinator;
    create_dag_wake(port, Arc::new(move || state))
}

#[test]
fn idle_terminal_adds_directive_and_preserves_idle_coordinator_order() {
    let coordinator = Arc::new(Coordinator::default());
    let mut wake = wake(ParentState::Idle, coordinator.clone());
    wake.on_run_event(run("run"), "dag.run.completed", Some(Default::default()), None);
    assert_eq!(*coordinator.calls.lock().expect("calls"), ["enqueue", "soon"]);
    let injections = coordinator.injections.lock().expect("injections");
    assert_eq!(injections[0].source, IdleInjectionSource::DagRun);
    assert_eq!(injections[0].custom_type.as_deref(), Some(DAG_WAKE_MESSAGE_TYPE));
    assert!(injections[0].content.contains("\n\nDAG SUBAGENT COMPLETION - TREAT AS FALSE UNTIL YOU PROVE IT."));
}

#[test]
fn streaming_paused_run_preserves_reason_without_terminal_directive() {
    let coordinator = Arc::new(Coordinator::default());
    let mut wake = wake(ParentState::Streaming, coordinator.clone());
    wake.on_run_event(run("run"), "dag.run.paused", None, Some(DagWakeFailure { code: "session_shutdown", message: "ignored", node_id: None }));
    assert_eq!(*coordinator.calls.lock().expect("calls"), ["enqueue", "schedule"]);
    let injections = coordinator.injections.lock().expect("injections");
    assert!(injections[0].content.contains("paused (session_shutdown)"));
    assert_eq!(injections[0].details.as_ref().expect("details")["reason"], "session_shutdown");
    assert!(!injections[0].content.contains("DAG SUBAGENT COMPLETION"));
}

#[test]
fn transition_buffer_coalesces_by_run_until_matching_session_start() {
    let coordinator = Arc::new(Coordinator::default());
    let mut wake = wake(ParentState::SessionShutdown, coordinator.clone());
    wake.on_run_event(run("run"), "dag.run.paused", None, None);
    assert_eq!(wake.buffered_count("parent"), 1);
    wake.on_session_start(Some("foreign"));
    assert!(coordinator.injections.lock().expect("injections").is_empty());
    wake.on_session_start(Some("parent"));
    wake.on_session_start(Some("parent"));
    assert_eq!(coordinator.injections.lock().expect("injections").len(), 1);
    assert_eq!(wake.buffered_count("parent"), 0);
}
