use crate::support;
use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use maho_ext_api::{ExtensionMode,WidgetContent};
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
// TS dag-status-row-format.test.ts NOW: the render clock the waiting/elapsed labels are measured against.
const NOW_MS:i64=1_787_572_800_000;
fn fixture(mode:ExtensionMode)->(Arc<DagStatusUi>,Arc<Timers>,Arc<support::Ui>,Arc<Runs>,tempfile::TempDir) {
    let root=tempfile::tempdir().expect("root"); let store=create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store"); let manager=create_dag_manager(DagManagerOptions { store:Arc::new(store),new_run_id:Some(Arc::new(|| "run-1".into())),now:Some(Arc::new(|| 1000)),materialize_skills:None,settings:None }); let deps=DagToolDeps { manager:manager.clone(),parent_session_id:Arc::new(|| "session".into()),root_session_id:Arc::new(|| "session".into()),wait:None,cancel:None,retry:None,send:None,amend:None,cancel_node:None,guide:None,pause_reason:None }; run_dag_tool(&deps,serde_json::from_value(json!({"action":"start","definition":{"key":"plan","name":"release","nodes":[{"id":"a","prompt":"work","category":"quick"}]}})).expect("input")).expect("start"); let runs=Arc::new(Runs(Mutex::new(Some(manager.snapshot(&"run-1".into(),"session").expect("snapshot"))))); let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); context.mode=mode; let mut runtime=TaskRuntimeContext::new(root.path().into()); runtime.capture_from(&context); let timers=Arc::new(Timers::default()); (DagStatusUi::new(runs.clone(),Arc::new(Mutex::new(runtime)),timers.clone()),timers,ui,runs,root)
}
#[test] fn live_run_publishes_footer_status_and_reuses_refresh_timer() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),1); status.on_activity("run-1","a","reading"); assert_eq!(timers.count(),1); timers.fire(1000); assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 0/1 wave 1/1")); assert!(ui.widgets.lock().expect("widgets").iter().all(|widget| widget.0.is_none())); status.dispose(); assert_eq!(timers.count(),0); }
#[test] fn terminal_run_clears_status_and_live_refresh() { let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui); status.sync_now(); runs.0.lock().expect("run").as_mut().expect("run").status=DagRunStatus::Completed; timers.fire(1000); assert_eq!(timers.count(),0); assert!(!ui.statuses.lock().expect("statuses").contains_key("omo-dag")); }
#[test] fn headless_mode_produces_no_status_or_refresh() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Rpc); status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.statuses.lock().expect("statuses").is_empty()); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn uncaptured_runtime_never_queries_dag_manager() {
    struct Unavailable;
    impl DagStatusUiManager for Unavailable { fn list(&self,_:&str)->Vec<DagRunSummary> { panic!("uncaptured runtime must not query runs") } fn snapshot(&self,_:&str,_:&str)->Option<DagRunSnapshot> { panic!("uncaptured runtime must not query snapshots") } }
    let timers=Arc::new(Timers::default()); let status=DagStatusUi::new(Arc::new(Unavailable),Arc::new(Mutex::new(TaskRuntimeContext::new("/tmp".into()))),timers.clone()); status.sync_now(); assert_eq!(timers.count(),0); status.dispose();
}
#[test] fn node_transitions_advance_wave_and_count_failures() {
    use senpi_task::dag::types::{DagNodeState,DagRoute,DagWave};
    let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui);
    { let mut run=runs.0.lock().expect("run"); let run=run.as_mut().expect("snapshot"); run.nodes[0].id="plan".into(); run.nodes[0].created_at=String::new(); let mut build=run.nodes[0].clone(); build.id="build".into(); build.route=DagRoute::Agent { agent:"coder".into(),model:None }; run.nodes.push(build); run.waves=vec![DagWave { index:0,node_ids:vec!["plan".into()] },DagWave { index:1,node_ids:vec!["build".into()] }]; }
    for (first,second,expected) in [
        (DagNodeState::Pending,DagNodeState::Pending,"dag 0/2 wave 1/2"),
        (DagNodeState::Running,DagNodeState::Blocked,"dag 0/2 wave 1/2"),
        (DagNodeState::Completed,DagNodeState::Running,"dag 1/2 wave 2/2"),
        (DagNodeState::Completed,DagNodeState::Failed,"dag 1/2 wave 2/2 1 failed"),
    ] {
        { let mut run=runs.0.lock().expect("run"); let run=run.as_mut().expect("snapshot"); run.nodes[0].state=first; run.nodes[1].state=second; }
        status.sync_now(); assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some(expected));
    }
    status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn paused_run_keeps_its_footer_status() {
    use senpi_task::dag::types::DagNodeState;
    let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui);
    { let mut run=runs.0.lock().expect("run"); let run=run.as_mut().expect("snapshot"); run.status=DagRunStatus::Paused; run.nodes[0].state=DagNodeState::Skipped; let mut second=run.nodes[0].clone(); second.id="b".into(); second.state=DagNodeState::Cancelled; run.nodes.push(second); }
    status.sync_now(); assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 0/2 wave 1/1")); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn footer_counts_come_from_current_node_states() {
    use senpi_task::dag::types::DagNodeState;
    let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui);
    { let mut run=runs.0.lock().expect("run"); let run=run.as_mut().expect("snapshot"); let original=run.nodes[0].clone(); run.nodes=[DagNodeState::Completed,DagNodeState::Running,DagNodeState::Failed,DagNodeState::Pending].into_iter().enumerate().map(|(index,state)| { let mut node=original.clone(); node.id=format!("n{index}"); node.state=state; node }).collect(); }
    status.sync_now(); assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 1/4 wave 1/1 1 failed")); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn node_activity_refreshes_status_without_leaking_into_it() {
    let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui);
    runs.0.lock().expect("run").as_mut().expect("run").nodes[0].state=senpi_task::dag::types::DagNodeState::Running;
    status.on_activity("run-1","a","read file"); status.on_activity("run-1","a","edit file"); timers.fire(250);
    assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 0/1 wave 1/1"));
    runs.0.lock().expect("run").as_mut().expect("run").nodes[0].state=senpi_task::dag::types::DagNodeState::Completed; status.sync_now();
    assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 1/1 wave 1/1"));
    status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn unknown_run_activity_never_appears_in_owned_status() {
    let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); status.on_activity("unknown","ghost","wandering"); timers.fire(250);
    assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 0/1 wave 1/1")); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn pending_sync_debounces_and_disposal_prevents_render() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); status.dispose(); timers.fire(250); assert_eq!(timers.count(),0); assert!(ui.statuses.lock().expect("statuses").is_empty()); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn pruned_run_clears_status_on_next_refresh() { let (status,timers,ui,runs,_root)=fixture(ExtensionMode::Tui); status.sync_now(); assert!(ui.statuses.lock().expect("statuses").contains_key("omo-dag")); *runs.0.lock().expect("run")=None; timers.fire(1000); assert_eq!(timers.count(),0); assert!(!ui.statuses.lock().expect("statuses").contains_key("omo-dag")); }
#[test] fn scheduled_burst_renders_once_before_live_refresh() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); timers.fire(250); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); assert_eq!(timers.count(),1); status.dispose(); }
#[test] fn disposal_cancels_active_refresh_without_further_paint() { let (status,timers,ui,_,_root)=fixture(ExtensionMode::Tui); status.sync_now(); status.dispose(); timers.fire(1000); assert_eq!(timers.count(),0); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); }
#[test] fn run_missing_between_list_and_snapshot_does_not_hide_surviving_run() {
    struct Missing(Arc<Runs>);
    impl DagStatusUiManager for Missing {
        fn list(&self,session:&str)->Vec<DagRunSummary> { let mut runs=self.0.list(session); let mut missing=runs[0].clone(); missing.run_id="gone".into(); runs.push(missing); runs }
        fn snapshot(&self,run:&str,session:&str)->Option<DagRunSnapshot> { self.0.snapshot(run,session) }
    }
    let (_original,timers,ui,runs,_root)=fixture(ExtensionMode::Tui); let mut context=support::context(); context.ui=ui.clone(); context.mode=ExtensionMode::Tui; let mut runtime=TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context); let status=DagStatusUi::new(Arc::new(Missing(runs)),Arc::new(Mutex::new(runtime)),timers.clone()); status.sync_now(); assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag 0/1 wave 1/1")); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn several_live_runs_prefix_the_run_count() {
    struct Twice(Arc<Runs>);
    impl DagStatusUiManager for Twice {
        fn list(&self,session:&str)->Vec<DagRunSummary> { let mut runs=self.0.list(session); let mut other=runs[0].clone(); other.run_id="run-2".into(); runs.push(other); runs }
        fn snapshot(&self,_:&str,session:&str)->Option<DagRunSnapshot> { self.0.snapshot("run-1",session) }
    }
    let (_original,timers,ui,runs,_root)=fixture(ExtensionMode::Tui); let mut context=support::context(); context.ui=ui.clone(); context.mode=ExtensionMode::Tui; let mut runtime=TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context); let status=DagStatusUi::new(Arc::new(Twice(runs)),Arc::new(Mutex::new(runtime)),timers.clone()); status.sync_now(); assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("2 runs \u{b7} dag 0/1 wave 1/1")); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn unprojectable_live_run_keeps_tracking_status_and_refresh() {
    struct Unreadable;
    impl DagStatusUiManager for Unreadable { fn list(&self,_:&str)->Vec<DagRunSummary> { vec![DagRunSummary {run_id:"lost".into(),run_key:"key".into(),name:"lost".into(),parent_session_id:"session".into(),status:DagRunStatus::Running,created_at:"now".into(),updated_at:"now".into(),counts:Default::default()}] } fn snapshot(&self,_:&str,_:&str)->Option<DagRunSnapshot> {None} }
    let root=tempfile::tempdir().expect("root");let ui=Arc::new(support::Ui::default());let mut context=support::context();context.ui=ui.clone();context.mode=ExtensionMode::Tui;let mut runtime=TaskRuntimeContext::new(root.path().into());runtime.capture_from(&context);let timers=Arc::new(Timers::default());let status=DagStatusUi::new(Arc::new(Unreadable),Arc::new(Mutex::new(runtime)),timers.clone());status.sync_now();assert_eq!(ui.statuses.lock().expect("statuses").get("omo-dag").map(String::as_str),Some("dag tracking 1 run"));assert_eq!(timers.count(),1);status.dispose();
}
#[test] fn node_rows_rank_live_work_and_include_waiting_attempt_and_distinct_icons() {
    use senpi_task::dag::types::DagNodeState;
    use maho_omo_task::dag_status_row_format::{run_rows_with_options,DagRunRowsOptions};
    let (_status,_timers,_ui,runs,_root)=fixture(ExtensionMode::Tui);let now="2026-08-24T12:00:00.000Z";
    let run={let mut locked=runs.0.lock().expect("run");let run=locked.as_mut().expect("run");let original=run.nodes[0].clone();let mut pending=original.clone();pending.id="wait".into();pending.created_at="2026-08-24T11:59:00.000Z".into();let mut retry=original.clone();retry.id="retry".into();retry.state=DagNodeState::Running;retry.attempt=3;retry.started_at=Some(now.into());run.nodes=vec![pending,retry];run.clone()};
    let rows=run_rows_with_options(&run,None,Some(&DagRunRowsOptions{max_width:Some(120),now:Some(NOW_MS),is_process_alive:None}));assert!(rows[1].contains("▶ retry"));assert!(rows[1].contains("x3"));assert!(rows[2].contains("◌ wait"));assert!(rows[2].contains("waiting 1m 0s"));
}
#[test] fn failed_run_list_is_contained_and_render_fault_warning_is_once_per_message() {
    struct Fault;
    impl DagStatusUiManager for Fault { fn list(&self,_:&str)->Vec<DagRunSummary>{panic!("list fault")} fn snapshot(&self,_:&str,_:&str)->Option<DagRunSnapshot>{None} }
    let root=tempfile::tempdir().expect("root");let ui=Arc::new(support::Ui::default());let mut context=support::context();context.ui=ui.clone();context.mode=ExtensionMode::Tui;let mut runtime=TaskRuntimeContext::new(root.path().into());runtime.capture_from(&context);let timers=Arc::new(Timers::default());let messages=Arc::new(Mutex::new(Vec::new()));let captured=messages.clone();let status=DagStatusUi::with_options(Arc::new(Fault),Arc::new(Mutex::new(runtime)),timers,Some(Arc::new(move |message,error|captured.lock().expect("logs").push((message.to_owned(),error.to_owned())))));status.sync_now();status.sync_now();assert_eq!(messages.lock().expect("logs").len(),1);assert_eq!(messages.lock().expect("logs")[0].0,"omo-dag status widget render failed; frame skipped");
}
#[test] fn render_fault_is_contained_and_warns_only_once() {
    struct BrokenUi(support::Ui);
    impl maho_ext_api::ExtensionUi for BrokenUi {
        fn select<'a>(&'a self,title:&'a str,options:&'a [String],args:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>> { maho_ext_api::ExtensionUi::select(&self.0,title,options,args) }
        fn confirm<'a>(&'a self,title:&'a str,body:&'a str,args:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,bool> { maho_ext_api::ExtensionUi::confirm(&self.0,title,body,args) }
        fn input<'a>(&'a self,title:&'a str,placeholder:Option<&'a str>,args:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>> { maho_ext_api::ExtensionUi::input(&self.0,title,placeholder,args) }
        fn notify(&self,message:&str,kind:maho_ext_api::NotificationType){maho_ext_api::ExtensionUi::notify(&self.0,message,kind)} fn set_status(&self,key:&str,value:Option<&str>){maho_ext_api::ExtensionUi::set_status(&self.0,key,value)} fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:maho_ext_api::ExtensionWidgetOptions){panic!("widget fault")} fn set_header(&self,value:Option<maho_ext_api::ComponentFactory>){maho_ext_api::ExtensionUi::set_header(&self.0,value)} fn set_footer(&self,value:Option<maho_ext_api::ComponentFactory>){maho_ext_api::ExtensionUi::set_footer(&self.0,value)} fn set_title(&self,value:&str){maho_ext_api::ExtensionUi::set_title(&self.0,value)} fn paste_to_editor(&self,value:&str){maho_ext_api::ExtensionUi::paste_to_editor(&self.0,value)} fn set_editor_text(&self,value:&str){maho_ext_api::ExtensionUi::set_editor_text(&self.0,value)} fn get_editor_text(&self)->String{maho_ext_api::ExtensionUi::get_editor_text(&self.0)} fn custom(&self,factory:maho_ext_api::ComponentFactory,options:maho_ext_api::CustomUiOptions)->maho_ext_api::ExtensionFuture<'_,maho_ext_api::JsonValue>{maho_ext_api::ExtensionUi::custom(&self.0,factory,options)} fn theme(&self)->maho_ext_api::Theme{maho_ext_api::ExtensionUi::theme(&self.0)}
    }
    let root=tempfile::tempdir().expect("root");let broken=Arc::new(BrokenUi(support::Ui::default()));let mut context=support::context();context.ui=broken;context.mode=ExtensionMode::Tui;let mut runtime=TaskRuntimeContext::new(root.path().into());runtime.capture_from(&context);let timers=Arc::new(Timers::default());let warnings=Arc::new(Mutex::new(Vec::new()));let sink=warnings.clone();let status=DagStatusUi::with_options(Arc::new(Runs(Mutex::new(None))),Arc::new(Mutex::new(runtime)),timers,Some(Arc::new(move|message,error|sink.lock().expect("warnings").push((message.to_owned(),error.to_owned())))));status.sync_now();status.sync_now();assert_eq!(warnings.lock().expect("warnings").len(),1);assert_eq!(warnings.lock().expect("warnings")[0].0,"omo-dag status widget render failed; frame skipped");
}
