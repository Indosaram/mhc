use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_task::parent_notifier::{TaskParentNotifier,CompletionCoordinator};
use senpi_task::{completion::{ParentNotifier,ParentNotifierMessage},host::HostError};
#[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
impl ExtensionActions for Actions {
    fn send_message(&self,message:CustomMessage,options:SendMessageOptions)->Result<(),ExtensionFailure> { self.0.lock().expect("messages").push((message,options)); Ok(()) }
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { panic!("not user message") }
    fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { panic!("not entry") }
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
}
#[derive(Default)] struct Coordinator { calls:Mutex<Vec<String>>,messages:Mutex<Vec<ParentNotifierMessage>> }
impl CompletionCoordinator for Coordinator {
    fn enqueue(&self,key:&str,source:&str,message:&ParentNotifierMessage)->Result<(),HostError> { assert_eq!(key,"task-completion"); assert_eq!(source,"task-completion"); self.messages.lock().expect("messages").push(message.clone()); self.calls.lock().expect("calls").push("enqueue".into()); Ok(()) }
    fn schedule_flush(&self) { self.calls.lock().expect("calls").push("schedule".into()); }
    fn flush_soon(&self) { self.calls.lock().expect("calls").push("soon".into()); }
}
fn message()->ParentNotifierMessage { ParentNotifierMessage { custom_type:"senpi-task.completion",content:"done".into(),display:false,details:vec![],trigger_turn:Some(true) } }
#[test] fn direct_fallback_preserves_hidden_renderer_channel_and_steers() { let actions=Arc::new(Actions::default()); let notifier=TaskParentNotifier { actions:actions.clone(),coordinator:None,is_streaming:Arc::new(|| false) }; notifier.enqueue(&message()).expect("enqueue"); let sent=actions.0.lock().expect("messages"); assert_eq!(sent.len(),1); assert_eq!(sent[0].0.custom_type,"senpi-task.completion"); assert!(!sent[0].0.display); assert!(sent[0].1.trigger_turn); assert_eq!(sent[0].1.deliver_as,Some(DeliverAs::Steer)); }
#[test] fn streaming_parent_schedules_coordinator_without_direct_delivery() { let actions=Arc::new(Actions::default()); let coordinator=Arc::new(Coordinator::default()); let notifier=TaskParentNotifier { actions:actions.clone(),coordinator:Some(coordinator.clone()),is_streaming:Arc::new(|| true) }; notifier.enqueue(&message()).expect("enqueue"); assert!(actions.0.lock().expect("messages").is_empty()); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","schedule"]); assert_eq!(*coordinator.messages.lock().expect("messages"),[message()]); }
#[test] fn idle_parent_requests_soon_flush_after_enqueue() { let coordinator=Arc::new(Coordinator::default()); let notifier=TaskParentNotifier { actions:Arc::new(Actions::default()),coordinator:Some(coordinator.clone()),is_streaming:Arc::new(|| false) }; notifier.enqueue(&message()).expect("enqueue"); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","soon"]); }
