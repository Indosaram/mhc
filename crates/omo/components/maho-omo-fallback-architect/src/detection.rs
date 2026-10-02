use serde_json::Value;
pub const FABLE_FIVE_MODEL_ID: &str = "claude-fable-5";

pub fn is_refusal_like_message(message: &Value) -> bool {
    if message["role"] != "assistant" || !matches!(message["stopReason"].as_str(), Some("error" | "toolUse")) { return false; }
    if matches!(message["stopDetails"]["type"].as_str(), Some("refusal" | "sensitive")) { return true; }
    message["errorMessage"].as_str().is_some_and(|text| {
        match regex::Regex::new(r"(?i)This request triggered restrictions on [\s\S]+? and was blocked under Anthropic's Usage Policy\b") {
            Ok(pattern) => pattern.is_match(text),
            Err(error) => panic!("invalid static refusal pattern: {error}"),
        }
    })
}
pub fn is_fable_five_model(model: &Value) -> bool { model["id"] == FABLE_FIVE_MODEL_ID }
pub fn is_model_select_event(payload: &Value) -> bool {
    payload["type"] == "model_select" && payload["source"].is_string() && payload["model"]["id"].is_string()
        && payload.get("previousModel").is_none_or(|model| model["id"].is_string())
}
pub fn is_message_end_event(payload: &Value) -> bool { payload["type"] == "message_end" && payload["message"].is_object() }
pub fn format_model_selector(model: &Value) -> String {
    let id = model["id"].as_str().unwrap_or_default();
    model["provider"].as_str().map_or_else(|| id.into(), |provider| format!("{provider}/{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn refusal_details() { assert!(is_refusal_like_message(&json!({"role":"assistant","stopReason":"error","stopDetails":{"type":"refusal"}}))); }
    #[test] fn sensitive_details() { assert!(is_refusal_like_message(&json!({"role":"assistant","stopReason":"error","stopDetails":{"type":"sensitive"}}))); }
    #[test] fn policy_error() { assert!(is_refusal_like_message(&json!({"role":"assistant","stopReason":"error","errorMessage":"This request triggered restrictions on account 42 and was blocked under Anthropic's Usage Policy."}))); }
    #[test] fn policy_tool_use() { assert!(is_refusal_like_message(&json!({"role":"assistant","stopReason":"toolUse","errorMessage":"This request triggered restrictions on output content and was blocked under Anthropic's Usage Policy"}))); }
    #[test] fn transient_not_refusal() { assert!(!is_refusal_like_message(&json!({"role":"assistant","stopReason":"error","errorMessage":"Request timed out."}))); }
    #[test] fn normal_stop() { assert!(!is_refusal_like_message(&json!({"role":"assistant","stopReason":"stop"}))); }
    #[test] fn user_not_refusal() { assert!(!is_refusal_like_message(&json!({"role":"user","stopDetails":{"type":"refusal"}}))); }
    #[test] fn stop_reason_wins() { for stop in ["aborted","stop"] { assert!(!is_refusal_like_message(&json!({"role":"assistant","stopReason":stop,"stopDetails":{"type":"refusal"}}))); } }
    #[test] fn malformed_message() { for value in [Value::Null,json!("refusal")] { assert!(!is_refusal_like_message(&value)); } }
    #[test] fn fable_any_provider() { for provider in ["anthropic","anthropic-api"] { assert!(is_fable_five_model(&json!({"provider":provider,"id":"claude-fable-5"}))); } }
    #[test] fn other_models() { for value in [Value::Null,json!({"provider":"anthropic"}),json!({"id":"claude-opus-5"})] { assert!(!is_fable_five_model(&value)); } }
    #[test] fn valid_model_event() { assert!(is_model_select_event(&json!({"type":"model_select","model":{"id":"x"},"previousModel":{"id":"claude-fable-5"},"source":"fallback"}))); }
    #[test] fn invalid_model_event() { for value in [Value::Null,json!({"type":"model_select","source":"fallback"}),json!({"type":"model_select","model":{"id":"x"}})] { assert!(!is_model_select_event(&value)); } }
    #[test] fn message_event_guard() { assert!(is_message_end_event(&json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}))); assert!(!is_message_end_event(&json!({"type":"message_end"}))); assert!(!is_message_end_event(&json!(42))); }
}
