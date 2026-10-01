use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_ai::utils::provider_failure_description::{describe_provider_failure_for_user,strip_turn_retry_suppression_prefix};
use maho_ai::utils::retry::ProviderStallDescriptionOptions;
use crate::provider_native_rendering::{format_provider_native_body,format_provider_native_summary};

#[derive(Default,Debug,PartialEq)]
pub struct PrintOutput { pub stdout:String,pub stderr:String,pub exit_code:i32 }
pub fn format_print_result(message: Option<&AssistantMessage>) -> Result<PrintOutput,serde_json::Error> {
    let Some(message) = message else { return Ok(PrintOutput::default()); };
    if matches!(message.stop_reason,StopReason::Error|StopReason::Aborted) {
        let error = describe_provider_failure_for_user(message.error_message.as_deref(),&ProviderStallDescriptionOptions::default())
            .unwrap_or_else(|| {
                let stripped = strip_turn_retry_suppression_prefix(message.error_message.as_deref().unwrap_or(""));
                if stripped.is_empty() { format!("Request {}",message.stop_reason.as_str()) } else { stripped }
            });
        return Ok(PrintOutput { stdout:String::new(),stderr:format!("{error}\n"),exit_code:1 });
    }
    let mut output = PrintOutput::default();
    for content in &message.content {
        match content {
            ContentBlock::Text(text) => { output.stdout.push_str(&text.text);output.stdout.push('\n'); }
            ContentBlock::ProviderNative(content) => {
                output.stdout.push_str(&format_provider_native_summary(message.provider.as_str(),&content.subtype,&content.raw,false));output.stdout.push('\n');
                output.stdout.push_str(&format_provider_native_body(&content.subtype,&content.raw,false)?);output.stdout.push('\n');
            }
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::Image(_) => {}
        }
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn message(content:serde_json::Value,stop:&str,error:Option<&str>) -> AssistantMessage {
        serde_json::from_value(json!({"role":"assistant","content":content,"api":"openai-completions","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"errorMessage":error,"timestamp":0})).unwrap()
    }
    #[test] fn prints_text_blocks_with_newlines() { let message=message(json!([{"type":"text","text":"one"},{"type":"text","text":"two"}]),"stop",None);assert_eq!(format_print_result(Some(&message)).unwrap().stdout,"one\ntwo\n"); }
    #[test] fn provider_native_uses_summary_and_body() { let message=message(json!([{"type":"providerNative","subtype":"image_generation_call","raw":{"result":"YWJj"}}]),"stop",None);let output=format_print_result(Some(&message)).unwrap();assert!(output.stdout.ends_with("\n3 bytes\n"));assert!(!output.stdout.contains("YWJj")); }
    #[test] fn failed_turn_strips_internal_marker_and_exits_one() { let message=message(json!([]),"error",Some("senpi:no-turn-retry:failed"));assert_eq!(format_print_result(Some(&message)).unwrap(),PrintOutput { stdout:String::new(),stderr:"failed\n".into(),exit_code:1 }); }
    #[test] fn missing_error_uses_terminal_status() { let message=message(json!([]),"aborted",None);assert_eq!(format_print_result(Some(&message)).unwrap().stderr,"Request aborted\n"); }
    #[test] fn absent_assistant_outputs_nothing() { assert_eq!(format_print_result(None).unwrap(),PrintOutput::default()); }
}
