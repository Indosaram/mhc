use maho_ai::types::{ModelThinkingLevel,AssistantMessage,ContentBlock,StopReason,ThinkingLevel};
use crate::arguments::NormalizedLookAtArgs;
pub const LOOK_AT_TIMEOUT_MS:u64=120_000;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LookAtRunResult { pub model:String,pub sources:Vec<String>,pub mime_types:Vec<String>,pub text:String }
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
}
