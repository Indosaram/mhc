mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_ultrawork::{UltraworkComponent,SessionArming,ULTRAWORK_REMINDER};
#[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{self.0.lock().expect("messages").push((m,o));Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
fn register()->ExtensionApi {
    let mut api=ExtensionApi::new(LoadedExtension::new("ultrawork","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    UltraworkComponent{arming:Arc::new(Mutex::new(SessionArming::default()))}.register(&mut api);api
}
async fn input(api:&ExtensionApi,text:&str,queued:bool)->EventResult {
    let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:text.into(),images:None,source:InputSource::Interactive,streaming_behavior:queued.then_some(StreamingBehavior::Steer)});
    api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch")
}
#[tokio::test] async fn queued_first_directive_byte_equality() { let api=register();let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw build",true).await else {panic!("transform");};assert_eq!(text,format!("ulw build\n{}",maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE)); }
#[tokio::test] async fn queued_second_uses_reminder() { let api=register();input(&api,"ulw build",true).await;let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw again",true).await else {panic!("transform");};assert_eq!(text,format!("ulw again\n{ULTRAWORK_REMINDER}")); }
#[tokio::test] async fn pasted_block_arms_without_transform() { let api=register();assert!(matches!(input(&api,"<ultrawork-mode>x</ultrawork-mode>",true).await,EventResult::Input(InputEventResult::Continue)));let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert!(text.ends_with(ULTRAWORK_REMINDER)); }
#[tokio::test] async fn skill_expansion_arms() { let api=register();input(&api,"/skill:ultrawork",true).await;let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert!(text.ends_with(ULTRAWORK_REMINDER)); }
#[tokio::test] async fn ordinary_input_passes() { assert!(matches!(input(&register(),"hello",true).await,EventResult::Input(InputEventResult::Continue))); }
#[tokio::test] async fn idle_injects_hidden_without_rewriting_user() {let api=register();let a=Arc::new(Actions::default());api.runtime.bind(a.clone());assert!(matches!(input(&api,"ulw build",false).await,EventResult::Input(InputEventResult::Continue)));let m=a.0.lock().expect("messages");assert_eq!(m.len(),1);assert!(!m[0].0.display);assert!(!m[0].1.trigger_turn);assert!(m[0].1.deliver_as.is_none());}
#[tokio::test] async fn accepted_compact_reinjects_full_directive() {let api=register();input(&api,"ulw",true).await;let mut event=ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted{reason:CompactionReason::Manual,request_id:"id".into(),compaction_entry:SessionEntry{id:"entry".into(),parent_id:None,timestamp:String::new(),kind:"compaction".into(),data:JsonValue::Null},from_extension:false,will_retry:false});api.registered.handlers[&EventKind::SessionCompact][0](&mut event,&support::context()).await.expect("dispatch");let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert_eq!(text,format!("ulw\n{}",maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE));}
#[tokio::test] async fn rejected_compact_retains_reminder() {let api=register();input(&api,"ulw",true).await;let mut event=ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{reason:CompactionReason::Manual,request_id:"id".into(),rejection_cause:CompactionRejectionCause::CancelledByExtension});api.registered.handlers[&EventKind::SessionCompact][0](&mut event,&support::context()).await.expect("dispatch");let EventResult::Input(InputEventResult::Transform{text,..})=input(&api,"ulw",true).await else {panic!("transform");};assert_eq!(text,format!("ulw\n{ULTRAWORK_REMINDER}"));}
