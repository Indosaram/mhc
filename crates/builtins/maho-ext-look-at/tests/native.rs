struct ActiveLook(maho_ext_look_at::runner::VisionModelRunner);
struct VisionRegistry(maho_ext_api::Model);
impl maho_ext_api::ModelRegistry for VisionRegistry {
    fn get_all(&self)->Vec<maho_ext_api::Model>{vec![self.0.clone()]}
    fn get_available(&self)->Vec<maho_ext_api::Model>{self.get_all()}
    fn find(&self,_:&str,_:&str)->Option<maho_ext_api::Model>{Some(self.0.clone())}
    fn has_configured_auth(&self,_:&maho_ext_api::Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->maho_ext_api::ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("provider fallback forbidden")})}
    fn get_api_key_and_headers<'a>(&'a self,model:&'a maho_ext_api::Model)->maho_ext_api::ExtensionFuture<'a,maho_ext_api::ResolvedRequestAuth>{Box::pin(async move{assert_eq!(model.id,self.0.id);Ok(maho_ext_api::ResolvedRequestAuth{auth:maho_ai::models::ProviderAuthResult{api_key:Some("fixture".into()),headers:None,base_url:None},extra_body:None,upstream_model_id:None,service_tier:None,env:None})})}
}
impl maho_ext_api::Extension for ActiveLook {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        maho_ext_look_at::index::LookAtExtension {runner:self.0.clone()}.register(api);
        let runtime=api.runtime.clone();
        api.on(maho_ext_api::EventKind::BeforeAgentStart,std::sync::Arc::new(move |_,_|{let runtime=runtime.clone();Box::pin(async move {runtime.session_actions()?.set_active_tools(vec!["look_at".into()])?;Ok(maho_ext_api::EventResult::None)})}));
    }
}
#[tokio::test]
async fn native_factory_executes_owned_request_chain_and_host_services(){
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
    use serde_json::json;
    let captured=std::sync::Arc::new(std::sync::Mutex::new(None));let captured_stream=captured.clone();
    let stream:maho_ext_look_at::runner::StreamRunner=std::sync::Arc::new(move |_,model,context,options|{
        assert_eq!(model.id,"vision");assert_eq!(options.stream.max_tokens,Some(4096));assert!(options.stream.request.signal.is_some());assert!(context.system_prompt.is_some());assert_eq!(context.messages.len(),1);
        *captured_stream.lock().unwrap()=options.stream.request.signal;
        let request=serde_json::to_value(&context.messages[0]).unwrap();assert_eq!(request["content"][0]["type"],"image");assert_eq!(request["content"][1]["type"],"text");
        let stream=maho_ai::utils::event_stream::create_assistant_message_event_stream();stream.end(Some(faux_assistant_message("owned analysis",FauxAssistantMessageOptions::default())));Ok(stream)
    });
    let processor:maho_ext_look_at::image_input::ImageProcessor=std::sync::Arc::new(|bytes,mime,options|Box::pin(async move {
        assert!(options.auto_resize_images);assert_eq!(bytes,b"\x89PNG\r\n\x1a\n");assert_eq!(mime,"image/png");
        Ok(maho_ext_look_at::image_input::ProcessedImage{data:"iVBORw0KGgo=".into(),mime_type:mime,hints:vec![]})
    }));
    let owned=maho_ext_look_at::runner::create_vision_runner(stream,Some(processor));
    let runner:maho_ext_look_at::runner::VisionModelRunner=std::sync::Arc::new(move |args,ctx,store,signal|{let owned=owned.clone();let captured=captured.clone();Box::pin(async move{
        let mut ctx=ctx.clone();ctx.model_registry=std::sync::Arc::new(VisionRegistry(serde_json::from_value(json!({"id":"vision","name":"vision","provider":"fixture","api":"faux","baseUrl":"","reasoning":false,"input":["text","image"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).unwrap()));
        let parent=maho_ai::utils::abort::AbortController::new();
        assert!(!signal.as_ref().is_some_and(|signal|signal.aborted()));
        let result=owned(args,&ctx,store,Some(parent.signal())).await;
        parent.abort(None);
        assert!(!captured.lock().unwrap().as_ref().unwrap().aborted(),"finally must remove the tool-to-request abort listener");
        result
    })});
    let session=FauxSession::new(FauxScript{name:"owned-vision".into(),prompt:"inspect".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory{path:"builtin:look-at".into(),source_info:maho_ext_api::SourceInfo{source:"builtin".into(),..Default::default()},extension:Box::new(ActiveLook(runner))})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("look_at",serde_json::from_value(json!({"goal":"read labels","image_data":"data:image/png;base64,iVBORw0KGgo="})).unwrap(),Some("owned-vision")),FauxAssistantMessageOptions{stop_reason:Some(maho_ai::types::StopReason::ToolUse),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions::default())]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let tool=result["messages"].as_array().unwrap().iter().find(|message|message["role"]=="toolResult").unwrap();assert_eq!(tool["isError"],false,"{tool}");assert_eq!(tool["details"]["model"],"fixture/vision");
}
#[tokio::test]
async fn native_factory_executes_validated_vision_runner_and_serializes_metadata() {
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
    use serde_json::json;
    let runner:maho_ext_look_at::runner::VisionModelRunner=std::sync::Arc::new(|args,ctx,_,_|Box::pin(async move {
        assert_eq!(args.args.goal,"read labels");assert!(ctx.model.is_some());
        let branch=ctx.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
        let inputs=maho_ext_look_at::image_input::load_look_at_inputs(&maho_ext_look_at::image_input::LookAtImageInputContext {cwd:&ctx.cwd,branch:&branch,auto_resize:false,block_images:false},&maho_ext_look_at::runner::input_paths(args),&maho_ext_look_at::runner::input_data(args)).await.map_err(maho_ext_api::ExtensionFailure::new)?;
        assert_eq!(inputs.len(),1);
        Ok(maho_ext_look_at::runner::LookAtRunResult {model:"faux/vision".into(),sources:inputs.iter().map(|input|input.label.clone()).collect(),mime_types:inputs.iter().map(|input|input.mime_type.clone()).collect(),text:"native vision response".into()})
    }));
    let session=FauxSession::new(FauxScript {name:"look-at-native".into(),prompt:"inspect".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory {path:"builtin:look-at".into(),source_info:maho_ext_api::SourceInfo {source:"builtin".into(),..Default::default()},extension:Box::new(ActiveLook(runner))})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("look_at",serde_json::from_value(json!({"goal":"read labels","image_data":"data:image/png;base64,iVBORw0KGgo="})).unwrap(),Some("vision-call")),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::ToolUse),timestamp:Some(0),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()})]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let tool=result["messages"].as_array().unwrap().iter().find(|message|message["role"]=="toolResult").unwrap();
    assert_eq!(tool["isError"],false,"{tool}");assert_eq!(tool["details"]["model"],"faux/vision");assert_eq!(tool["details"]["sources"],json!(["base64 input"]));assert_eq!(tool["details"]["mimeTypes"],json!(["image/png"]));
}
