use maho_ai::types::{ModelThinkingLevel,AssistantMessage,ContentBlock,StopReason,ThinkingLevel};
use crate::arguments::NormalizedLookAtArgs;
pub const LOOK_AT_TIMEOUT_MS:u64=120_000;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LookAtRunResult { pub model:String,pub sources:Vec<String>,pub mime_types:Vec<String>,pub text:String }
pub fn input_paths(args:&NormalizedLookAtArgs)->Vec<String> { args.args.file_paths.clone().unwrap_or_else(||args.args.file_path.as_ref().filter(|path|!path.is_empty()).cloned().into_iter().collect()) }
pub fn input_data(args:&NormalizedLookAtArgs)->Vec<String> { args.args.image_data_list.clone().unwrap_or_else(||args.args.image_data.as_ref().filter(|data|!data.is_empty()).cloned().into_iter().collect()) }
pub fn to_stream_reasoning(level:Option<ModelThinkingLevel>)->Option<ThinkingLevel> { match level { None|Some(ModelThinkingLevel::Off)=>None,Some(ModelThinkingLevel::Minimal)=>Some(ThinkingLevel::Minimal),Some(ModelThinkingLevel::Low)=>Some(ThinkingLevel::Low),Some(ModelThinkingLevel::Medium)=>Some(ThinkingLevel::Medium),Some(ModelThinkingLevel::High)=>Some(ThinkingLevel::High),Some(ModelThinkingLevel::Xhigh)=>Some(ThinkingLevel::Xhigh),Some(ModelThinkingLevel::Max)=>Some(ThinkingLevel::Max) } }
pub fn response_text(response:&AssistantMessage,aborted:bool)->Result<String,String> {
    if response.stop_reason==StopReason::Error { return Err(format!("Vision model failed to analyze the supplied media: {}",response.error_message.as_deref().unwrap_or("The vision provider returned an unspecified error."))); }
    if response.stop_reason==StopReason::Aborted || aborted { return Err("look_at analysis was aborted.".into()); }
    let text=response.content.iter().filter_map(|block|match block { ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None }).collect::<Vec<_>>().join("\n").trim().to_owned();
    if text.is_empty() { Err("Vision model returned no analysis text. Try a clearer goal or another image.".into()) } else { Ok(text) }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn message(stop:&str,text:&str)->AssistantMessage { serde_json::from_value(json!({"content":[{"type":"text","text":text}],"api":"test","provider":"test","model":"vision","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"timestamp":0})).unwrap() }
    #[test] fn response_text_trim_and_empty_error() { assert_eq!(response_text(&message("stop","  extracted  "),false).unwrap(),"extracted"); assert!(response_text(&message("stop","  "),false).unwrap_err().contains("no analysis text")); }
    #[test] fn provider_error_precedes_abort() { assert!(response_text(&message("error","text"),true).unwrap_err().contains("unspecified error")); assert_eq!(response_text(&message("stop","text"),true).unwrap_err(),"look_at analysis was aborted."); }
    #[test] fn off_reasoning_is_omitted() { assert_eq!(to_stream_reasoning(Some(ModelThinkingLevel::Off)),None); assert_eq!(to_stream_reasoning(Some(ModelThinkingLevel::High)),Some(ThinkingLevel::High)); }
}
