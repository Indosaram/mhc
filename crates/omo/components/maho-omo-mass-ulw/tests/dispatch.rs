mod support;
use maho_ext_api::*;
use maho_omo_mass_ulw::{MassUlwComponent,MASS_ULW_DISABLED_FLAG};
fn register()->ExtensionApi {let mut api=ExtensionApi::new(LoadedExtension::new("mass-ulw","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());MassUlwComponent{skills_root:"/skills/".into()}.register(&mut api);api}
async fn dispatch(api:&ExtensionApi,text:&str,source:InputSource)->EventResult {let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:text.into(),images:None,source,streaming_behavior:Some(StreamingBehavior::FollowUp)});api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch")}
#[tokio::test] async fn queued_pointer_atomic() {let api=register();let EventResult::Input(InputEventResult::Transform{text,..})=dispatch(&api,"mass ulw build",InputSource::Interactive).await else {panic!("transform");};assert_eq!(text,format!("mass ulw build\n{}",maho_omo_mass_ulw::mass_ulw_skill_pointer("/skills/")));}
#[tokio::test] async fn repeated_mentions_transform_each_time() {let api=register();for _ in 0..2 {assert!(matches!(dispatch(&api,"mass ulw",InputSource::Interactive).await,EventResult::Input(InputEventResult::Transform{..})));}}
#[tokio::test] async fn extension_source_suppressed() {assert!(matches!(dispatch(&register(),"mass ulw",InputSource::Extension).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn disabled_suppressed() {let api=register();api.set_flag(MASS_ULW_DISABLED_FLAG,FlagValue::Boolean(true));assert!(matches!(dispatch(&api,"mass ulw",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn skill_command_suppressed() {assert!(matches!(dispatch(&register(),"/skill:mass-ulw",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn expanded_skill_suppressed() {assert!(matches!(dispatch(&register(),"<skill name=\"mass-ulw\">mass ulw</skill>",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
#[tokio::test] async fn ordinary_input_suppressed() {assert!(matches!(dispatch(&register(),"hello",InputSource::Interactive).await,EventResult::Input(InputEventResult::Continue)));}
