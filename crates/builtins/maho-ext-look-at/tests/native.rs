struct ActiveLook(maho_ext_look_at::runner::VisionModelRunner);
impl maho_ext_api::Extension for ActiveLook {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        maho_ext_look_at::index::LookAtExtension {runner:self.0.clone()}.register(api);
        let runtime=api.runtime.clone();
        api.on(maho_ext_api::EventKind::BeforeAgentStart,std::sync::Arc::new(move |_,_|{let runtime=runtime.clone();Box::pin(async move {runtime.session_actions()?.set_active_tools(vec!["look_at".into()])?;Ok(maho_ext_api::EventResult::None)})}));
    }
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
