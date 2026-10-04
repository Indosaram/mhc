use std::collections::BTreeMap;
use serde_json::{Map, Value};
#[derive(Debug, PartialEq)]
pub enum AssistantCommitOutcome { Clean, Rewritten, NotResident }
fn semantic_content_block(block: &Value) -> Value {
    let keys: &[&str] = match block["type"].as_str() {
        Some("text") => &["type", "text"],
        Some("thinking") => &["type", "thinking", "thinkingSignature"],
        Some("toolCall") => &["type", "id", "name", "arguments"],
        _ => return block.clone(),
    };
    Value::Object(keys.iter().filter_map(|key| block.get(*key).map(|value| ((*key).into(), value.clone()))).collect())
}
pub fn assistant_content_hash(message: &Value) -> String {
    let mut semantic = Map::new();
    for key in ["role", "api", "provider", "model"] { if let Some(value) = message.get(key) { semantic.insert(key.into(), value.clone()); } }
    semantic.insert("content".into(), Value::Array(message["content"].as_array().expect("assistant content").iter().map(semantic_content_block).collect()));
    crate::session_sync::session_sync_digest(&Value::Object(semantic))
}
pub fn is_resident_assistant(message: &Value, model: &str) -> bool {
    message["api"] == "claude-sdk-oauth" && message["provider"] == "anthropic-subscription" && message["model"] == model
}
pub fn is_terminal_failure(message: &Value) -> bool { matches!(message["stopReason"].as_str(), Some("error" | "aborted")) }
#[derive(Default)]
pub struct AssistantCommitBoundary { provider_final_by_key: BTreeMap<String, String> }
impl AssistantCommitBoundary {
    pub fn capture_provider_final(&mut self, key: &str, message: &Value) { self.provider_final_by_key.insert(key.into(), assistant_content_hash(message)); }
    pub fn commit(&mut self, key: &str, message: &Value, model: &str) -> AssistantCommitOutcome {
        let provider_final = self.provider_final_by_key.remove(key);
        if !is_resident_assistant(message, model) { return AssistantCommitOutcome::NotResident; }
        if provider_final.is_none_or(|hash| hash == assistant_content_hash(message)) { AssistantCommitOutcome::Clean } else { AssistantCommitOutcome::Rewritten }
    }
    pub fn forget(&mut self, key: &str) { self.provider_final_by_key.remove(key); }
}
#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    #[test]
    fn pipeline_metadata_is_not_a_rewrite_but_answer_changes_are() {
        let streamed = json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"test","content":[{"type":"thinking","thinking":"why","thinkingSignature":"sig","index":0},{"type":"text","text":"answer","index":1},{"type":"toolCall","id":"c","name":"echo","arguments":{"text":"hi"},"partialJson":"{}","index":2}]});
        let mut committed = streamed.clone(); committed["content"][0]["startedAt"] = json!(5); committed["content"][1]["textSignature"] = json!("later");
        let mut boundary = AssistantCommitBoundary::default(); boundary.capture_provider_final("s", &streamed); assert_eq!(boundary.commit("s", &committed, "test"), AssistantCommitOutcome::Clean);
        for (index, field, value) in [(0,"thinking",json!("else")), (1,"text",json!("other")), (2,"arguments",json!({"text":"bye"}))] {
            let mut rewritten = committed.clone(); rewritten["content"][index][field] = value; boundary.capture_provider_final("s", &streamed); assert_eq!(boundary.commit("s", &rewritten, "test"), AssistantCommitOutcome::Rewritten);
        }
    }
    #[test]
    fn missing_capture_is_clean_and_nonresident_commit_consumes_capture() {
        let message = json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"test","content":[]});
        let mut boundary = AssistantCommitBoundary::default(); assert_eq!(boundary.commit("s", &message, "test"), AssistantCommitOutcome::Clean);
        boundary.capture_provider_final("s", &message); assert_eq!(boundary.commit("s", &message, "other"), AssistantCommitOutcome::NotResident); assert!(boundary.provider_final_by_key.is_empty());
        assert!(is_terminal_failure(&json!({"stopReason":"aborted"}))); assert!(!is_terminal_failure(&json!({"stopReason":"stop"})));
    }
}
