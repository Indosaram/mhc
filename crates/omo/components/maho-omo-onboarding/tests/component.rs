mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_onboarding::component::OnboardingComponent;
#[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
impl ExtensionActions for Actions {
    fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{self.0.lock().expect("messages").push((m,o));Ok(())}
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected user message")}
    fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
fn register(path:&std::path::Path)->(ExtensionApi,Arc<Actions>) {
    let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("onboarding","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    OnboardingComponent{state_dir:path.into(),skills_root:"/skills".into()}.register(&mut api);(api,actions)
}
async fn start(api:&ExtensionApi,reason:SessionReason) { let mut event=ExtensionEvent::SessionStart(SessionStartEvent{reason,initial_model_provenance:None,previous_session_file:None});api.registered.handlers[&EventKind::SessionStart][0](&mut event,&support::context()).await.expect("dispatch"); }
#[tokio::test] async fn startup_claims_and_injects()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let(api,a)=register(root.path());start(&api,SessionReason::Startup).await;assert!(maho_omo_onboarding::state::is_onboarding_complete(root.path()));let messages=a.0.lock().expect("messages");assert_eq!(messages.len(),1);assert!(!messages[0].0.display);assert!(messages[0].1.trigger_turn);assert_eq!(messages[0].1.deliver_as,Some(DeliverAs::FollowUp));Ok(())}
#[tokio::test] async fn new_does_not_inject()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let(api,a)=register(root.path());start(&api,SessionReason::New).await;assert!(a.0.lock().expect("messages").is_empty());Ok(())}
#[tokio::test] async fn resume_reload_fork_do_not_inject()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let(api,a)=register(root.path());for reason in [SessionReason::Resume,SessionReason::Reload,SessionReason::Fork] {start(&api,reason).await;}assert!(a.0.lock().expect("messages").is_empty());Ok(())}
#[tokio::test] async fn disabled_does_not_inject()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let(api,a)=register(root.path());api.set_flag("omo-senpi-onboarding-disabled",FlagValue::Boolean(true));start(&api,SessionReason::Startup).await;assert!(a.0.lock().expect("messages").is_empty());Ok(())}
#[tokio::test] async fn force_only_once()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;maho_omo_onboarding::state::claim_onboarding(root.path());let(api,a)=register(root.path());api.set_flag("onboard",FlagValue::Boolean(true));start(&api,SessionReason::Startup).await;start(&api,SessionReason::Startup).await;assert_eq!(a.0.lock().expect("messages").len(),1);Ok(())}
#[tokio::test] async fn claimed_does_not_inject()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;maho_omo_onboarding::state::claim_onboarding(root.path());let(api,a)=register(root.path());start(&api,SessionReason::Startup).await;assert!(a.0.lock().expect("messages").is_empty());Ok(())}
