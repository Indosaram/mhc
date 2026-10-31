use crate::support;
use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use maho_ext_api::ExtensionMode;
use maho_omo_task::{runtime_context::TaskRuntimeContext,status_ui::{TaskStatusUi,StatusUiTimers,StatusUiManager}};
use senpi_task::{state::{TaskRecord,TaskRecordInput,TaskStatus,create_task_record},manager::child_handle::{ManagedChildListener,Unsubscribe}};
type TimerCallback=Box<dyn FnOnce()+Send>;
#[derive(Default)] struct Timers { next:Mutex<u64>,pending:Mutex<BTreeMap<u64,TimerCallback>> }
impl StatusUiTimers for Timers {
    fn set(&self,callback:TimerCallback,ms:u64)->u64 { assert_eq!(ms,250); let mut next=self.next.lock().expect("next"); *next+=1; self.pending.lock().expect("pending").insert(*next,callback); *next }
    fn clear(&self,id:u64) { self.pending.lock().expect("pending").remove(&id); }
}
impl Timers { fn fire(&self) { let callbacks=std::mem::take(&mut *self.pending.lock().expect("pending")); for (_,callback) in callbacks { callback(); } } fn count(&self)->usize { self.pending.lock().expect("pending").len() } }
struct Manager { records:Mutex<Vec<TaskRecord>>,background:bool,listeners:Arc<Mutex<BTreeMap<String,ManagedChildListener>>> }
impl StatusUiManager for Manager {
    fn list(&self,session:&str)->Vec<TaskRecord> { self.records.lock().expect("records").iter().filter(|record| record.parent_session_id==session).cloned().collect() }
    fn has_background_surface(&self)->bool { self.background }
    fn was_background(&self,_:&str)->bool { self.background }
    fn subscribe_child(&self,id:&str,listener:ManagedChildListener)->Option<Unsubscribe> { self.listeners.lock().expect("listeners").insert(id.into(),listener); let listeners=self.listeners.clone(); let id=id.to_owned(); Some(Box::new(move || { listeners.lock().expect("listeners").remove(&id); })) }
}
fn fixture(background:bool,mode:ExtensionMode)->(Arc<TaskStatusUi>,Arc<Timers>,Arc<support::Ui>,Arc<Manager>) {
    let record=create_task_record(TaskRecordInput { parent_session_id:"session".into(),..Default::default() },Some(1)).expect("record"); let manager=Arc::new(Manager { records:Mutex::new(vec![record]),background,listeners:Arc::new(Mutex::new(BTreeMap::new())) }); let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); context.mode=mode; let mut runtime=TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context); let timers=Arc::new(Timers::default()); (TaskStatusUi::new(manager.clone(),Arc::new(Mutex::new(runtime)),timers.clone(),Arc::new(|| 1000),Arc::new(|| Some(80))),timers,ui,manager)
}
fn footer(ui:&support::Ui)->Option<String> { ui.statuses.lock().expect("statuses").get("omo-task").cloned() }
#[test] fn static_status_counts_live_task_without_widget_or_timer() { let (status,timers,ui,_)=fixture(false,ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),0); assert_eq!(footer(&ui).as_deref(),Some("1 subagent")); assert!(ui.widgets.lock().expect("widgets").iter().all(|widget| widget.0.is_none())); }
#[test] fn count_is_scoped_to_the_session_and_reports_failures() {
    let (status,timers,ui,manager)=fixture(false,ExtensionMode::Tui);
    { let mut records=manager.records.lock().expect("records"); let mut second=records[0].clone(); second.task_id="st_second".into(); records.push(second); let mut failed=records[0].clone(); failed.task_id="st_failed".into(); failed.status=TaskStatus::Error; records.push(failed); let mut foreign=records[0].clone(); foreign.parent_session_id="foreign".into(); records.push(foreign); }
    status.sync_now(); assert_eq!(footer(&ui).as_deref(),Some("2 subagents 1 failed")); assert_eq!(timers.count(),0);
}
#[test] fn failures_without_live_tasks_name_the_subject() { let (status,_timers,ui,manager)=fixture(false,ExtensionMode::Tui); manager.records.lock().expect("records")[0].status=TaskStatus::Error; status.sync_now(); assert_eq!(footer(&ui).as_deref(),Some("subagents 1 failed")); }
#[test] fn rpc_mode_does_not_publish_status() { let (status,timers,ui,_)=fixture(true,ExtensionMode::Rpc); status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.statuses.lock().expect("statuses").is_empty()); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn background_surface_counts_solo_and_process_members() {
    struct Spend;
    impl StatusUiManager for Spend {
        fn list(&self,_:&str)->Vec<TaskRecord> { ["st_solo","st_member"].into_iter().map(|id| { let mut record=create_task_record(TaskRecordInput { parent_session_id:"session".into(),..Default::default() },Some(1)).expect("record"); record.task_id=id.into(); if id=="st_member" { record.execution_mode="process".into(); record.pid=Some(4242); } record }).collect() }
        fn has_background_surface(&self)->bool { true } fn was_background(&self,_:&str)->bool { true }
    }
    let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); context.mode=ExtensionMode::Tui; let mut runtime=TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context); let timers=Arc::new(Timers::default()); let status=TaskStatusUi::new(Arc::new(Spend),Arc::new(Mutex::new(runtime)),timers.clone(),Arc::new(|| 1000),Arc::new(|| None)); status.sync_now(); assert_eq!(footer(&ui).as_deref(),Some("2 subagents")); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn uncaptured_runtime_never_queries_manager() {
    struct Unavailable;
    impl StatusUiManager for Unavailable { fn list(&self,_:&str)->Vec<TaskRecord> { panic!("uncaptured runtime must not query tasks") } }
    let timers=Arc::new(Timers::default()); let status=TaskStatusUi::new(Arc::new(Unavailable),Arc::new(Mutex::new(TaskRuntimeContext::new("/tmp".into()))),timers.clone(),Arc::new(|| 1000),Arc::new(|| Some(80)));
    status.sync_now(); assert_eq!(timers.count(),0); status.dispose();
}
#[test] fn non_background_tasks_are_left_out_on_the_background_surface() {
    let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui);
    struct Foreground(Arc<Manager>);
    impl StatusUiManager for Foreground { fn list(&self,session:&str)->Vec<TaskRecord> { self.0.list(session) } fn has_background_surface(&self)->bool { true } fn was_background(&self,_:&str)->bool { false } }
    let status=TaskStatusUi::new(Arc::new(Foreground(manager)),status.runtime.clone(),timers.clone(),Arc::new(|| 1000),Arc::new(|| Some(120))); status.sync_now(); assert_eq!(footer(&ui),None); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn dag_owned_background_tasks_stay_out_of_the_task_count() {
    let (status,_timers,ui,manager)=fixture(true,ExtensionMode::Tui);
    { let mut records=manager.records.lock().expect("records"); let mut node=records[0].clone(); node.task_id="st_dag_node".into(); node.owner=Some(senpi_task::shared::DagTaskOwner { kind:senpi_task::shared::DagOwnerKind::Dag,run_id:"dag_run".into(),node_id:"a".into(),fingerprint:"fp".into() }); records.push(node); }
    status.sync_now(); assert_eq!(footer(&ui).as_deref(),Some("1 subagent"));
}
#[test] fn terminal_set_clears_status() { let (status,timers,ui,manager)=fixture(false,ExtensionMode::Tui); status.sync_now(); assert!(footer(&ui).is_some()); manager.records.lock().expect("records")[0].status=TaskStatus::Completed; status.sync_now(); assert_eq!(timers.count(),0); assert_eq!(footer(&ui),None); }
#[test] fn repeated_schedule_coalesces_to_one_render() { let (status,timers,ui,_)=fixture(false,ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); assert_eq!(footer(&ui),None); timers.fire(); assert_eq!(footer(&ui).as_deref(),Some("1 subagent")); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); assert_eq!(timers.count(),0); }
#[test] fn subscribes_before_debounce_and_dispose_removes_listener_and_timer() { let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); status.schedule_sync(); assert_eq!(manager.listeners.lock().expect("listeners").len(),1); assert_eq!(timers.count(),1); status.dispose(); assert_eq!(timers.count(),0); assert!(manager.listeners.lock().expect("listeners").is_empty()); timers.fire(); assert_eq!(footer(&ui),None); }
#[test] fn live_refresh_reuses_existing_timer_and_stops_when_terminal() { let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),1); status.schedule_sync(); assert_eq!(timers.count(),1); manager.records.lock().expect("records")[0].status=TaskStatus::Completed; status.schedule_sync(); timers.fire(); assert_eq!(timers.count(),0); assert_eq!(footer(&ui),None); assert!(manager.listeners.lock().expect("listeners").is_empty()); }
#[test] fn child_activity_refreshes_the_count_and_lost_child_shows_failed() {
    let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); { let mut records=manager.records.lock().expect("records"); records[0].task_id="st_first".into(); let mut second=records[0].clone(); second.task_id="st_second".into(); records.push(second); }
    status.schedule_sync(); let listeners=manager.listeners.lock().expect("listeners").clone(); timers.fire(); assert_eq!(footer(&ui).as_deref(),Some("2 subagents"));
    manager.records.lock().expect("records")[1].status=TaskStatus::Lost;
    listeners["st_first"](&senpi_task::shared::ManagedChildEvent { event_type:"tool_execution_start".into(),tool_name:Some("read".into()),args:Some(serde_json::json!({"path":"first.rs"})),..Default::default() });
    timers.fire(); assert_eq!(footer(&ui).as_deref(),Some("1 subagent 1 failed")); status.dispose(); assert_eq!(timers.count(),0); assert!(manager.listeners.lock().expect("listeners").is_empty());
}
