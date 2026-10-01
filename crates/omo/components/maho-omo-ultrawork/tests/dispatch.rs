mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_ultrawork::{UltraworkComponent,SessionArming,ULTRAWORK_REMINDER};
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
