#[path="native_account/support.rs"]
mod support;
use maho_ext_api::*;
use maho_ext_reasoning::{Reasoning,ReasoningHost,ThinkingPreferences};
use maho_ai::types::{ModelThinkingLevel,ThinkingLevel};
use std::sync::{Arc,Mutex};
struct Host{level:Mutex<ModelThinkingLevel>,last_on:Mutex<Option<ModelThinkingLevel>>,operations:Mutex<Vec<&'static str>>}
impl ReasoningHost for Host{
    fn level(&self,_:&ExtensionContext)->Result<ModelThinkingLevel,ExtensionFailure>{Ok(*self.level.lock().expect("level"))}
    fn set_level(&self,_:&ExtensionContext,level:ModelThinkingLevel)->Result<(),ExtensionFailure>{self.operations.lock().expect("operations").push("set");*self.level.lock().expect("level")=level;Ok(())}
    fn preferences(&self,_:&ExtensionContext,_:&Model)->Result<ThinkingPreferences,ExtensionFailure>{Ok(ThinkingPreferences{last_on:*self.last_on.lock().expect("memory"),remembered:None,global:None})}
    fn remember_last_on<'a>(&'a self,_:&'a ExtensionContext,_:&'a Model,level:ModelThinkingLevel)->ExtensionFuture<'a,()>{Box::pin(async move{self.operations.lock().expect("operations").push("remember");*self.last_on.lock().expect("memory")=Some(level);Ok(())})}
    fn restore_on<'a>(&'a self,_:&'a ExtensionContext,_:&'a Model,level:ThinkingLevel)->ExtensionFuture<'a,()>{Box::pin(async move{self.operations.lock().expect("operations").push("restore");*self.level.lock().expect("level")=level.into();Ok(())})}
}
#[tokio::test]
async fn registered_reasoning_preserves_off_axis_and_reclassifies_live_model()->Result<(),Box<dyn std::error::Error>>{
    let root=tempfile::tempdir()?;
    let registry=Arc::new(support::SyntheticRegistry{storage:Arc::new(tokio::sync::Mutex::new(maho_core::auth_storage::AuthStorage::create(&root.path().join("auth.json").to_string_lossy()))),repository:Arc::new(maho_core::credential_pool::state_store::CredentialSlotRepository::new(&root.path().join("pool.json").to_string_lossy()))});
    let ui=Arc::new(support::TestUi::default());
    let mut ctx=support::context(registry,ui.clone());
    ctx.model=Some(serde_json::from_value(serde_json::json!({"id":"graded","name":"Graded","api":"openai-completions","provider":"fixture","baseUrl":"","reasoning":true,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":1000,"maxTokens":100}))?);
    let host=Arc::new(Host{level:Mutex::new(ModelThinkingLevel::High),last_on:Mutex::new(None),operations:Mutex::new(vec![])});
    let mut api=ExtensionApi::new(LoadedExtension::new("reasoning",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    Reasoning{host:host.clone()}.register(&mut api);
    let reasoning=api.registered.commands.iter().find(|command|command.name=="reasoning").expect("reasoning").handler.clone();
    let efforts=api.registered.commands.iter().find(|command|command.name=="efforts").expect("efforts").handler.clone();
    let outcome=async{
        reasoning("off",&ctx).await?;
        assert_eq!(host.level(&ctx)?,ModelThinkingLevel::Off);
        assert_eq!(*host.operations.lock().expect("operations"),["remember","set"]);
        reasoning("on",&ctx).await?;
        assert_eq!(host.level(&ctx)?,ModelThinkingLevel::High);
        assert_eq!(*host.last_on.lock().expect("last on"),Some(ModelThinkingLevel::High));
        let before=host.operations.lock().expect("operations").len();
        efforts("off",&ctx).await?;
        assert_eq!(host.operations.lock().expect("operations").len(),before);
        assert_eq!(ui.0.lock().expect("notifications").last().expect("unsupported").1,NotificationType::Error);
        efforts("low",&ctx).await?;assert_eq!(host.level(&ctx)?,ModelThinkingLevel::Low);
        let completions=(api.registered.command_argument_completions["efforts"])("h").await?.expect("graded completions");
        assert!(completions.iter().any(|item|item.value=="high"));
        ctx.model.as_mut().expect("model").reasoning=false;
        let before=host.operations.lock().expect("operations").len();
        reasoning("on",&ctx).await?;efforts("high",&ctx).await?;
        assert_eq!(host.operations.lock().expect("operations").len(),before);
        assert!((api.registered.command_argument_completions["efforts"])("").await?.is_none());
        Ok::<(),Box<dyn std::error::Error>>(())
    }.await;
    drop(api);drop(ctx);root.close()?;outcome
}
