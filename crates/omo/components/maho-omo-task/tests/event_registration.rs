mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_task::{dag_runtime::{DagRuntimeLifecycle,wire_dag_lifecycle},event_bridge::wire_task_usage_guidance};
struct Runtime(Arc<Mutex<Vec<&'static str>>>);
impl DagRuntimeLifecycle for Runtime {
    fn attach(&self)->Result<(),ExtensionFailure> { self.0.lock().expect("events").push("attach"); Ok(()) }
    fn detach(&self) { self.0.lock().expect("events").push("detach"); }
    fn pause_for_shutdown(&self)->Result<(),ExtensionFailure> { self.0.lock().expect("events").push("pause"); Ok(()) }
    fn dispose(&self) { self.0.lock().expect("events").push("dispose"); }
}
#[tokio::test]
async fn dag_shutdown_pauses_before_task_teardown_then_disposes() {
    let mut api=support::api(); let events=Arc::new(Mutex::new(Vec::new())); let task=events.clone();
    wire_dag_lifecycle(&mut api,Arc::new(Runtime(events.clone())),move |api| { api.on(EventKind::SessionShutdown,Arc::new(move |_,_| { let task=task.clone(); Box::pin(async move { task.lock().expect("events").push("task"); Ok(EventResult::None) }) })); });
    let context=support::context(); let mut event=ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None });
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] { handler(&mut event,&context).await.expect("shutdown"); }
    assert_eq!(*events.lock().expect("events"),["pause","task","dispose"]);
}
#[tokio::test]
async fn usage_guidance_is_hidden_and_delivered_once_per_session() {
    let mut api=support::api(); wire_task_usage_guidance(&mut api,Arc::new(|| true)); let context=support::context();
    let mut event=ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent { prompt:"work".into(),images:None,system_prompt:String::new(),system_prompt_options:Default::default() });
    let handler=&api.registered.handlers[&EventKind::BeforeAgentStart][0];
    let EventResult::BeforeAgentStart(result)=handler(&mut event,&context).await.expect("event") else { panic!("guidance"); }; let message=result.message.expect("message"); assert_eq!(message.custom_type,"senpi-task.usage"); assert!(!message.display);
    assert!(matches!(handler(&mut event,&context).await.expect("repeat"),EventResult::None));
}
#[tokio::test]
async fn dag_start_attaches_after_task_recovery_and_switch_detaches_after_task_capture() {
    let mut api=support::api(); let events=Arc::new(Mutex::new(Vec::new())); let task=events.clone();
    wire_dag_lifecycle(&mut api,Arc::new(Runtime(events.clone())),move |api| {
        for kind in [EventKind::SessionStart,EventKind::SessionBeforeSwitch] { let task=task.clone(); api.on(kind,Arc::new(move |_,_| { let task=task.clone(); Box::pin(async move { task.lock().expect("events").push("task"); Ok(EventResult::None) }) })); }
    });
    let context=support::context(); let mut start=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None }); for handler in &api.registered.handlers[&EventKind::SessionStart] { handler(&mut start,&context).await.expect("start"); }
    let mut switch=ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::New,target_session_file:None }; for handler in &api.registered.handlers[&EventKind::SessionBeforeSwitch] { handler(&mut switch,&context).await.expect("switch"); }
    assert_eq!(*events.lock().expect("events"),["task","attach","task","detach"]);
}
#[tokio::test]
async fn disabled_usage_guidance_does_not_deliver() {
    let mut api=support::api(); wire_task_usage_guidance(&mut api,Arc::new(|| false)); let context=support::context(); let mut event=ExtensionEvent::AgentStart;
    assert!(matches!(api.registered.handlers[&EventKind::BeforeAgentStart][0](&mut event,&context).await.expect("event"),EventResult::None));
}
