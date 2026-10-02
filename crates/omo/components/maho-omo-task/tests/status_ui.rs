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
#[test] fn rpc_mode_does_not_render_widget() { let (status,timers,ui,_)=fixture(true,ExtensionMode::Rpc); status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn terminal_set_clears_widget() { let (status,timers,ui,manager)=fixture(false,ExtensionMode::Tui); manager.records.lock().expect("records")[0].status=TaskStatus::Completed; status.sync_now(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets")[0].0.is_none()); }
#[test] fn repeated_schedule_coalesces_to_one_render() { let (status,timers,ui,_)=fixture(false,ExtensionMode::Tui); for _ in 0..3 { status.schedule_sync(); } assert_eq!(timers.count(),1); assert!(ui.widgets.lock().expect("widgets").is_empty()); timers.fire(); assert_eq!(ui.widgets.lock().expect("widgets").len(),1); assert_eq!(timers.count(),0); }
#[test] fn subscribes_before_debounce_and_dispose_removes_listener_and_timer() { let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); status.schedule_sync(); assert_eq!(manager.listeners.lock().expect("listeners").len(),1); assert_eq!(timers.count(),1); status.dispose(); assert_eq!(timers.count(),0); assert!(manager.listeners.lock().expect("listeners").is_empty()); timers.fire(); assert!(ui.widgets.lock().expect("widgets").is_empty()); }
#[test] fn live_refresh_reuses_existing_timer_and_stops_when_terminal() { let (status,timers,ui,manager)=fixture(true,ExtensionMode::Tui); status.sync_now(); assert_eq!(timers.count(),1); status.schedule_sync(); assert_eq!(timers.count(),1); manager.records.lock().expect("records")[0].status=TaskStatus::Completed; status.schedule_sync(); timers.fire(); assert_eq!(timers.count(),0); assert!(ui.widgets.lock().expect("widgets").last().expect("widget").0.is_none()); assert!(manager.listeners.lock().expect("listeners").is_empty()); }
