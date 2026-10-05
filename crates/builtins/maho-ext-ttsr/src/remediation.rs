use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use serde::{Deserialize, Serialize};
use crate::{prompts::{LEAK_ERROR_MESSAGE, render_system_interrupt}, types::{TTSR_INJECTION_CUSTOM_TYPE, TtsrStreamSource}};
const TRUNCATION_MARKER: &str = "[output interrupted by stream rule]";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ErrorShellReplacement { pub role: String, pub content: Vec<ContentBlock>, pub stop_reason: StopReason, pub error_message: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TtsrNudgeMessage { pub custom_type: String, pub content: String, pub display: bool, pub details: NudgeDetails }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NudgeDetails { pub rules: Vec<String> }
pub fn build_error_shell_replacement() -> ErrorShellReplacement { ErrorShellReplacement { role: "assistant".into(), content: vec![], stop_reason: StopReason::Error, error_message: LEAK_ERROR_MESSAGE.into() } }
pub fn build_nudge_message(rule_name: &str, rule_content: &str) -> TtsrNudgeMessage { TtsrNudgeMessage { custom_type: TTSR_INJECTION_CUSTOM_TYPE.into(), content: render_system_interrupt(rule_name, rule_content), display: false, details: NudgeDetails { rules: vec![rule_name.into()] } } }
pub fn build_truncate_replacement(mut message: AssistantMessage, garbage_start_offset: usize, stream_kind: TtsrStreamSource) -> AssistantMessage {
    if stream_kind == TtsrStreamSource::Tool {
        message.content.retain(|block| !matches!(block, ContentBlock::ToolCall(_)));
        message.content.push(ContentBlock::text(TRUNCATION_MARKER));
        if message.stop_reason == StopReason::ToolUse { message.stop_reason = StopReason::Aborted; }
        return message;
    }
    let mut remaining = garbage_start_offset;
    let mut truncated = false;
    message.content = message.content.into_iter().filter_map(|mut block| {
        let text = match (&mut block, stream_kind) { (ContentBlock::Text(text), TtsrStreamSource::Text) => Some(&mut text.text), (ContentBlock::Thinking(thinking), TtsrStreamSource::Thinking) => Some(&mut thinking.thinking), _ => None };
        if let Some(text) = text {
            if truncated { return None; }
            let length = text.encode_utf16().count();
            if remaining > length { remaining -= length; } else { *text = String::from_utf16_lossy(&text.encode_utf16().take(remaining).collect::<Vec<_>>()); truncated = true; }
        }
        Some(block)
    }).collect();
    message.content.push(ContentBlock::text(TRUNCATION_MARKER));
    if message.error_message.as_ref().is_some_and(|error| error.eq_ignore_ascii_case("Request timed out") || error.eq_ignore_ascii_case("Request timed out.")) { message.error_message = None; }
    message
}
#[cfg(test)] mod tests {
    use super::*;
    fn message(content: serde_json::Value, stop_reason: StopReason, error: Option<&str>) -> AssistantMessage { serde_json::from_value(serde_json::json!({"content":content,"api":"faux","provider":"faux","model":"fixture-model","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop_reason,"errorMessage":error,"timestamp":123})).unwrap() }
    #[test] fn shell_is_empty_terminal_error() { let result = build_error_shell_replacement(); assert!(result.content.is_empty()); assert_eq!(result.stop_reason, StopReason::Error); }
    #[test] fn shell_does_not_contain_control_tokens() { let result = build_error_shell_replacement(); assert!(!result.error_message.contains("<|")); assert!(!result.error_message.contains("|>")); }
    #[test] fn shell_is_retryable_by_real_classifier() { let shell = build_error_shell_replacement(); let result = maho_ai::utils::retry::is_retryable_assistant_error(&message(serde_json::json!([]), shell.stop_reason, Some(&shell.error_message))); assert!(result); }
    #[test] fn thinking_truncation_preserves_timestamp_and_stop() { let input = message(serde_json::json!([{"type":"thinking","thinking":"steady reasoning abcdefghijklmnop"}]), StopReason::Aborted, None); let result = build_truncate_replacement(input, 24, TtsrStreamSource::Thinking); assert_eq!(result.timestamp, 123); assert_eq!(result.stop_reason, StopReason::Aborted); assert!(matches!(&result.content[0], ContentBlock::Thinking(t) if t.thinking == "steady reasoning abcdefg")); }
    #[test] fn text_truncation_uses_offset() { let input = message(serde_json::json!([{"type":"text","text":"abcdef"}]), StopReason::Aborted, None); let result = build_truncate_replacement(input, 3, TtsrStreamSource::Text); assert!(matches!(&result.content[0], ContentBlock::Text(t) if t.text == "abc")); }
    #[test] fn walks_concatenated_same_kind_blocks() { let input = message(serde_json::json!([{"type":"thinking","thinking":"good"},{"type":"text","text":"kept"},{"type":"thinking","thinking":"garbage"},{"type":"thinking","thinking":"late garbage"}]), StopReason::Aborted, None); let result = build_truncate_replacement(input, 6, TtsrStreamSource::Thinking); assert_eq!(result.content.len(), 4); assert!(matches!(&result.content[2], ContentBlock::Thinking(t) if t.thinking == "ga")); }
    #[test] fn transport_timeout_is_removed() { let input = message(serde_json::json!([{"type":"text","text":"abcdef"}]), StopReason::Aborted, Some("Request timed out.")); let result = build_truncate_replacement(input, 2, TtsrStreamSource::Text); assert!(result.error_message.is_none()); assert_eq!(result.model, "fixture-model"); }
    #[test] fn other_provider_error_is_preserved() { let input = message(serde_json::json!([{"type":"text","text":"abcdef"}]), StopReason::Error, Some("provider exploded")); let result = build_truncate_replacement(input, 2, TtsrStreamSource::Text); assert_eq!(result.error_message.as_deref(), Some("provider exploded")); }
    #[test] fn truncated_abort_is_not_retryable() { let input = message(serde_json::json!([{"type":"thinking","thinking":"steady reasoning"}]), StopReason::Aborted, None); let result = maho_ai::utils::retry::is_retryable_assistant_error(&build_truncate_replacement(input, 6, TtsrStreamSource::Thinking)); assert!(!result); }
    #[test] fn nudge_is_hidden_and_attributes_rule() { let result = build_nudge_message("collapse-repetition", "instruction"); assert!(!result.display); assert_eq!(result.custom_type, TTSR_INJECTION_CUSTOM_TYPE); assert_eq!(result.details.rules, ["collapse-repetition"]); }
}
