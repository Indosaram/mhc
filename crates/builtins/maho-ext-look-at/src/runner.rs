use maho_ai::types::{ModelThinkingLevel,AssistantMessage,ContentBlock,StopReason,ThinkingLevel};
use crate::arguments::NormalizedLookAtArgs;
pub const LOOK_AT_TIMEOUT_MS:u64=120_000;
pub type VisionModelRunner=std::sync::Arc<dyn for<'a> Fn(&'a crate::arguments::NormalizedLookAtArgs,&'a maho_ext_api::ExtensionContext,&'a crate::settings::LookAtStore,Option<maho_ai::utils::abort::AbortSignal>)->maho_ext_api::ExtensionFuture<'a,LookAtRunResult>+Send+Sync>;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LookAtRunResult { pub model:String,pub sources:Vec<String>,pub mime_types:Vec<String>,pub text:String }
pub async fn preflight_model_auth(registry:&dyn maho_ext_api::ModelRegistry,model:&maho_ai::model::Model)->Result<maho_ext_api::ResolvedRequestAuth,maho_ext_api::ExtensionFailure> {
    registry.get_api_key_and_headers(model).await.map_err(|error|maho_ext_api::ExtensionFailure::new(format!("look_at cannot use {}/{}: {error}. Configure credentials with /login {} and try again.",model.provider,model.id,model.provider)))
}
pub type StreamService=dyn Fn(&maho_ext_api::ExtensionContext,&maho_ai::model::Model,&maho_ai::types::Context,maho_ai::types::SimpleStreamOptions)->Result<maho_ai::utils::event_stream::AssistantMessageEventStream,maho_ext_api::ExtensionFailure>+Send+Sync;
pub type StreamRunner=std::sync::Arc<StreamService>;
pub fn create_vision_runner(stream:StreamRunner,processor:Option<crate::image_input::ImageProcessor>)->VisionModelRunner {
    std::sync::Arc::new(move |args,ctx,store,signal|{let stream=stream.clone();let processor=processor.clone();Box::pin(async move {run_look_at(args,ctx,store,signal,stream.as_ref(),processor.as_ref()).await})})
}
pub async fn run_look_at(args:&NormalizedLookAtArgs,ctx:&maho_ext_api::ExtensionContext,store:&crate::settings::LookAtStore,signal:Option<maho_ai::utils::abort::AbortSignal>,stream:&StreamService,processor:Option<&crate::image_input::ImageProcessor>)->Result<LookAtRunResult,maho_ext_api::ExtensionFailure> {
    use maho_ai::utils::abort::AbortController;
    let check=||if signal.as_ref().is_some_and(|signal|signal.aborted()){Err(maho_ext_api::ExtensionFailure::new("look_at analysis was aborted."))}else{Ok(())};
    check()?;
    let settings=ctx.get_image_settings()?;
    let branch=ctx.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
    let inputs=crate::image_input::load_look_at_inputs_with_processor(&crate::image_input::LookAtImageInputContext{cwd:&ctx.cwd,branch:&branch,auto_resize:settings.auto_resize,block_images:settings.block_images},&input_paths(args),&input_data(args),processor).await.map_err(maho_ext_api::ExtensionFailure::new)?;
    let chain=crate::settings::load_chain_from_context(store,||ctx.get_look_at_settings())?;
    let resolved=crate::model_selector::resolve_vision_model(&chain,&ctx.model_registry.get_available()).ok_or_else(||maho_ext_api::ExtensionFailure::new("No image-capable model is available. Configure a vision-capable provider and try look_at again."))?;
    preflight_model_auth(ctx.model_registry.as_ref(),&resolved.model).await?;
    check()?;
    let controller=AbortController::new();let request_signal=controller.signal();
    let link=signal.as_ref().map(|signal|{let controller=controller.clone();signal.add_abort_listener(move |reason|controller.abort(Some(reason.clone()))) });
    struct RequestLink {parent:Option<maho_ai::utils::abort::AbortSignal>,id:Option<maho_ai::utils::abort::ListenerId>}
    impl Drop for RequestLink {fn drop(&mut self){if let (Some(parent),Some(id))=(&self.parent,self.id){parent.remove_abort_listener(id);}}}
    let _link=RequestLink{parent:signal.clone(),id:link};
    if let Some(reason)=signal.as_ref().and_then(|signal|signal.reason()){controller.abort(Some(reason));}
    let context=maho_ai::types::Context{system_prompt:Some(crate::prompts::LOOK_AT_SYSTEM_PROMPT.into()),messages:vec![maho_ai::types::Message::User(build_user_message(&args.args.goal,&inputs,std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?.as_millis() as i64))],tools:None};
    let mut options=maho_ai::types::SimpleStreamOptions{reasoning:resolved.thinking_level,..Default::default()};options.stream.max_tokens=Some(4096);options.stream.request.signal=Some(request_signal.clone());
    let response=async {stream(ctx,&resolved.model,&context,options)?.result().await.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))};
    let response=await_response(response,&controller).await;
    let response=response.map_err(|error|if request_signal.aborted(){maho_ext_api::ExtensionFailure::new("look_at analysis was aborted.")}else{error})?;
    run_result(&resolved.model.provider,&resolved.model.id,&inputs,&response,request_signal.aborted()).map_err(|error|maho_ext_api::ExtensionFailure::new(if request_signal.aborted(){"look_at analysis was aborted.".into()}else{error}))
}
async fn await_response(response:impl std::future::Future<Output=Result<AssistantMessage,maho_ext_api::ExtensionFailure>>,controller:&maho_ai::utils::abort::AbortController)->Result<AssistantMessage,maho_ext_api::ExtensionFailure>{
    tokio::pin!(response);
    tokio::select! {
        result=&mut response=>result,
        ()=tokio::time::sleep(std::time::Duration::from_millis(LOOK_AT_TIMEOUT_MS))=>{controller.abort(None);response.await},
    }
}
pub fn run_result(provider:&str,model_id:&str,inputs:&[crate::image_input::LoadedLookAtInput],response:&AssistantMessage,aborted:bool)->Result<LookAtRunResult,String> {
    Ok(LookAtRunResult{model:format!("{provider}/{model_id}"),sources:inputs.iter().map(|input|input.label.clone()).collect(),mime_types:inputs.iter().map(|input|input.mime_type.clone()).collect(),text:response_text(response,aborted)?})
}
pub fn input_paths(args:&NormalizedLookAtArgs)->Vec<String> { args.args.file_paths.clone().unwrap_or_else(||args.args.file_path.as_ref().filter(|path|!path.is_empty()).cloned().into_iter().collect()) }
pub fn input_data(args:&NormalizedLookAtArgs)->Vec<String> { args.args.image_data_list.clone().unwrap_or_else(||args.args.image_data.as_ref().filter(|data|!data.is_empty()).cloned().into_iter().collect()) }
pub fn to_stream_reasoning(level:Option<ModelThinkingLevel>)->Option<ThinkingLevel> { match level { None|Some(ModelThinkingLevel::Off)=>None,Some(ModelThinkingLevel::Minimal)=>Some(ThinkingLevel::Minimal),Some(ModelThinkingLevel::Low)=>Some(ThinkingLevel::Low),Some(ModelThinkingLevel::Medium)=>Some(ThinkingLevel::Medium),Some(ModelThinkingLevel::High)=>Some(ThinkingLevel::High),Some(ModelThinkingLevel::Xhigh)=>Some(ThinkingLevel::Xhigh),Some(ModelThinkingLevel::Max)=>Some(ThinkingLevel::Max) } }
pub fn build_user_message(goal:&str,inputs:&[crate::image_input::LoadedLookAtInput],timestamp:i64)->maho_ai::types::UserMessage {
    let mut content:Vec<_>=inputs.iter().map(|input|ContentBlock::Image(maho_ai::types::ImageContent{data:input.data.clone(),mime_type:input.mime_type.clone()})).collect();
    content.push(ContentBlock::Text(maho_ai::types::TextContent{text:crate::prompts::build_look_at_user_message(goal,&inputs.iter().map(|input|input.label.clone()).collect::<Vec<_>>()),..Default::default()}));
    maho_ai::types::UserMessage{content:maho_ai::types::UserContent::Blocks(content),timestamp}
}
pub fn response_text(response:&AssistantMessage,aborted:bool)->Result<String,String> {
    if response.stop_reason==StopReason::Error { return Err(format!("Vision model failed to analyze the supplied media: {}",response.error_message.as_deref().unwrap_or("The vision provider returned an unspecified error."))); }
    if response.stop_reason==StopReason::Aborted || aborted { return Err("look_at analysis was aborted.".into()); }
    let text=response.content.iter().filter_map(|block|match block { ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None }).collect::<Vec<_>>().join("\n").trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).to_owned();
    if text.is_empty() { Err("Vision model returned no analysis text. Try a clearer goal or another image.".into()) } else { Ok(text) }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn result_metadata_keeps_source_and_mime_order() {
        let inputs=vec![crate::image_input::LoadedLookAtInput{data:String::new(),label:"first".into(),mime_type:"image/png".into()},crate::image_input::LoadedLookAtInput{data:String::new(),label:"second".into(),mime_type:"image/jpeg".into()}];
        let result=run_result("provider","vision",&inputs,&message("stop","analysis"),false).unwrap(); assert_eq!(result.model,"provider/vision"); assert_eq!(result.sources,["first","second"]); assert_eq!(result.mime_types,["image/png","image/jpeg"]); assert!(run_result("provider","vision",&inputs,&message("aborted","analysis"),false).is_err());
    }
    #[test] fn user_message_keeps_media_order_before_goal() { let inputs=vec![crate::image_input::LoadedLookAtInput{data:"data".into(),label:"one".into(),mime_type:"image/png".into()}]; let message=build_user_message("goal",&inputs,0); let maho_ai::types::UserContent::Blocks(blocks)=message.content else { panic!() }; assert!(matches!(&blocks[0],ContentBlock::Image(image) if image.data=="data" && image.mime_type=="image/png")); assert!(matches!(&blocks[1],ContentBlock::Text(_))); assert_eq!(message.timestamp,0); }
    #[test] fn response_trim_uses_javascript_whitespace() { assert_eq!(response_text(&message("stop","\u{feff}text\u{feff}"),false).unwrap(),"text"); assert_eq!(response_text(&message("stop","\u{0085}text\u{0085}"),false).unwrap(),"\u{0085}text\u{0085}"); }
    fn message(stop:&str,text:&str)->AssistantMessage { serde_json::from_value(json!({"content":[{"type":"text","text":text}],"api":"test","provider":"test","model":"vision","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"timestamp":0})).unwrap() }
    #[test] fn response_text_trim_and_empty_error() { assert_eq!(response_text(&message("stop","  extracted  "),false).unwrap(),"extracted"); assert!(response_text(&message("stop","  "),false).unwrap_err().contains("no analysis text")); }
    #[test] fn provider_error_precedes_abort() { assert!(response_text(&message("error","text"),true).unwrap_err().contains("unspecified error")); assert_eq!(response_text(&message("stop","text"),true).unwrap_err(),"look_at analysis was aborted."); }
    #[test] fn off_reasoning_is_omitted() { assert_eq!(to_stream_reasoning(Some(ModelThinkingLevel::Off)),None); assert_eq!(to_stream_reasoning(Some(ModelThinkingLevel::High)),Some(ThinkingLevel::High)); }
    #[tokio::test(start_paused=true)] async fn request_deadline_aborts_then_awaits_signal_gated_stream_settlement(){
        let controller=maho_ai::utils::abort::AbortController::new();
        let start=tokio::time::Instant::now();
        let signal=controller.signal();let observed=signal.clone();
        let (settle,settlement)=tokio::sync::oneshot::channel();
        let stream=maho_ai::utils::event_stream::create_assistant_message_event_stream();let completed=stream.clone();
        let response=async{stream.result().await.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))};
        let request=await_response(response,&controller);tokio::pin!(request);
        tokio::select!{biased;result=&mut request=>panic!("request settled before stream: {result:?}"),()=observed.cancelled()=>{}}
        assert_eq!(start.elapsed(),std::time::Duration::from_millis(LOOK_AT_TIMEOUT_MS));
        let release=async{settlement.await.expect("settlement signal");completed.end(Some(message("stop","late analysis")));};
        settle.send(()).expect("stream still awaiting settlement");
        let (result,())=tokio::join!(request,release);
        let response=result.expect("timeout must not drop stream result");assert!(response_text(&response,signal.aborted()).is_err());
    }
    #[tokio::test(start_paused=true)] async fn successful_request_disposes_deadline(){
        let controller=maho_ai::utils::abort::AbortController::new();
        assert!(await_response(async{Ok(message("stop","analysis"))},&controller).await.is_ok());
        tokio::time::advance(std::time::Duration::from_millis(LOOK_AT_TIMEOUT_MS+1)).await;
        assert!(!controller.signal().aborted());
    }
}
