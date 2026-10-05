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
    fn enqueue(&self,key:&str,source:&str,message:&ParentNotifierMessage,_callbacks:senpi_task::completion::DeliveryCallbacks)->Result<(),HostError> { assert_eq!(key,"task-completion"); assert_eq!(source,"task-completion"); self.messages.lock().expect("messages").push(message.clone()); self.calls.lock().expect("calls").push("enqueue".into()); Ok(()) }
    fn schedule_flush(&self) { self.calls.lock().expect("calls").push("schedule".into()); }
    fn flush_soon(&self) { self.calls.lock().expect("calls").push("soon".into()); }
}
fn message()->ParentNotifierMessage { ParentNotifierMessage { custom_type:"senpi-task.completion",content:"done".into(),display:false,details:vec![],trigger_turn:Some(true) } }
#[test] fn direct_fallback_preserves_hidden_renderer_channel_and_steers() { let actions=Arc::new(Actions::default()); let notifier=TaskParentNotifier { actions:actions.clone(),coordinator:None,is_streaming:Arc::new(|| false) }; notifier.enqueue(&message()).expect("enqueue"); let sent=actions.0.lock().expect("messages"); assert_eq!(sent.len(),1); assert_eq!(sent[0].0.custom_type,"senpi-task.completion"); assert!(!sent[0].0.display); assert!(sent[0].1.trigger_turn); assert_eq!(sent[0].1.deliver_as,Some(DeliverAs::Steer)); }
#[test] fn streaming_parent_schedules_coordinator_without_direct_delivery() { let actions=Arc::new(Actions::default()); let coordinator=Arc::new(Coordinator::default()); let notifier=TaskParentNotifier { actions:actions.clone(),coordinator:Some(coordinator.clone()),is_streaming:Arc::new(|| true) }; notifier.enqueue(&message()).expect("enqueue"); assert!(actions.0.lock().expect("messages").is_empty()); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","schedule"]); assert_eq!(*coordinator.messages.lock().expect("messages"),[message()]); }
#[test] fn idle_parent_requests_soon_flush_after_enqueue() { let coordinator=Arc::new(Coordinator::default()); let notifier=TaskParentNotifier { actions:Arc::new(Actions::default()),coordinator:Some(coordinator.clone()),is_streaming:Arc::new(|| false) }; notifier.enqueue(&message()).expect("enqueue"); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","soon"]); }
#[test] fn structured_completion_ids_form_stable_coordinator_key_and_preserve_details() {
    #[derive(Default)] struct Capture(Mutex<Vec<(String,ParentNotifierMessage)>>);
    impl CompletionCoordinator for Capture {
        fn enqueue(&self,key:&str,source:&str,message:&ParentNotifierMessage,_callbacks:senpi_task::completion::DeliveryCallbacks)->Result<(),HostError> { assert_eq!(source,"task-completion"); self.0.lock().expect("captured").push((key.into(),message.clone())); Ok(()) }
        fn schedule_flush(&self) {} fn flush_soon(&self) {}
    }
    let actions=Arc::new(Actions::default()); let coordinator=Arc::new(Capture::default()); let notifier=TaskParentNotifier { actions:actions.clone(),coordinator:Some(coordinator.clone()),is_streaming:Arc::new(|| true) }; let mut message=message(); message.details=["st_first","st_second"].into_iter().map(|id| senpi_task::completion::CompletionDetails { task_id:id.into(),name:id.into(),status:senpi_task::state::TaskStatus::Completed,category:None,agent_type:None,model:"faux/faux".into(),requested_model:None,fallback_models:None,resolved_model:None,duration_ms:1000,tokens:None,run_stats:None,final_response:"finished".into(),final_response_file:None,continuation_hint:"continue".into() }).collect(); notifier.enqueue(&message).expect("enqueue"); let captured=coordinator.0.lock().expect("captured"); assert_eq!(captured.len(),1); assert_eq!(captured[0].0,"task-completion:st_first,st_second"); assert_eq!(captured[0].1,message); assert!(actions.0.lock().expect("messages").is_empty());
}
#[test] fn direct_and_coordinated_delivery_settle_the_callback_exactly_once() {
    use senpi_task::completion::{DeliveryCallbacks,DeliveryState};
    let actions=Arc::new(Actions::default());
    let direct=TaskParentNotifier { actions:actions.clone(),coordinator:None,is_streaming:Arc::new(|| false) };
    let callbacks=DeliveryCallbacks::new(|_| {});
    direct.enqueue_with_callbacks(&message(),callbacks.clone()).expect("direct enqueue"); assert_eq!(callbacks.state(),DeliveryState::Delivered);
    callbacks.failed(HostError { message:"late rejection".into() }); assert_eq!(callbacks.state(),DeliveryState::Delivered);
    #[derive(Default)] struct Settling(Mutex<Vec<String>>);
    impl CompletionCoordinator for Settling {
        fn enqueue(&self,_:&str,_:&str,_:&ParentNotifierMessage,callbacks:senpi_task::completion::DeliveryCallbacks)->Result<(),HostError> { self.0.lock().expect("calls").push("enqueue".into()); callbacks.delivered(); Ok(()) }
        fn schedule_flush(&self) {} fn flush_soon(&self) {}
    }
    let coordinator=Arc::new(Settling::default()); let forwarded=DeliveryCallbacks::new(|_| {});
    // A fresh collector: the direct delivery above already steered into `actions`, so only a
    // collector used solely by the coordinated notifier can prove it takes no direct action.
    let coordinated_actions=Arc::new(Actions::default());
    let notifier=TaskParentNotifier { actions:coordinated_actions.clone(),coordinator:Some(coordinator.clone()),is_streaming:Arc::new(|| true) };
    notifier.enqueue_with_callbacks(&message(),forwarded.clone()).expect("coordinated enqueue"); assert_eq!(forwarded.state(),DeliveryState::Delivered);
    assert_eq!(*coordinator.0.lock().expect("calls"),["enqueue".to_string()]); assert!(coordinated_actions.0.lock().expect("messages").is_empty(),"coordinated delivery must not steer directly");
}
