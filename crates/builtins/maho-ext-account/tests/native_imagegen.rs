#[path="native_account/support.rs"]
mod support;
use maho_ext_api::*;
use maho_ai::types::{ProviderImages,ImagesModel,ImagesContext,ImagesOptions,AssistantImages,ImagesStopReason,ImagesBackground,BoxFuture,ContentBlock,ImageContent,TextContent};
use std::sync::{Arc,Mutex};
struct Registry;
impl ModelRegistry for Registry{
    fn get_all(&self)->Vec<Model>{vec![]}
    fn get_available(&self)->Vec<Model>{vec![]}
    fn find(&self,_:&str,_:&str)->Option<Model>{None}
    fn has_configured_auth(&self,_:&Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("client must use resolved provider auth")})}
    fn get_stored_credential_type(&self,_:&str)->Result<Option<maho_ai::auth::types::CredentialType>,ExtensionFailure>{Ok(Some(maho_ai::auth::types::CredentialType::ApiKey))}
    fn get_provider_auth<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<maho_ai::models::AuthResolution>>{
        Box::pin(async{Ok(Some(maho_ai::models::AuthResolution{auth:maho_ai::models::ProviderAuthResult{api_key:Some("fixture-image-secret".into()),headers:Some(std::collections::BTreeMap::from([("X-Fixture".into(),Some("image".into()))])),base_url:None},env:None}))})
    }
}
struct Provider{calls:Mutex<Vec<(ImagesModel,ImagesContext,ImagesOptions)>>}
impl ProviderImages for Provider{
    fn generate_images<'a>(&'a self,model:&'a ImagesModel,context:&'a ImagesContext,options:Option<ImagesOptions>)->BoxFuture<'a,AssistantImages>{
        self.calls.lock().expect("calls").push((model.clone(),context.clone(),options.expect("options")));
        Box::pin(async move{AssistantImages{api:model.api.clone(),provider:model.provider.clone(),model:model.id.clone(),output:vec![ContentBlock::Text(TextContent{text:"revised".into(),..Default::default()}),ContentBlock::Image(ImageContent{data:"AQID".into(),mime_type:"image/jpeg".into()})],response_id:None,usage:None,background:Some(ImagesBackground::Opaque),stop_reason:ImagesStopReason::Stop,error_message:None,timestamp:0}})
    }
}
#[tokio::test]
async fn registered_image_client_preserves_auth_and_delivered_file_format()->Result<(),Box<dyn std::error::Error>>{
    let root=tempfile::tempdir()?;let scope=maho_ai::node::provider_scope::ProviderScope::new();
    let provider=Arc::new(Provider{calls:Mutex::new(vec![])});
    let outcome=maho_ai::node::provider_scope::run_with_provider_scope_async(&scope,async{
        maho_ai::images_api_registry::register_images_api_provider("openai-images",provider.clone(),Some("native-image-fixture"))?;
        let mut context=support::context(Arc::new(Registry),Arc::new(support::TestUi::default()));context.cwd=root.path().into();
        let mut api=ExtensionApi::new(LoadedExtension::new("imagegen",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
        maho_ext_imagegen::ImageGen::default().register(&mut api);
        let execute=api.runtime.extension_tool_executor("imagegen","generate_image").ok_or("registered executor")?;
        let result=execute("fixture-image",serde_json::json!({"prompt":"fixture prompt","output_path":"result.png","output_format":"png"}),None,None,&context).await?;
        assert_eq!(result.details["outputFormat"],"jpeg");assert_eq!(result.details["paths"],serde_json::json!(["result.jpg"]));
        assert_eq!(std::fs::read(root.path().join("result.jpg"))?,[1,2,3]);assert!(!root.path().join("result.png").exists());
        assert_eq!(result.details["revisedPrompts"],serde_json::json!(["revised"]));assert_eq!(result.details["transparentBackground"],false);
        let duplicate=execute("duplicate",serde_json::json!({"prompt":"fixture prompt","output_path":"result.png"}),None,None,&context).await?;
        assert_eq!(duplicate.details["reason"],"write_failed");assert_eq!(std::fs::read(root.path().join("result.jpg"))?,[1,2,3]);
        let controller=maho_ai::utils::abort::AbortController::new();controller.abort(None);
        let aborted=execute("aborted",serde_json::json!({"prompt":"fixture prompt","output_path":"aborted.png"}),Some(controller.signal()),None,&context).await?;
        assert_eq!(aborted.details["reason"],"provider_error");assert!(!root.path().join("aborted.png").exists());
        let calls=provider.calls.lock().expect("calls");assert_eq!(calls.len(),2);
        assert_eq!(calls[0].0.api,"openai-images");assert_eq!(calls[0].0.base_url,"https://api.openai.com/v1");
        assert_eq!(calls[0].2.request.api_key.as_deref(),Some("fixture-image-secret"));assert_eq!(calls[0].2.request.headers.as_ref().expect("headers")["X-Fixture"],Some("image".into()));
        assert_eq!(calls[0].1.input.len(),1);assert!(!serde_json::to_string(&result)?.contains("fixture-image-secret"));
        Ok::<(),Box<dyn std::error::Error>>(())
    }).await;
    scope.close();root.close()?;outcome??;Ok(())
}
