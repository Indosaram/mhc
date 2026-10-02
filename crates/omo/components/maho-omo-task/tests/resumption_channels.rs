use std::sync::{Arc, Mutex};
use maho_ext_api::EventBus;
use maho_omo_task::resumption_channel_emitter::{ResumptionChannelEmitter, ResumptionChannelManager, RESUMPTION_CHANNEL_STATE_EVENT};
use senpi_task::state::{create_task_record, TaskRecord, TaskRecordInput, TaskStatus};

struct Manager(Mutex<Vec<TaskRecord>>);
impl ResumptionChannelManager for Manager {
    fn list(&self, _: &str) -> Vec<TaskRecord> { self.0.lock().expect("valid test state").clone() }
    fn was_background(&self, id: &str) -> bool { id == "background" }
    fn is_owned_team_member(&self, record: &TaskRecord, _: &str) -> bool { record.task_id == "owned" }
}

#[test]
fn publishes_only_count_changes_for_background_and_owned_nonterminal_tasks() {
    let mut record = create_task_record(TaskRecordInput {
        parent_session_id: "session".into(), root_session_id: "session".into(),
        execution_mode: "in-process".into(), model: "faux/faux-1".into(),
        ..Default::default()
    }, Some(1)).expect("valid test state");
    record.status = TaskStatus::Running;
    let mut background = record.clone(); background.task_id = "background".into();
    let mut owned = record.clone(); owned.task_id = "owned".into();
    record.task_id = "foreground".into();
    let manager = Arc::new(Manager(Mutex::new(vec![background, owned, record])));
    let events = EventBus::default();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let _subscription = events.on(RESUMPTION_CHANNEL_STATE_EVENT, Arc::new(move |event| {
        sink.lock().expect("valid test state").push(event.clone());
    }));
    let mut emitter = ResumptionChannelEmitter::new(events, manager.clone(), Arc::new(|| Some("session".into())));
    emitter.emit_if_changed();
    assert!(captured.lock().expect("valid test state").is_empty());
    emitter.emit_session_start();
    assert_eq!(captured.lock().expect("valid test state")[0]["activeCount"], 2);
    emitter.emit_if_changed();
    assert_eq!(captured.lock().expect("valid test state").len(), 1);
    manager.0.lock().expect("valid test state")[0].status = TaskStatus::Completed;
    emitter.emit_if_changed();
    assert_eq!(captured.lock().expect("valid test state")[1]["activeCount"], 1);
    emitter.emit_shutdown();
    assert_eq!(captured.lock().expect("valid test state")[2]["activeCount"], 0);
    emitter.emit_if_changed();
    assert_eq!(captured.lock().expect("valid test state").len(), 3);
}
