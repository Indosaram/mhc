mod support;
use maho_ext_api::*;
use maho_omo_mass_ulw::{MassUlwComponent,MASS_ULW_DISABLED_FLAG};
#[derive(Default)] struct Actions(std::sync::Mutex<Vec<CustomMessage>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{assert!(!o.trigger_turn);assert!(o.deliver_as.is_none());self.0.lock().expect("messages").push(m);Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
fn register()->ExtensionApi {let mut api=ExtensionApi::new(LoadedExtension::new("mass-ulw","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());MassUlwComponent{skills_root:"/skills/".into()}.register(&mut api);api}
async fn dispatch(api:&ExtensionApi,text:&str,source:InputSource)->EventResult {let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:text.into(),images:None,source,streaming_behavior:Some(StreamingBehavior::FollowUp)});api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch")}
#[tokio::test] async fn queued_pointer_atomic() {let api=register();let EventResult::Input(InputEventResult::Transform{text,..})=dispatch(&api,"mass ulw build",InputSource::Interactive).await else {panic!("transform");};assert_eq!(text,format!("mass ulw build\n{}",maho_omo_mass_ulw::mass_ulw_skill_pointer("/skills/")));}
#[tokio::test] async fn repeated_mentions_transform_each_time() {let api=register();for _ in 0..2 {assert!(matches!(dispatch(&api,"mass ulw",InputSource::Interactive).await,EventResult::Input(InputEventResult::Transform{..})));}}
#[tokio::test] async fn extension_source_suppressed() {assert!(matches!(dispatch(&register(),"mass ulw",InputSource::Extension).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn disabled_suppressed() {let api=register();api.set_flag(MASS_ULW_DISABLED_FLAG,FlagValue::Boolean(true));assert!(matches!(dispatch(&api,"mass ulw",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn skill_command_suppressed() {assert!(matches!(dispatch(&register(),"/skill:mass-ulw",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn expanded_skill_suppressed() {assert!(matches!(dispatch(&register(),"<skill name=\"mass-ulw\">mass ulw</skill>",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn ordinary_input_suppressed() {assert!(matches!(dispatch(&register(),"hello",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn idle_hidden_pointer_leaves_text_unchanged() {let api=register();let a=std::sync::Arc::new(Actions::default());api.runtime.bind(a.clone());let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"mass ulw build".into(),images:None,source:InputSource::Interactive,streaming_behavior:None});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::Input(InputEventResult::Continue)));let m=a.0.lock().expect("messages");assert_eq!(m.len(),1);assert!(!m[0].display);}
