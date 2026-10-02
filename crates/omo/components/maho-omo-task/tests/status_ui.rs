pub mod support;
use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use maho_ext_api::{ExtensionMode,WidgetContent,WidgetPlacement};
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
#[test] fn static_widget_uses_below_editor_without_animation_timer() { let (status,timers,ui,_)=fixture(false,ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),0); let widgets=ui.widgets.lock().expect("widgets"); assert_eq!(widgets.len(),1); assert_eq!(widgets[0].1,WidgetPlacement::BelowEditor); assert!(matches!(&widgets[0].0,Some(WidgetContent::Lines(rows)) if rows.len()==1)); }
#[test] fn multiple_rows_are_scoped_and_sanitize_controls_without_losing_wide_text() {
    let (status,timers,ui,manager)=fixture(false,ExtensionMode::Tui);
    { let mut records=manager.records.lock().expect("records"); records[0].name=Some("한국어\u{7} 작업".into()); records[0].category=Some("ultra\u{1b}[2Jbrain".into()); let mut second=records[0].clone(); second.task_id="st_second".into(); records.push(second); let mut foreign=records[0].clone(); foreign.parent_session_id="foreign".into(); records.push(foreign); }
    status.sync_now(); let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets[0].0 else { panic!("rows"); }; assert_eq!(rows.len(),2); assert!(rows[0].contains("한")); assert!(rows[0].contains("category:ultrabrain")); assert!(!rows[0].chars().any(|c| c.is_control())); assert_eq!(timers.count(),0);
}
#[test] fn rpc_mode_does_not_render_widget() { let (status,timers,ui,_)=fixture(true,ExtensionMode::Rpc); status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn live_solo_and_process_member_render_cost_before_tps_without_cache_rate() {
    struct Spend;
    impl StatusUiManager for Spend {
        fn list(&self,_:&str)->Vec<TaskRecord> { ["st_solo","st_member"].into_iter().map(|id| { let mut record=create_task_record(TaskRecordInput { parent_session_id:"session".into(),..Default::default() },Some(1)).expect("record"); record.task_id=id.into(); record.name=Some(if id=="st_solo" { "Solo" } else { "Member" }.into()); if id=="st_member" { record.execution_mode="process".into(); record.pid=Some(4242); } record }).collect() }
        fn has_background_surface(&self)->bool { true } fn was_background(&self,_:&str)->bool { true }
        fn run_stats_snapshot(&self,id:&str)->Option<senpi_task::state::TaskRunStats> { Some(senpi_task::state::TaskRunStats { runtime_ms:1000,turns:1,tool_calls:0,cost_usd:Some(if id=="st_solo" { 0.4213 } else { 0.017 }),tokens_per_second:Some(if id=="st_solo" { 40.0 } else { 12.0 }),cache_hit_rate_last:Some(0.5),cache_hit_rate_run:Some(0.1),..Default::default() }) }
    }
    let ui=Arc::new(support::Ui::default()); let mut context=support::context(); context.ui=ui.clone(); context.mode=ExtensionMode::Tui; let mut runtime=TaskRuntimeContext::new("/tmp".into()); runtime.capture_from(&context); let timers=Arc::new(Timers::default()); let status=TaskStatusUi::new(Arc::new(Spend),Arc::new(Mutex::new(runtime)),timers.clone(),Arc::new(|| 1000),Arc::new(|| None)); status.sync_now(); let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets.last().expect("widget").0 else { panic!("rows") }; assert!(rows[0].contains("$0.4213 · 40 tok/s")); assert!(rows[1].contains("$0.0170 · 12 tok/s")); assert!(!rows.join("\n").contains("CH:")); drop(widgets); status.dispose(); assert_eq!(timers.count(),0);
}
#[test] fn uncaptured_runtime_never_queries_manager() {
    struct Unavailable;
    impl StatusUiManager for Unavailable { fn list(&self,_:&str)->Vec<TaskRecord> { panic!("uncaptured runtime must not query tasks") } }
    let timers=Arc::new(Timers::default()); let status=TaskStatusUi::new(Arc::new(Unavailable),Arc::new(Mutex::new(TaskRuntimeContext::new("/tmp".into()))),timers.clone(),Arc::new(|| 1000),Arc::new(|| Some(80)));
    status.sync_now(); assert_eq!(timers.count(),0); status.dispose();
}
#[test] fn production_widget_uses_terminal_width_and_keeps_activity_and_elapsed() {
    let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); { let mut records=manager.records.lock().expect("records"); records[0].task_summary=Some("Plan the complete Spider-Man media library migration".into()); records[0].category=Some("unspecified-high".into()); records[0].model="anthropic/claude-opus-5".into(); records[0].created_at="1970-01-01T00:00:00.000Z".into(); }
    let status=TaskStatusUi::new(manager,status.runtime.clone(),timers.clone(),Arc::new(|| 60000),Arc::new(|| Some(120))); status.sync_now(); let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets.last().expect("widget").0 else { panic!("rows") }; assert_eq!(rows.len(),1); assert!(senpi_task::renderer_text::renderer_visible_width(&rows[0])<=120); assert!(rows[0].contains("Plan the")); assert!(rows[0].contains("running")); assert!(rows[0].ends_with("1m 0s")); drop(widgets); status.dispose(); assert_eq!(timers.count(),0); assert!(status.manager.list("session").len()==1);
}
#[test] fn terminal_set_clears_widget() { let (status,timers,ui,manager)=fixture(false,ExtensionMode::Tui); manager.records.lock().expect("records")[0].status=TaskStatus::Completed; status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets")[0].0.is_none()); }
#[test] fn repeated_schedule_coalesces_to_one_render() { let (status,timers,ui,_)=fixture(false,ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); assert!(ui.widgets.lock().expect("widgets").is_empty()); timers.fire(); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); assert_eq!(timers.count(),0); }
#[test] fn subscribes_before_debounce_and_dispose_removes_listener_and_timer() { let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); status.schedule_sync(); assert_eq!(manager.listeners.lock().expect("listeners").len(),1); assert_eq!(timers.count(),1); status.dispose(); assert_eq!(timers.count(),0); assert!(manager.listeners.lock().expect("listeners").is_empty()); timers.fire(); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn live_refresh_reuses_existing_timer_and_stops_when_terminal() { let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),1); status.schedule_sync(); assert_eq!(timers.count(),1); manager.records.lock().expect("records")[0].status=TaskStatus::Completed; status.schedule_sync(); timers.fire(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").last().expect("widget").0.is_none()); assert!(manager.listeners.lock().expect("listeners").is_empty()); }
#[test] fn two_live_children_render_latest_activity_independently() {
    let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); { let mut records=manager.records.lock().expect("records"); records[0].task_id="st_first".into(); records[0].task_summary=Some("First child".into()); let mut second=records[0].clone(); second.task_id="st_second".into(); second.task_summary=Some("Second child".into()); records.push(second); }
    status.schedule_sync(); let listeners=manager.listeners.lock().expect("listeners").clone();
    for (id,tool,path) in [("st_first","read","old.rs"),("st_second","write","second.rs"),("st_first","read","first.rs")] { listeners[id](&senpi_task::shared::ManagedChildEvent { event_type:"tool_execution_start".into(),tool_name:Some(tool.into()),args:Some(serde_json::json!({"path":path})),..Default::default() }); }
    timers.fire(); let widgets=ui.widgets.lock().expect("widgets"); let Some(WidgetContent::Lines(rows))=&widgets.last().expect("widget").0 else { panic!("rows") }; assert_eq!(rows.len(),2); assert!(rows[0].contains("first.rs")); assert!(!rows[0].contains("old.rs")); assert!(rows[1].contains("second.rs")); drop(widgets); status.dispose(); assert_eq!(timers.count(),0); assert!(manager.listeners.lock().expect("listeners").is_empty());
}
