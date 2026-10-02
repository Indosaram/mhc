use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};
use maho_ext_api::*;
use maho_omo_task::lead_poller_lifecycle::{LeadMessageSink,LeadInjectionCoordinator};
use senpi_task::{completion::ParentState,team::messaging::lead_poller_types::{LeadInjection,LeadInjectionSink}};
#[derive(Default)] struct Actions { sent:Mutex<Vec<(CustomMessage,SendMessageOptions)>>,reject:bool }
impl ExtensionActions for Actions {
    fn send_message(&self,message:CustomMessage,options:SendMessageOptions)->Result<(),ExtensionFailure> { if self.reject { return Err(ExtensionFailure::new("fixture rejection")); } self.sent.lock().expect("sent").push((message,options)); Ok(()) }
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { panic!("not user message") } fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { panic!("not entry") } fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
}
#[derive(Default)] struct Coordinator { keys:Mutex<Vec<String>>,scheduled:AtomicUsize,soon:AtomicUsize }
impl LeadInjectionCoordinator for Coordinator {
    fn enqueue(&self,injection:LeadInjection,custom_type:&str,display:bool) { assert_eq!(custom_type,"senpi-task:team-message"); assert!(!display); self.keys.lock().expect("keys").push(injection.key); }
    fn schedule_flush(&self) { self.scheduled.fetch_add(1,Ordering::SeqCst); } fn flush_soon(&self) { self.soon.fetch_add(1,Ordering::SeqCst); }
}
fn injection(flushed:Arc<AtomicUsize>)->LeadInjection { LeadInjection { key:"team-message:m1".into(),source:"team-message",content:"ready".into(),on_flushed:Some(Box::new(move || { flushed.fetch_add(1,Ordering::SeqCst); })) } }
#[test] fn direct_team_message_is_hidden_steer_and_acknowledges_only_success() {
    for reject in [false,true] { let actions=Arc::new(Actions { reject,..Default::default() }); let errors=Arc::new(AtomicUsize::new(0)); let captured=errors.clone(); let sink=LeadMessageSink { actions:actions.clone(),coordinator:None,parent_state:Arc::new(|| ParentState::Idle),on_error:Arc::new(move |_| { captured.fetch_add(1,Ordering::SeqCst); }) }; let flushed=Arc::new(AtomicUsize::new(0)); sink.enqueue(injection(flushed.clone())); assert_eq!(flushed.load(Ordering::SeqCst),usize::from(!reject)); assert_eq!(errors.load(Ordering::SeqCst),usize::from(reject)); let sent=actions.sent.lock().expect("sent"); assert_eq!(sent.len(),usize::from(!reject)); if let Some((message,options))=sent.first() { assert_eq!(message.custom_type,"senpi-task:team-message"); assert!(!message.display); assert!(options.trigger_turn); assert_eq!(options.deliver_as,Some(DeliverAs::Steer)); } }
}
#[test] fn coordinator_enqueues_all_states_but_only_schedules_idle_and_streaming() {
    let actions=Arc::new(Actions::default()); let coordinator=Arc::new(Coordinator::default()); let flushed=Arc::new(AtomicUsize::new(0)); for state in [ParentState::Idle,ParentState::Streaming,ParentState::Compacting,ParentState::SessionSwitching,ParentState::SessionShutdown] { let sink=LeadMessageSink { actions:actions.clone(),coordinator:Some(coordinator.clone()),parent_state:Arc::new(move || state),on_error:Arc::new(|_| panic!("no direct send")) }; sink.enqueue(injection(flushed.clone())); }
    assert_eq!(coordinator.keys.lock().expect("keys").len(),5); assert_eq!(coordinator.scheduled.load(Ordering::SeqCst),1); assert_eq!(coordinator.soon.load(Ordering::SeqCst),1); assert_eq!(flushed.load(Ordering::SeqCst),0); assert!(actions.sent.lock().expect("sent").is_empty());
}
