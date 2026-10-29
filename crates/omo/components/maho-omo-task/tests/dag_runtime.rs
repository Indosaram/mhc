use std::sync::{Arc, Mutex};
use idle_injection::{IdleInjection, IdleInjectionCoordinator};
use maho_omo_task::dag_wake::{create_dag_wake, DagWakeRun};
use senpi_task::completion::ParentState;

#[derive(Default)]
struct Coordinator(Mutex<Vec<IdleInjection>>);
impl IdleInjectionCoordinator for Coordinator {
    fn enqueue(&self, event: IdleInjection) -> bool { self.0.lock().expect("events").push(event); true }
    fn schedule_flush(&self) {}
    fn flush_soon(&self) {}
    fn flush_on_idle(&self) -> usize { 0 }
    fn pending_count(&self) -> usize { self.0.lock().expect("events").len() }
    fn remove(&self, _key: &str) -> bool { false }
}

#[test]
fn given_paused_run_when_notice_is_delivered_then_it_is_buffered_until_session_start() {
    let coordinator = Arc::new(Coordinator::default());
    let port: Arc<dyn IdleInjectionCoordinator> = coordinator.clone();
    let mut wake = create_dag_wake(port, Arc::new(|| ParentState::SessionSwitching));
    wake.on_run_event(DagWakeRun { run_id:"run", name:"workflow", parent_session_id:"parent" }, "dag.run.paused", None, None);
    assert!(coordinator.0.lock().expect("events").is_empty());
    assert_eq!(wake.buffered_count("parent"), 1);
    wake.on_session_start(Some("parent"));
    assert_eq!(coordinator.0.lock().expect("events").len(), 1);
}

#[test]
fn given_terminal_event_when_sequence_is_replayed_then_terminal_wake_is_deduplicated() {
    let coordinator = Arc::new(Coordinator::default());
    let port: Arc<dyn IdleInjectionCoordinator> = coordinator.clone();
    let mut wake = create_dag_wake(port, Arc::new(|| ParentState::Idle));
    let run = DagWakeRun { run_id:"run", name:"workflow", parent_session_id:"parent" };
    wake.on_run_event(DagWakeRun { run_id:run.run_id, name:run.name, parent_session_id:run.parent_session_id }, "dag.run.completed", Some(Default::default()), None);
    wake.on_run_event(run, "dag.run.completed", Some(Default::default()), None);
    assert_eq!(coordinator.0.lock().expect("events").len(), 2);
}

