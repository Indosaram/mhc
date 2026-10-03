#[path="native_account/support.rs"]
mod support;
use maho_ext_api::*;
use maho_ext_builtin_loose::service_tier::{ServiceTierExtension,ServiceTierHost,CODEX_RESPONSES_API};
use std::sync::{Arc,Mutex};
struct Host{memory:Mutex<Option<ServiceTier>>,reads:Mutex<Vec<String>>}
impl ServiceTierHost for Host{
    fn catalog_tier(&self,_:&Model)->Option<ServiceTier>{None}
    fn upstream_id(&self,model:&Model)->String{model.id.clone()}
    fn remembered(&self,_:&ExtensionContext,model:&Model)->Result<Option<ServiceTier>,ExtensionFailure>{self.reads.lock().expect("reads").push(model.id.clone());Ok(*self.memory.lock().expect("memory"))}
    fn global_tier(&self,_:&ExtensionContext)->Result<Option<ServiceTier>,ExtensionFailure>{Ok(None)}
    fn persist<'a>(&'a self,_:&'a ExtensionContext,_:&'a Model,_:ServiceTier)->ExtensionFuture<'a,()>{Box::pin(async{panic!("pin refusal must not persist")})}
    fn thinking_level(&self,_:&ExtensionContext)->Result<Option<maho_ai::types::ModelThinkingLevel>,ExtensionFailure>{panic!("model-select does not normalize startup thinking")}
    fn restore_thinking_level(&self,_:&ExtensionContext,_:maho_ai::types::ModelThinkingLevel)->Result<(),ExtensionFailure>{panic!("model-select does not normalize startup thinking")}
}
#[tokio::test]
async fn registered_fast_pin_refusal_and_model_memory_do_not_mutate_settings()->Result<(),Box<dyn std::error::Error>>{
    let root=tempfile::tempdir()?;
    let registry=Arc::new(support::SyntheticRegistry{storage:Arc::new(tokio::sync::Mutex::new(maho_core::auth_storage::AuthStorage::create(&root.path().join("auth.json").to_string_lossy()))),repository:Arc::new(maho_core::credential_pool::state_store::CredentialSlotRepository::new(&root.path().join("pool.json").to_string_lossy()))});
    let ui=Arc::new(support::TestUi::default());let mut ctx=support::context(registry,ui.clone());
    let model:Model=serde_json::from_value(serde_json::json!({"id":"codex","name":"Codex","api":CODEX_RESPONSES_API,"provider":"fixture","baseUrl":"https://example.invalid","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100}))?;
    ctx.model=Some(model.clone());ctx.service_tier=Some(ServiceTier::Priority);
    let host=Arc::new(Host{memory:Mutex::new(Some(ServiceTier::Auto)),reads:Mutex::new(vec![])});
    let mut api=ExtensionApi::new(LoadedExtension::new("tier",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    ServiceTierExtension{host:host.clone()}.register(&mut api);
    let command=api.registered.commands.iter().find(|command|command.name=="fast").expect("fast").handler.clone();
    let selected=ExtensionEvent::ModelSelect(ModelSelectEvent{model:model.clone(),previous_model:None,source:ModelSelectSource::Set,system_prompt:String::new(),system_prompt_options:Default::default()});
    let payload=ExtensionEvent::BeforeProviderRequest{payload:serde_json::json!({"model":"codex"}),model:Some(model),headers:None};
    let outcome=async{
        (api.registered.handlers[&EventKind::ModelSelect][0])(&selected,&ctx).await?;
        assert_eq!(*host.reads.lock().expect("reads"),["codex"]);
        command("off",&ctx).await?;
        assert_eq!(ui.0.lock().expect("notifications").last().expect("refusal").1,NotificationType::Info);
        let EventResult::ProviderPayload(value)=(api.registered.handlers[&EventKind::BeforeProviderRequest][0])(&payload,&ctx).await?else{panic!("payload")};
        assert_eq!(value["service_tier"],"priority");
        let before=host.reads.lock().expect("reads").len();command("invalid",&ctx).await?;
        assert_eq!(host.reads.lock().expect("reads").len(),before);
        assert_eq!(ui.0.lock().expect("notifications").last().expect("invalid").1,NotificationType::Error);
        ctx.model.as_mut().expect("model").api="anthropic-messages".into();ctx.service_tier=None;
        command("on",&ctx).await?;
        assert_eq!(ui.0.lock().expect("notifications").last().expect("unavailable").1,NotificationType::Warning);
        Ok::<(),Box<dyn std::error::Error>>(())
    }.await;
    drop(api);drop(ctx);root.close()?;outcome
}
