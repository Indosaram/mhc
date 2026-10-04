#[path="native_account/support.rs"]
mod support;
use maho_ext_api::*;
use std::sync::Arc;
struct Registry;
impl ModelRegistry for Registry{
    fn get_all(&self)->Vec<Model>{vec![]}
    fn get_available(&self)->Vec<Model>{vec![]}
    fn find(&self,_:&str,_:&str)->Option<Model>{None}
    fn has_configured_auth(&self,_:&Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("resolved auth required")})}
    fn get_stored_credential_type(&self,_:&str)->Result<Option<maho_ai::auth::types::CredentialType>,ExtensionFailure>{Ok(Some(maho_ai::auth::types::CredentialType::ApiKey))}
    fn get_provider_auth<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<maho_ai::models::AuthResolution>>{Box::pin(async{Ok(Some(maho_ai::models::AuthResolution{auth:maho_ai::models::ProviderAuthResult{api_key:Some("fixture-secret".into()),..Default::default()},env:None}))})}
}
#[tokio::test]
async fn registered_native_image_handler_uses_effective_model_and_scrubs_history_bytes()->Result<(),Box<dyn std::error::Error>>{
    let _guard=support::GlobalNativeStateGuard::acquire().await;
    let root=tempfile::tempdir()?;let mut ctx=support::context(Arc::new(Registry),Arc::new(support::TestUi::default()));ctx.cwd=root.path().into();
    let mut api=ExtensionApi::new(LoadedExtension::new("native-image",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_openai_image_gen::OpenAiImageGen.register(&mut api);
    let model=serde_json::from_value(serde_json::json!({"id":"native","name":"Native","api":"openai-responses","provider":"openai","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100}))?;
    let request=ExtensionEvent::BeforeProviderRequest{model:Some(model),payload:serde_json::json!({"model":"native","tools":[{"name":"generate_image"},{"name":"other"}]}),headers:None};
    let outcome=async{
        let EventResult::ProviderPayload(payload)=(api.registered.handlers[&EventKind::BeforeProviderRequest][0])(&request,&ctx).await?else{panic!("payload")};
        let native=maho_ext_openai_image_gen::gate::is_open_ai_image_gen_enabled();
        assert_eq!(maho_ext_imagegen::state::is_native_bypass(),native);
        let tools=payload["tools"].as_array().ok_or("tools")?;
        assert_eq!(tools.iter().filter(|tool|tool["type"]=="image_generation").count(),usize::from(native));
        assert_eq!(tools.iter().filter(|tool|tool["name"]=="generate_image").count(),usize::from(!native));
        assert!(ctx.model.is_none(),"request model was not a frozen context model");
        let message:AssistantMessage=serde_json::from_value(serde_json::json!({"role":"assistant","api":"openai-responses","provider":"openai","model":"native","timestamp":0,"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"content":[{"type":"providerNative","subtype":"image_generation_call","raw":{"type":"image_generation_call","status":"completed","id":"registered-image","result":"AQID"}}]}))?;
        let end=ExtensionEvent::MessageEnd{message:maho_agent_message(message)};
        let EventResult::MessageEnd{message:Some(replaced)}=(api.registered.handlers[&EventKind::MessageEnd][0])(&end,&ctx).await?else{panic!("externalized result")};
        assert_eq!(std::fs::read(root.path().join("generated-images/registered-image.png"))?,[1,2,3]);
        assert!(!serde_json::to_string(&replaced)?.contains("AQID"));
        assert!(matches!((api.registered.handlers[&EventKind::MessageEnd][0])(&ExtensionEvent::MessageEnd{message:replaced},&ctx).await?,EventResult::None));
        Ok::<(),Box<dyn std::error::Error>>(())
    }.await;
    let shutdown=(api.registered.handlers[&EventKind::SessionShutdown][0])(&ExtensionEvent::SessionShutdown(SessionShutdownEvent{reason:SessionReason::Quit,target_session_file:None,signal:None}),&ctx).await;
    let bypass=maho_ext_imagegen::state::is_native_bypass();drop(api);drop(ctx);root.close()?;
    shutdown?;outcome?;assert!(!bypass);Ok(())
}
fn maho_agent_message(message:AssistantMessage)->AgentMessage{AgentMessage::Llm(Message::Assistant(Box::new(message)))}
