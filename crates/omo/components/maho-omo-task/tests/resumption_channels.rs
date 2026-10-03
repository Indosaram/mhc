use std::sync::{Arc, Mutex};
use maho_ext_api::EventBus;
use serde_json::json;
use maho_omo_task::resumption_channel_emitter::{ResumptionChannelEmitter, ResumptionChannelManager, RESUMPTION_CHANNEL_STATE_EVENT};
use senpi_task::state::{create_task_record, TaskRecord, TaskRecordInput, TaskStatus};

struct Manager(Mutex<Vec<TaskRecord>>);
#[test] fn background_and_terminal_records_skip_ownership_and_empty_event_bus_is_safe() {
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct CountingManager { records:Vec<TaskRecord>,lookups:Arc<AtomicUsize> }
    impl ResumptionChannelManager for CountingManager {
        fn list(&self,_:&str)->Vec<TaskRecord> { self.records.clone() }
        fn was_background(&self,id:&str)->bool { id=="background" }
        fn is_owned_team_member(&self,_:&TaskRecord,_:&str)->bool { self.lookups.fetch_add(1,Ordering::SeqCst); false }
    }
    let mut background=create_task_record(TaskRecordInput::default(),Some(1)).expect("record"); background.task_id="background".into();
    let mut terminal=background.clone(); terminal.task_id="terminal".into(); terminal.status=TaskStatus::Completed;
    let mut foreground=background.clone(); foreground.task_id="foreground".into();
    let lookups=Arc::new(AtomicUsize::new(0));
    let mut emitter=ResumptionChannelEmitter::new(EventBus::default(),Arc::new(CountingManager { records:vec![background,terminal,foreground],lookups:lookups.clone() }),Arc::new(|| Some("session".into())));
    emitter.emit_if_changed(); assert_eq!(lookups.load(Ordering::SeqCst),0);
    emitter.emit_session_start(); assert_eq!(lookups.load(Ordering::SeqCst),1);
    emitter.emit_if_changed(); assert_eq!(lookups.load(Ordering::SeqCst),2);
    emitter.emit_shutdown(); emitter.emit_if_changed(); assert_eq!(lookups.load(Ordering::SeqCst),2);
}
#[test] fn production_manager_resolves_owned_members_from_real_runtime() {
    use maho_omo_task::resumption_channel_emitter::TaskResumptionChannelManager;
    use senpi_task::{manager::{create_task_manager,types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,TaskManagerOptions,ResolvedChildPlan}},store::{StateDirConfig,TaskRecordStore},team::{liveness_ownership::TeamMemberOwnershipDeps,runtime_config::{TeamTaskBounds,to_team_core_config},storage::team_storage_base_dir,normalize::normalize_senpi_team_spec}};
    struct NoLaunch; impl ManagedRunner for NoLaunch { fn start(&self,_:&ManagedStartSpec)->ManagedRunnerResult { panic!("unexpected launch") } }
    let root=tempfile::tempdir().expect("root"); let state_dir=StateDirConfig { project_dir:root.path().into(),task_state_dir:None }; let store=TaskRecordStore::new(&state_dir); let bounds=TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 };
    let config=to_team_core_config(&bounds,&team_storage_base_dir(&state_dir).to_string_lossy()).expect("config"); let spec=normalize_senpi_team_spec(&json!({"members":[{"name":"beta","kind":"category","category":"quick","prompt":"work"}]}),"squad",None).expect("spec");
    let runtime=team_core::team_state_store::create_runtime_state(&spec,Some("parent"),team_core::types::SpecSource::Project,&config).expect("runtime");
    let record=create_task_record(TaskRecordInput { parent_session_id:"parent".into(),name:Some(format!("team:{}:beta",runtime.team_run_id)),..Default::default() },Some(1)).expect("record"); store.save(&record).expect("save");
    let manager=create_task_manager(TaskManagerOptions::new(store,ManagedRunners { in_process:Arc::new(NoLaunch),process:Arc::new(NoLaunch) },Arc::new(|_| Ok(ResolvedChildPlan { model:"faux/faux".into(),..Default::default() })),root.path().to_string_lossy()));
    let view=TaskResumptionChannelManager { manager:Arc::new(manager),ownership:TeamMemberOwnershipDeps { state_dir,team_bounds:bounds,load_runtime_state:None } };
    assert_eq!(view.list("parent"),vec![record.clone()]); assert!(view.list("foreign").is_empty()); assert!(!view.was_background(&record.task_id)); assert!(view.is_owned_team_member(&record,"parent")); assert!(!view.is_owned_team_member(&record,"foreign"));
    let events=EventBus::default(); let received=Arc::new(Mutex::new(vec![])); let sink=received.clone(); let _subscription=events.on(RESUMPTION_CHANNEL_STATE_EVENT,Arc::new(move |event| sink.lock().expect("events").push(event.clone())));
    let mut emitter=ResumptionChannelEmitter::new(events,Arc::new(view),Arc::new(|| Some("parent".into()))); emitter.emit_session_start();
    { let events=received.lock().expect("events"); assert_eq!(events[0]["activeCount"],1); assert_eq!(events[0]["channels"][0]["id"],record.task_id); }
    emitter.emit_shutdown(); assert_eq!(received.lock().expect("events")[1]["activeCount"],0);
}
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
