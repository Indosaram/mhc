pub mod support;
use std::{collections::BTreeMap,sync::{Arc,Mutex,Condvar},time::Duration};
use maho_omo_task::task_rpc_bridge::wire_task_rpc_bridge;
use senpi_task::{host::HostError,manager::{ManagedChildHandle,ManagedChildListener,Unsubscribe,types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,TaskManagerOptions,ManagerStartSpec,ResolvedChildPlan,StartResult},create_task_manager},runners::RunnerOutcome,store::{StateDirConfig,TaskRecordStore},shared::ManagedChildEvent};
use serde_json::{Value,json};
struct Child { id:String,next:Mutex<u64>,listeners:Arc<Mutex<BTreeMap<u64,ManagedChildListener>>>,outcome:Mutex<Option<RunnerOutcome>>,settled:Condvar,steered:Mutex<Vec<String>> }
impl Child {
    fn finish(&self) { *self.outcome.lock().expect("outcome")=Some(RunnerOutcome::Cancelled); self.settled.notify_all(); }
    fn emit(&self,event:&ManagedChildEvent) { let listeners=self.listeners.lock().expect("listeners").values().cloned().collect::<Vec<_>>(); for listener in listeners { listener(event); } }
}
impl ManagedChildHandle for Child {
    fn task_id(&self)->&str { &self.id } fn session_id(&self)->Option<String> { Some("child-session".into()) } fn pid(&self)->Option<i64> { None }
    fn steer(&self,text:&str)->Result<(),HostError> { self.steered.lock().expect("steered").push(text.into()); Ok(()) }
    fn follow_up(&self,_:&str)->Result<(),HostError> { panic!("not follow-up") }
    fn abort(&self)->Result<(),HostError> { self.finish(); Ok(()) }
    fn subscribe(&self,listener:ManagedChildListener)->Unsubscribe { let mut next=self.next.lock().expect("next"); *next+=1; let id=*next; self.listeners.lock().expect("listeners").insert(id,listener); let listeners=self.listeners.clone(); Box::new(move || { listeners.lock().expect("listeners").remove(&id); }) }
    fn wait_for_outcome(&self)->RunnerOutcome { let (mut outcome,timeout)=self.settled.wait_timeout_while(self.outcome.lock().expect("outcome"),Duration::from_secs(10),|outcome| outcome.is_none()).expect("outcome signal"); assert!(!timeout.timed_out(),"child fixture not settled"); outcome.take().expect("settlement") }
    fn last_assistant_text(&self)->Option<String> { None } fn dispose(&self)->Result<(),HostError> { self.listeners.lock().expect("listeners").clear(); Ok(()) }
}
struct Runner(Arc<Mutex<Option<Arc<Child>>>>);
impl ManagedRunner for Runner { fn start(&self,spec:&ManagedStartSpec)->ManagedRunnerResult { let child=Arc::new(Child { id:spec.task_id.clone(),next:Mutex::new(0),listeners:Arc::new(Mutex::new(BTreeMap::new())),outcome:Mutex::new(None),settled:Condvar::new(),steered:Mutex::new(vec![]) }); *self.0.lock().expect("child")=Some(child.clone()); Ok(child) } }
#[tokio::test] async fn registered_controls_and_live_progress_release_bridge_subscriptions_on_cancel() {
    let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None }); let slot=Arc::new(Mutex::new(None)); let runner=Arc::new(Runner(slot.clone())); let manager=Arc::new(create_task_manager(TaskManagerOptions::new(store.clone(),ManagedRunners { in_process:runner.clone(),process:runner },Arc::new(|_| Ok(ResolvedChildPlan { model:"faux/faux".into(),..Default::default() })),root.path().to_string_lossy())));
    let StartResult::Started(task)=manager.start(&ManagerStartSpec { prompt:"work".into(),parent_session_id:"parent".into(),root_session_id:Some("parent".into()),run_in_background:true,..Default::default() }) else { panic!("start"); }; let child=slot.lock().expect("child").clone().expect("started child"); let baseline=child.listeners.lock().expect("listeners").len(); let mut api=support::api(); let events=Arc::new(Mutex::new(Vec::<Value>::new())); let sink=events.clone(); let bridge=wire_task_rpc_bridge(&mut api,manager.clone(),Arc::new(|| Some("parent".into())),store.state_dir().to_string_lossy().into_owned(),Arc::new(move |_,value| sink.lock().expect("events").push(value))).expect("bridge"); bridge.attach(); assert_eq!(child.listeners.lock().expect("listeners").len(),baseline+1);
    maho_omo_task::reload_guard::wire_reload_guard(&mut api,manager.clone()); let context=support::context(); let mut reload=maho_ext_api::ExtensionEvent::SessionBeforeReload; let handler=&api.registered.handlers[&maho_ext_api::EventKind::SessionBeforeReload][0];
    for _ in 0..2 { let maho_ext_api::EventResult::SessionBefore(veto)=handler(&mut reload,&context).await.expect("reload") else { panic!("running resident veto"); }; assert_eq!(veto.cancel,Some(true)); }
    let sent=(api.registered.rpc_handlers["omo.task.send"])(json!({"to":task.task_id,"message":"read file"})).await.expect("send"); assert_eq!(sent["kind"],"steered"); assert_eq!(*child.steered.lock().expect("steered"),["read file"]);
    child.emit(&ManagedChildEvent { event_type:"tool_execution_start".into(),tool_name:Some("read".into()),tool_call_id:Some("call-1".into()),args:Some(json!({"path":"src/lib.rs"})),..Default::default() }); { let events_guard=events.lock().expect("events"); let live=&events_guard.last().expect("snapshot")["tasks"][0]; assert!(live.get("live_progress").is_some()); assert!(live["live_progress"].get("current_tool").is_some()); }
    (api.registered.rpc_handlers["omo.task.cancel"])(json!({"task_id":task.task_id,"reason":"finished fixture"})).await.expect("cancel"); bridge.sync(); let count=events.lock().expect("events").len(); child.emit(&ManagedChildEvent { event_type:"tool_execution_start".into(),tool_name:Some("write".into()),..Default::default() }); assert_eq!(events.lock().expect("events").len(),count); bridge.dispose(); manager.forget(&task.task_id); assert!(child.listeners.lock().expect("listeners").is_empty());
}
