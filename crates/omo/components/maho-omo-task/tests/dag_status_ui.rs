pub mod support;
use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use maho_ext_api::{ExtensionMode,WidgetContent,WidgetPlacement};
use maho_omo_task::{runtime_context::TaskRuntimeContext,status_ui::StatusUiTimers,dag_status_ui::{DagStatusUi,DagStatusUiManager},dag_tool::{DagToolDeps,run_dag_tool}};
use senpi_task::dag::{manager::{DagRunSummary,DagManagerOptions,create_dag_manager},store::{DagStoreConfig,DagStoreOptions,create_dag_file_store},types::{DagRunSnapshot,DagRunStatus}};
use serde_json::json;
type Callback=Box<dyn FnOnce()+Send>;
#[derive(Default)] struct Timers { next:Mutex<u64>,pending:Mutex<BTreeMap<u64,(u64,Callback)>> }
impl StatusUiTimers for Timers {
    fn set(&self,callback:Callback,ms:u64)->u64 { let mut next=self.next.lock().expect("next"); *next+=1; self.pending.lock().expect("pending").insert(*next,(ms,callback)); *next }
    fn clear(&self,id:u64) { self.pending.lock().expect("pending").remove(&id); }
}
impl Timers { fn fire(&self,ms:u64) { let ids=self.pending.lock().expect("pending").iter().filter(|(_,entry)| entry.0==ms).map(|(id,_)| *id).collect::<Vec<_>>(); for id in ids { let (_,callback)=self.pending.lock().expect("pending").remove(&id).expect("callback"); callback(); } } fn count(&self)->usize { self.pending.lock().expect("pending").len() } }
struct Runs(Mutex<Option<DagRunSnapshot>>);
impl DagStatusUiManager for Runs {
    fn list(&self,session:&str)->Vec<DagRunSummary> { self.0.lock().expect("run").iter().filter(|run| run.parent_session_id==session).map(|run| DagRunSummary { run_id:run.run_id.clone(),run_key:run.run_key.clone(),name:run.name.clone(),parent_session_id:run.parent_session_id.clone(),status:run.status,created_at:run.created_at.clone(),updated_at:run.created_at.clone(),counts:run.counts }).collect() }
    fn snapshot(&self,run:&str,session:&str)->Option<DagRunSnapshot> { self.0.lock().expect("run").as_ref().filter(|record| record.run_id==run && record.parent_session_id==session).cloned() }
}
fn fixture(mode:ExtensionMode)->(Arc<DagStatusUi>,Arc<Timers>,Arc<support::Ui>,Arc<Runs>,tempfile::TempDir) {
    let root=tempfile::tempdir().expect("root"); let store=create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store"); let manager=create_dag_manager(DagManagerOptions { store:Arc::new(store),new_run_id:Some(Arc::new(|| "run-1".into())),now:Some(Arc::new(|| 1000)),materialize_skills:None,settings:None }); let deps=DagToolDeps { manager:manager.clone(),parent_session_id:Arc::new(|| "session".into()),root_session_id:Arc::new(|| "session".into()),wait:None,cancel:None }; run_dag_tool(&deps,serde_json::from_value(json!({"action":"start","definition":{"key":"plan","name":"release","nodes":[{"id":"a","prompt":"work","category":"quick"}]}})).expect("input")).expect("start"); let runs=Arc::new(Runs(Mutex::new(Some(manager.snapshot(&"run-1".into(),"session").expect("snapshot"))))); let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); context.mode=mode; let mut runtime=TaskRuntimeContext::new(root.path().into()); runtime.capture_from(&context); let timers=Arc::new(Timers::default()); (DagStatusUi::new(runs.clone(),Arc::new(Mutex::new(runtime)),timers.clone()),timers,ui,runs,root)
}
#[test] fn live_run_renders_below_editor_and_reuses_refresh_timer() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),1); status.on_activity("run-1","a","reading"); assert_eq!(timers.count(),1); timers.fire(1000); let widgets=ui.widgets.lock().expect("widgets"); assert_eq!(widgets.len(),2); assert_eq!(widgets[0].1,WidgetPlacement::BelowEditor); assert!(matches!(&widgets[0].0,Some(WidgetContent::Lines(rows)) if rows.len()==2)); status.dispose(); assert_eq!(timers.count(),0); }
#[test] fn terminal_run_clears_widget_and_live_refresh() { let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui); status.sync_now(); runs.0.lock().expect("run").as_mut().expect("run").status=DagRunStatus::Completed; timers.fire(1000); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").last().expect("widget").0.is_none()); }
#[test] fn headless_mode_produces_no_widget_or_refresh() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Rpc); status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn latest_node_activity_replaces_prior_and_terminal_node_hides_it() {
    let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui);
    runs.0.lock().expect("run").as_mut().expect("run").nodes[0].state=senpi_task::dag::types::DagNodeState::Running;
    status.on_activity("run-1","a","read file"); status.on_activity("run-1","a","edit file"); timers.fire(250);
    { let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets.last().expect("widget").0 else { panic!("rows"); }; assert!(rows[1].contains("edit file")); assert!(!rows[1].contains("read file")); }
    runs.0.lock().expect("run").as_mut().expect("run").nodes[0].state=senpi_task::dag::types::DagNodeState::Completed; status.sync_now();
    { let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets.last().expect("widget").0 else { panic!("rows"); }; assert!(!rows[1].contains("edit file")); }
    status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn unknown_run_activity_never_appears_in_owned_widget() {
    let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); status.on_activity("unknown","ghost","wandering"); timers.fire(250);
    let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets.last().expect("widget").0 else { panic!("rows"); }; assert_eq!(rows.len(),2); assert!(!rows.join("\n").contains("wandering")); drop(widgets); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn pending_sync_debounces_and_disposal_prevents_render() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); status.dispose(); timers.fire(250); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn pruned_run_clears_widget_on_next_refresh() { let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui); status.sync_now(); *runs.0.lock().expect("run")=None; timers.fire(1000); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").last().expect("widget").0.is_none()); }
#[test] fn scheduled_burst_renders_once_before_live_refresh() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); timers.fire(250); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); assert_eq!(timers.count(),1); status.dispose(); }
#[test] fn disposal_cancels_active_refresh_without_further_paint() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); status.sync_now(); status.dispose(); timers.fire(1000); assert_eq!(timers.count(),0); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); }
