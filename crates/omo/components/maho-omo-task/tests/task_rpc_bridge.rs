use std::sync::{Arc,Mutex};
use maho_ext_api::{ExtensionApi,LoadedExtension,SourceInfo,ExtensionSessionProfile,EventBus,ExtensionRuntime};
use maho_omo_task::task_rpc_bridge::{TaskRpcBridge,wire_task_rpc_bridge};
use senpi_task::{manager::{create_task_manager,types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,TaskManagerOptions,ResolvedChildPlan}},store::{StateDirConfig,TaskRecordStore},state::{TaskRecordInput,create_task_record}};
use serde_json::{Value,json};
struct NoLaunch; impl ManagedRunner for NoLaunch { fn start(&self,_:&ManagedStartSpec)->ManagedRunnerResult { panic!("unexpected launch") } }
struct Fixture { bridge:Arc<TaskRpcBridge>,store:TaskRecordStore,events:Arc<Mutex<Vec<Value>>>,session:Arc<Mutex<Option<String>>>,api:ExtensionApi,_root:tempfile::TempDir }
fn fixture()->Fixture {
    let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None });
    let manager=create_task_manager(TaskManagerOptions::new(store.clone(),ManagedRunners { in_process:Arc::new(NoLaunch),process:Arc::new(NoLaunch) },Arc::new(|_| Ok(ResolvedChildPlan { model:"faux/faux".into(),..Default::default() })),root.path().to_string_lossy()));
    let mut api=ExtensionApi::new(LoadedExtension::new("task","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    let session=Arc::new(Mutex::new(Some("parent".into()))); let active=session.clone(); let events=Arc::new(Mutex::new(Vec::new())); let emitted=events.clone();
    let bridge=wire_task_rpc_bridge(&mut api,Arc::new(manager),Arc::new(move || active.lock().expect("session").clone()),store.state_dir().to_string_lossy().into_owned(),Arc::new(move |name,value| { assert_eq!(name,"omo.task.updated"); emitted.lock().expect("events").push(value); })).expect("bridge");
    Fixture { bridge,store,events,session,api,_root:root }
}
#[test] fn attached_empty_session_emits_once_per_fingerprint() { let f=fixture(); f.bridge.attach(); f.bridge.sync(); let events=f.events.lock().expect("events"); assert_eq!(events.len(),1); assert_eq!(events[0],json!({"parent_session_id":"parent","tasks":[]})); }
#[test] fn detached_control_is_unavailable_before_parsing() { let f=fixture(); assert_eq!(f.bridge.request("omo.task.send",&Value::Null).expect("request")["kind"],"unavailable"); }
#[test] fn attached_control_parses_before_manager_access() { let f=fixture(); f.bridge.attach(); assert_eq!(f.bridge.request("omo.task.cancel",&json!({"task_id":"bad"})).expect("request")["kind"],"invalid_arguments"); }
#[test] fn foreign_task_send_is_indistinguishable_from_missing() { let f=fixture(); let record=create_task_record(TaskRecordInput { parent_session_id:"foreign".into(),..Default::default() },Some(1)).expect("record"); f.store.save(&record).expect("save"); f.bridge.attach(); assert_eq!(f.bridge.request("omo.task.send",&json!({"to":record.task_id,"message":"work"})).expect("request"),json!({"kind":"not_found","reason":"Task not found."})); }
#[test] fn disposal_prevents_reattach_and_control() { let f=fixture(); f.bridge.attach(); f.bridge.dispose(); f.bridge.attach(); assert_eq!(f.events.lock().expect("events").len(),1); assert_eq!(f.bridge.request("omo.task.output",&json!({"task_id":"st_missing"})).expect("request")["kind"],"unavailable"); }
#[test] fn reattach_emits_new_consumer_snapshot() { let f=fixture(); f.bridge.attach(); f.bridge.attach(); assert_eq!(f.events.lock().expect("events").len(),2); }
#[test] fn missing_session_emits_no_snapshot() { let f=fixture(); *f.session.lock().expect("session")=None; f.bridge.attach(); assert!(f.events.lock().expect("events").is_empty()); }
#[test] fn registers_three_control_handlers() { let f=fixture(); for name in ["omo.task.send","omo.task.cancel","omo.task.output"] { assert!(f.api.registered.rpc_handlers.contains_key(name)); } }
#[test] fn every_foreign_control_returns_generic_not_found() {
    let f=fixture(); let record=create_task_record(TaskRecordInput { parent_session_id:"foreign".into(),root_session_id:"foreign".into(),..Default::default() },Some(1)).expect("record"); f.store.save(&record).expect("save"); f.bridge.attach();
    for (name,input) in [("omo.task.send",json!({"to":record.task_id,"message":"work"})),("omo.task.cancel",json!({"task_id":record.task_id})),("omo.task.output",json!({"task_id":record.task_id,"mode":"status"}))] { assert_eq!(f.bridge.request(name,&input).expect("control"),json!({"kind":"not_found","reason":"Task not found."})); }
}
#[test] fn all_controls_fail_closed_when_detached() {
    let f=fixture(); for name in ["omo.task.send","omo.task.cancel","omo.task.output"] { assert_eq!(f.bridge.request(name,&Value::Null).expect("control")["kind"],"unavailable"); }
}
#[test] fn snapshot_cap_keeps_live_tasks_before_recent_terminals() {
    let f=fixture();
    let mut live_id=String::new();
    for index in 0..260 { let mut record=create_task_record(TaskRecordInput { parent_session_id:"parent".into(),root_session_id:"parent".into(),..Default::default() },Some(index+1)).expect("record"); record.status=if index==0 { live_id=record.task_id.clone(); senpi_task::state::TaskStatus::Running } else { senpi_task::state::TaskStatus::Completed }; f.store.save(&record).expect("save"); }
    f.bridge.attach(); let events=f.events.lock().expect("events"); let payload=&events[0]; assert_eq!(payload["tasks"].as_array().expect("tasks").len(),256); assert_eq!(payload["truncated_tasks"],4); assert_eq!(payload["tasks"][0]["task_id"],live_id);
}
#[test] fn terminal_snapshot_uses_durable_run_stats() {
    let f=fixture(); let mut record=create_task_record(TaskRecordInput { parent_session_id:"parent".into(),..Default::default() },Some(1)).expect("record"); record.status=senpi_task::state::TaskStatus::Completed; record.run_stats=Some(senpi_task::state::TaskRunStats { runtime_ms:500,..Default::default() }); f.store.save(&record).expect("save"); f.bridge.attach(); assert_eq!(f.events.lock().expect("events")[0]["tasks"][0]["run_stats"]["runtime_ms"],500);
}
#[test] fn snapshot_cap_retains_newest_live_records_in_stable_order() {
    let f=fixture(); let mut ids=Vec::new(); for index in 0..260 { let mut record=create_task_record(TaskRecordInput { parent_session_id:"parent".into(),root_session_id:"parent".into(),..Default::default() },Some(index+1)).expect("record"); record.status=senpi_task::state::TaskStatus::Running; ids.push(record.task_id.clone()); f.store.save(&record).expect("save"); }
    f.bridge.attach(); let events=f.events.lock().expect("events"); let tasks=events[0]["tasks"].as_array().expect("tasks"); assert_eq!(events[0]["truncated_tasks"],4); assert_eq!(tasks.iter().map(|task| task["task_id"].as_str().expect("id")).collect::<Vec<_>>(),ids[4..].iter().map(String::as_str).collect::<Vec<_>>()); drop(events); f.bridge.dispose();
}
#[tokio::test] async fn registered_output_reads_owned_record_and_invalid_controls_preserve_state() {
    let f=fixture(); let record=create_task_record(TaskRecordInput { parent_session_id:"parent".into(),..Default::default() },Some(1)).expect("record"); f.store.save(&record).expect("save"); f.bridge.attach();
    let result=(f.api.registered.rpc_handlers["omo.task.output"])(json!({"task_id":record.task_id,"mode":"status"})).await.expect("status"); assert_eq!(result["kind"],"status"); assert_eq!(result["snapshot"]["task_id"],record.task_id); assert_eq!(result["snapshot"]["parent_session_id"],"parent");
    for (name,value) in [("omo.task.send",json!({"to":record.task_id})),("omo.task.send",Value::Null),("omo.task.output",json!({"task_id":record.task_id,"mode":"stream"})),("omo.task.cancel",json!({"task_id":record.task_id,"reason":42}))] { let result=(f.api.registered.rpc_handlers[name])(value).await.expect("invalid"); assert_eq!(result["kind"],"invalid_arguments"); }
    assert_eq!(f.store.load(&record.task_id).expect("load").expect("record"),record); f.bridge.dispose();
}
