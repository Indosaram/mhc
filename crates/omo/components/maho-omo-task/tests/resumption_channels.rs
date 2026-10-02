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
#[test] fn missing_session_publishes_empty_start_and_shutdown() {
    let events=EventBus::default(); let captured=Arc::new(Mutex::new(Vec::new())); let sink=captured.clone(); let _subscription=events.on(RESUMPTION_CHANNEL_STATE_EVENT,Arc::new(move |value| sink.lock().expect("events").push(value.clone())));
    let mut emitter=ResumptionChannelEmitter::new(events,Arc::new(Manager(Mutex::new(vec![]))),Arc::new(|| None)); emitter.emit_session_start(); emitter.emit_shutdown();
    assert_eq!(*captured.lock().expect("events"),vec![serde_json::json!({"source":"senpi-task","activeCount":0,"channels":[]});2]);
}
#[test] fn started_channel_preserves_task_identity_description_and_creation_timestamp() {
    let mut record=create_task_record(TaskRecordInput { parent_session_id:"session".into(),task_summary:Some("Inspect continuation timing".into()),..Default::default() },Some(1000)).expect("record"); record.task_id="background".into(); record.created_at="1970-01-01T00:00:01.000Z".into(); let events=EventBus::default(); let captured=Arc::new(Mutex::new(Vec::new())); let sink=captured.clone(); let _subscription=events.on(RESUMPTION_CHANNEL_STATE_EVENT,Arc::new(move |value| sink.lock().expect("events").push(value.clone())));
    let mut emitter=ResumptionChannelEmitter::new(events,Arc::new(Manager(Mutex::new(vec![record]))),Arc::new(|| Some("session".into()))); emitter.emit_session_start(); assert_eq!(captured.lock().expect("events")[0]["channels"][0],serde_json::json!({"id":"background","description":"Inspect continuation timing","startedAtMs":1000}));
}
#[test] fn terminal_owned_member_never_keeps_wake_channel_alive() {
    let mut record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record"); record.task_id="owned".into(); record.status=TaskStatus::Completed; let events=EventBus::default(); let captured=Arc::new(Mutex::new(Vec::new())); let sink=captured.clone(); let _subscription=events.on(RESUMPTION_CHANNEL_STATE_EVENT,Arc::new(move |value| sink.lock().expect("events").push(value.clone())));
    let mut emitter=ResumptionChannelEmitter::new(events,Arc::new(Manager(Mutex::new(vec![record]))),Arc::new(|| Some("session".into()))); emitter.emit_session_start(); assert_eq!(captured.lock().expect("events")[0]["activeCount"],0);
}
