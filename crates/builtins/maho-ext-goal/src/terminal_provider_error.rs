use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_ext_api::AbortSource;
use maho_core::agent_abort_provenance::AgentEndEvent;
use crate::last_assistant_message::last_assistant_message;
fn is_sdk_oauth_account_exhaustion(message: Option<&AssistantMessage>) -> bool {
    let Some(message) = message else { return false; };
    if message.api != "claude-sdk-oauth" || message.stop_reason != StopReason::Stop { return false; }
    let text = message.content.iter().map(|part| match part { ContentBlock::Text(text) => text.text.as_str(), _ => "" }).collect::<Vec<_>>().join("\n");
    ["API Error: Server is temporarily limiting requests", "accounts exhausted"].iter().all(|marker| text.contains(marker))
}
fn is_codex_policy_rejection(message: &AssistantMessage) -> bool {
    if message.api != "openai-codex-responses" || message.stop_reason != StopReason::Error { return false; }
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| match regex::Regex::new(r"(?i)\A(?:Codex error: )?This request was blocked by our safety systems\.(?: Reason: [^\n\r\x{2028}\x{2029}]+)?\z") { Ok(regex) => regex, Err(error) => panic!("invalid static policy pattern: {error}") }).is_match(message.error_message.as_deref().unwrap_or(""))
}
pub fn did_terminal_policy_rejection_end_turn(event: &AgentEndEvent) -> bool {
    if event.will_retry { return false; }
    let Some(message) = last_assistant_message(&event.messages) else { return false; };
    maho_ai::utils::stop_details::is_classifier_refusal(message) || is_codex_policy_rejection(message)
}
pub fn did_terminal_provider_error_end_turn(event: &AgentEndEvent) -> bool {
    if event.abort_source == Some(AbortSource::System) || event.will_retry { return false; }
    let message = last_assistant_message(&event.messages);
    if is_sdk_oauth_account_exhaustion(message) { return true; }
    message.is_some_and(|message| message.stop_reason == StopReason::Error || message.stop_reason == StopReason::Aborted && event.abort_source != Some(AbortSource::User))
}
#[cfg(test)] mod tests {
    use super::*;
    fn event(stop_reason: StopReason, api: &str, error: &str, text: &str) -> AgentEndEvent { let message: maho_agent::types::AgentMessage = serde_json::from_value(serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"api":api,"provider":"faux","model":"faux","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop_reason,"errorMessage":error,"timestamp":0})).unwrap(); AgentEndEvent { messages: vec![message], will_retry: false, aborted: false, abort_source: None } }
    #[test] fn provider_error_is_terminal_without_retry() { let event = event(StopReason::Error, "faux", "failure", ""); let result = did_terminal_provider_error_end_turn(&event); assert!(result); }
    #[test] fn retrying_error_is_not_terminal() { let mut event = event(StopReason::Error, "faux", "failure", ""); event.will_retry = true; let result = did_terminal_provider_error_end_turn(&event); assert!(!result); }
    #[test] fn system_abort_keeps_recovery_path() { let mut event = event(StopReason::Error, "faux", "failure", ""); event.abort_source = Some(AbortSource::System); let result = did_terminal_provider_error_end_turn(&event); assert!(!result); }
    #[test] fn user_abort_is_not_provider_error() { let mut event = event(StopReason::Aborted, "faux", "", ""); event.abort_source = Some(AbortSource::User); let result = did_terminal_provider_error_end_turn(&event); assert!(!result); }
    #[test] fn source_less_abort_is_terminal() { let event = event(StopReason::Aborted, "faux", "", ""); let result = did_terminal_provider_error_end_turn(&event); assert!(result); }
    #[test] fn sdk_exhaustion_clean_stop_is_terminal() { let event = event(StopReason::Stop, "claude-sdk-oauth", "", "API Error: Server is temporarily limiting requests: 5 accounts exhausted. Retry in 123s"); let result = did_terminal_provider_error_end_turn(&event); assert!(result); }
    #[test] fn exhaustion_requires_sdk_identity() { let event = event(StopReason::Stop, "faux", "", "API Error: Server is temporarily limiting requests: accounts exhausted"); let result = did_terminal_provider_error_end_turn(&event); assert!(!result); }
    #[test] fn codex_diagnostic_is_terminal_policy_rejection() { let event = event(StopReason::Error, "openai-codex-responses", "Codex error: This request was blocked by our safety systems. Reason: rejected", ""); let result = did_terminal_policy_rejection_end_turn(&event); assert!(result); }
    #[test] fn same_policy_sentence_other_api_is_not_policy_signal() { let event = event(StopReason::Error, "faux", "This request was blocked by our safety systems.", ""); let result = did_terminal_policy_rejection_end_turn(&event); assert!(!result); }
    #[test] fn ordinary_assistant_text_is_not_policy_signal() { let event = event(StopReason::Stop, "openai-codex-responses", "", "This request was blocked by our safety systems."); let result = did_terminal_policy_rejection_end_turn(&event); assert!(!result); }
}
