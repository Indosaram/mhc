use crate::support;
use maho_ext_api::{ExtensionMode, ExtensionWidgetOptions, WidgetContent};
use maho_omo_task::{dag_status_ui::{DagStatusUi,DagStatusUiManager},runtime_context::TaskRuntimeContext,status_ui::StatusUiTimers};
use senpi_task::dag::{manager::DagRunSummary,types::DagRunSnapshot};
use std::sync::{Arc,Mutex};

type TimerEntry = (u64, Option<Box<dyn FnOnce()+Send>>);
struct Timers(Mutex<Vec<TimerEntry>>);
impl Default for Timers{fn default()->Self{Self(Mutex::new(Vec::new()))}}
impl StatusUiTimers for Timers{fn set(&self,callback:Box<dyn FnOnce()+Send>,_:u64)->u64{let mut timers=self.0.lock().expect("timers");let id=timers.len() as u64+1;timers.push((id,Some(callback)));id}fn clear(&self,id:u64){if let Some((_,callback))=self.0.lock().expect("timers").iter_mut().find(|(candidate,_)|*candidate==id){*callback=None;}}}
impl Timers{fn run(&self){let callbacks=self.0.lock().expect("timers").iter_mut().filter_map(|(_,callback)|callback.take()).collect::<Vec<_>>();for callback in callbacks{callback();}}}
struct Manager;
impl DagStatusUiManager for Manager{fn list(&self,_:&str)->Vec<DagRunSummary>{Vec::new()}fn snapshot(&self,_:&str,_:&str)->Option<DagRunSnapshot>{None}}
struct FaultUi;
impl maho_ext_api::ExtensionUi for FaultUi{
fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>>{Box::pin(async{None})}
fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,bool>{Box::pin(async{false})}
fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>>{Box::pin(async{None})}
fn notify(&self,_:&str,_:maho_ext_api::NotificationType){}fn set_status(&self,_:&str,_:Option<&str>){}fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){panic!("setWidget fault")}fn set_header(&self,_:Option<maho_ext_api::ComponentFactory>){}fn set_footer(&self,_:Option<maho_ext_api::ComponentFactory>){}fn set_title(&self,_:&str){}fn paste_to_editor(&self,_:&str){}fn set_editor_text(&self,_:&str){}fn get_editor_text(&self)->String{String::new()}fn custom(&self,_:maho_ext_api::ComponentFactory,_:maho_ext_api::CustomUiOptions)->maho_ext_api::ExtensionFuture<'_,maho_ext_api::JsonValue>{Box::pin(async{Err(maho_ext_api::ExtensionFailure::new("unused"))})}fn theme(&self)->maho_ext_api::Theme{maho_ext_api::Theme::default()}}
fn make_ui(timers:Arc<Timers>,warnings:Arc<Mutex<Vec<(String,String)>>>) -> Arc<DagStatusUi>{let mut context=support::context();context.ui=Arc::new(FaultUi);context.mode=ExtensionMode::Tui;let mut runtime=TaskRuntimeContext::new("/tmp".into());runtime.capture_from(&context);DagStatusUi::with_options(Arc::new(Manager),Arc::new(Mutex::new(runtime)),timers,None,None,Some(Arc::new(move|message,error|warnings.lock().expect("warnings").push((message.into(),error.into())))))}
#[test]fn given_a_ui_whose_set_widget_throws_when_sync_now_runs_then_the_fault_is_contained_and_logged_once(){let timers=Arc::new(Timers::default());let warnings=Arc::new(Mutex::new(Vec::new()));let ui=make_ui(timers,warnings.clone());ui.sync_now();ui.sync_now();let warnings=warnings.lock().expect("warnings");assert_eq!(warnings.len(),1);assert_eq!(warnings[0].0,"omo-dag status widget render failed; frame skipped");assert!(warnings[0].1.contains("setWidget fault"));}
#[test]fn given_a_debounced_render_whose_widget_write_throws_when_the_timer_fires_then_the_callback_does_not_throw(){let timers=Arc::new(Timers::default());let warnings=Arc::new(Mutex::new(Vec::new()));let ui=make_ui(timers.clone(),warnings.clone());ui.schedule_sync();timers.run();assert_eq!(warnings.lock().expect("warnings").len(),1);ui.dispose();}
