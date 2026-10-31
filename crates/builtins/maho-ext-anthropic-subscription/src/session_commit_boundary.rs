use serde_json::{Map, Value};
use std::collections::BTreeMap;
#[derive(Debug, PartialEq)]
pub enum AssistantCommitOutcome {
    Clean,
    Rewritten,
    NotResident,
}
/// senpi `AssistantCommitResult`: the outcome plus, on a rewrite, the first diverged field path (senpi#1975).
/// Paths only, never values.
#[derive(Debug, PartialEq)]
pub struct AssistantCommitResult {
    pub outcome: AssistantCommitOutcome,
    pub diverged_path: Option<String>,
}
fn semantic_content_block(block: &Value) -> Value {
    let keys: &[&str] = match block["type"].as_str() {
        Some("text") => &["type", "text"],
        Some("thinking") => &["type", "thinking", "thinkingSignature"],
        Some("toolCall") => &["type", "id", "name", "arguments"],
        _ => return block.clone(),
    };
    Value::Object(
        keys.iter()
            .filter_map(|key| block.get(*key).map(|value| ((*key).into(), value.clone())))
            .collect(),
    )
}
fn semantic_projection(message: &Value) -> Value {
    let mut semantic = Map::new();
    for key in ["role", "api", "provider", "model"] {
        if let Some(value) = message.get(key) {
            semantic.insert(key.into(), value.clone());
        }
    }
    semantic.insert(
        "content".into(),
        Value::Array(
            message["content"]
                .as_array()
                .expect("assistant content")
                .iter()
                .map(semantic_content_block)
                .collect(),
        ),
    );
    Value::Object(semantic)
}
pub fn assistant_content_hash(message: &Value) -> String {
    crate::session_sync::session_sync_digest(&semantic_projection(message))
}
pub fn is_resident_assistant(message: &Value, model: &str) -> bool {
    message["api"] == "claude-sdk-oauth"
        && message["provider"] == "anthropic-subscription"
        && message["model"] == model
}
pub fn is_terminal_failure(message: &Value) -> bool {
    matches!(message["stopReason"].as_str(), Some("error" | "aborted"))
}
#[derive(Default)]
pub struct AssistantCommitBoundary {
    provider_final_by_key: BTreeMap<String, (String, Value)>,
}
impl AssistantCommitBoundary {
    /// The captured projection is an owned copy (upstream `snapshotProjection`), so a later in-place rewrite of the
    /// message cannot follow into it.
    pub fn capture_provider_final(&mut self, key: &str, message: &Value) {
        let projection = semantic_projection(message);
        self.provider_final_by_key.insert(
            key.into(),
            (
                crate::session_sync::session_sync_digest(&projection),
                projection,
            ),
        );
    }
    pub fn commit(&mut self, key: &str, message: &Value, model: &str) -> AssistantCommitResult {
        let provider_final = self.provider_final_by_key.remove(key);
        if !is_resident_assistant(message, model) {
            return AssistantCommitResult {
                outcome: AssistantCommitOutcome::NotResident,
                diverged_path: None,
            };
        }
        let Some((hash, projection)) = provider_final else {
            return AssistantCommitResult {
                outcome: AssistantCommitOutcome::Clean,
                diverged_path: None,
            };
        };
        let committed = semantic_projection(message);
        if hash == crate::session_sync::session_sync_digest(&committed) {
            return AssistantCommitResult {
                outcome: AssistantCommitOutcome::Clean,
                diverged_path: None,
            };
        }
        AssistantCommitResult {
            outcome: AssistantCommitOutcome::Rewritten,
            diverged_path: Some(crate::session_commit_divergence::first_diverged_path(
                &projection,
                &committed,
            )),
        }
    }
    pub fn forget(&mut self, key: &str) {
        self.provider_final_by_key.remove(key);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn pipeline_metadata_is_not_a_rewrite_but_answer_changes_are() {
        let streamed = json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"test","content":[{"type":"thinking","thinking":"why","thinkingSignature":"sig","index":0},{"type":"text","text":"answer","index":1},{"type":"toolCall","id":"c","name":"echo","arguments":{"text":"hi"},"partialJson":"{}","index":2}]});
        let mut committed = streamed.clone();
        committed["content"][0]["startedAt"] = json!(5);
        committed["content"][1]["textSignature"] = json!("later");
        let mut boundary = AssistantCommitBoundary::default();
        boundary.capture_provider_final("s", &streamed);
        assert_eq!(
            boundary.commit("s", &committed, "test").outcome,
            AssistantCommitOutcome::Clean
        );
        for (index, field, value, path) in [
            (0, "thinking", json!("else"), "content[0].thinking"),
            (1, "text", json!("other"), "content[1].text"),
            (
                2,
                "arguments",
                json!({"text":"bye"}),
                "content[2].arguments.text",
            ),
        ] {
            let mut rewritten = committed.clone();
            rewritten["content"][index][field] = value;
            boundary.capture_provider_final("s", &streamed);
            assert_eq!(
                boundary.commit("s", &rewritten, "test"),
                AssistantCommitResult {
                    outcome: AssistantCommitOutcome::Rewritten,
                    diverged_path: Some(path.into())
                }
            );
        }
    }
    #[test]
    fn missing_capture_is_clean_and_nonresident_commit_consumes_capture() {
        let message = json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"test","content":[]});
        let mut boundary = AssistantCommitBoundary::default();
        assert_eq!(
            boundary.commit("s", &message, "test").outcome,
            AssistantCommitOutcome::Clean
        );
        boundary.capture_provider_final("s", &message);
        assert_eq!(
            boundary.commit("s", &message, "other").outcome,
            AssistantCommitOutcome::NotResident
        );
        assert!(boundary.provider_final_by_key.is_empty());
        assert!(is_terminal_failure(&json!({"stopReason":"aborted"})));
        assert!(!is_terminal_failure(&json!({"stopReason":"stop"})));
    }
}
