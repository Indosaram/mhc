use std::sync::{Arc,Mutex};
use maho_ext_api::EventBus;
use maho_omo_task::{dag_wake_source::{DagWakeSource,DAG_WAKE_SOURCE_STATE_EVENT},dag_status_ui::DagStatusUiManager,dag_tool::{DagToolDeps,run_dag_tool}};
use senpi_task::dag::{manager::{DagManager,DagRunSummary,DagManagerOptions,create_dag_manager},store::{DagStoreConfig,DagStoreOptions,create_dag_file_store},types::DagRunSnapshot};
use serde_json::{Value,json};
struct Runs { manager:DagManager,ghost:bool }
impl DagStatusUiManager for Runs {
    fn list(&self,session:&str)->Vec<DagRunSummary> { let mut runs=self.manager.list(session,None).expect("list"); if self.ghost { let mut ghost=runs[0].clone(); ghost.run_id="ghost".into(); runs.insert(0,ghost); } runs }
    fn snapshot(&self,run:&str,session:&str)->Option<DagRunSnapshot> { self.manager.snapshot(&run.into(),session).ok() }
}
fn fixture(ghost:bool)->(tempfile::TempDir,DagWakeSource,Arc<Mutex<Vec<Value>>>,maho_ext_api::BusSubscription) {
    let root=tempfile::tempdir().expect("root"); let store=create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store"); let manager=create_dag_manager(DagManagerOptions { store:Arc::new(store),new_run_id:Some(Arc::new(|| "run-1".into())),now:Some(Arc::new(|| 1000)),materialize_skills:None,settings:None }); let deps=DagToolDeps { manager:manager.clone(),parent_session_id:Arc::new(|| "parent".into()),root_session_id:Arc::new(|| "parent".into()),wait:None,cancel:None }; run_dag_tool(&deps,serde_json::from_value(json!({"action":"start","definition":{"key":"plan","name":"release","nodes":[{"id":"a","prompt":"work","category":"quick"}]}})).expect("input")).expect("start");
    let events=EventBus::default(); let emitted=Arc::new(Mutex::new(vec![])); let sink=emitted.clone(); let subscription=events.on(DAG_WAKE_SOURCE_STATE_EVENT,Arc::new(move |value| sink.lock().expect("events").push(value.clone()))); (root,DagWakeSource { events,manager:Arc::new(Runs { manager,ghost }),session_id:Arc::new(|| Some("parent".into())) },emitted,subscription)
}
#[test] fn pending_run_reports_creation_time_and_name() { let (_root,source,events,_subscription)=fixture(false); source.publish_live(); assert_eq!(events.lock().expect("events")[0],json!({"source":"omo-dag","activeCount":1,"channels":[{"id":"run-1","description":"release","startedAtMs":1000}]})); }
#[test] fn pruned_run_does_not_hide_surviving_channel() { let (_root,source,events,_subscription)=fixture(true); source.publish_live(); let events=events.lock().expect("events"); assert_eq!(events[0]["activeCount"],1); assert_eq!(events[0]["channels"][0]["id"],"run-1"); }
#[test] fn missing_session_publishes_cleared_state() { let (_root,mut source,events,_subscription)=fixture(false); source.session_id=Arc::new(|| None); source.publish_live(); assert_eq!(events.lock().expect("events")[0],json!({"source":"omo-dag","activeCount":0,"channels":[]})); }
#[test] fn shutdown_clears_live_channels() { let (_root,source,events,_subscription)=fixture(false); source.publish_live(); source.emit_shutdown(); let events=events.lock().expect("events"); assert_eq!(events.len(),2); assert_eq!(events[1],json!({"source":"omo-dag","activeCount":0,"channels":[]})); }
