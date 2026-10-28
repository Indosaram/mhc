//! Per-session, per-model thinking-level memory (senpi #3819).
//!
//! Port of senpi `packages/coding-agent/src/core/session-thinking-memory.ts`.

use std::collections::HashMap;
use serde_json::Value;

/// Where the thinking level in force came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingLevelSource {
    Explicit,
    Session,
    Global,
    ModelDefault,
    Default,
    Fallback,
    Clamped,
}

const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn is_thinking_level(value: &str) -> bool {
    THINKING_LEVELS.contains(&value)
}

pub fn model_thinking_key(provider: &str, model_id: &str) -> String {
    format!("{provider}/{model_id}")
}

/// The last level each model ran at on this branch, keyed `provider/id`.
pub fn derive_session_model_thinking_levels(path: &[Value]) -> HashMap<String, String> {
    let mut levels = HashMap::new();
    for entry in path {
        if entry.get("type").and_then(Value::as_str) != Some("thinking_level_change") {
            continue;
        }
        let provider = match entry.get("provider").and_then(Value::as_str) {
            Some(p) => p,
            None => continue,
        };
        let model_id = match entry.get("modelId").and_then(Value::as_str) {
            Some(m) => m,
            None => continue,
        };
        let thinking_level = match entry.get("thinkingLevel").and_then(Value::as_str) {
            Some(l) if is_thinking_level(l) => l,
            _ => continue,
        };
        levels.insert(model_thinking_key(provider, model_id), thinking_level.to_owned());
    }
    levels
}

/// Re-derives only when the branch leaf moved, so repeated reads stay O(1).
#[derive(Clone, Debug, Default)]
pub struct SessionThinkingMemory {
    cached_leaf_id: Option<String>,
    cached_levels: HashMap<String, String>,
}

impl SessionThinkingMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn levels(&mut self, branch: &[Value], leaf_id: Option<&str>) -> HashMap<String, String> {
        let current_leaf = leaf_id.map(str::to_owned);
        if self.cached_leaf_id != current_leaf {
            self.cached_levels = derive_session_model_thinking_levels(branch);
            self.cached_leaf_id = current_leaf;
        }
        self.cached_levels.clone()
    }

    pub fn level_for(
        &mut self,
        branch: &[Value],
        leaf_id: Option<&str>,
        provider: &str,
        model_id: &str,
    ) -> Option<String> {
        let levels = self.levels(branch, leaf_id);
        levels.get(&model_thinking_key(provider, model_id)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derives_levels_keyed_by_model() {
        let entries = vec![
            json!({
                "type": "thinking_level_change",
                "thinkingLevel": "high",
                "provider": "anthropic",
                "modelId": "claude-3-7-sonnet"
            }),
            json!({
                "type": "message",
                "message": { "role": "user", "content": "hi" }
            }),
            json!({
                "type": "thinking_level_change",
                "thinkingLevel": "low",
                "provider": "openai",
                "modelId": "o3-mini"
            }),
            json!({
                "type": "thinking_level_change",
                "thinkingLevel": "minimal",
                "provider": "anthropic",
                "modelId": "claude-3-7-sonnet"
            }),
        ];

        let levels = derive_session_model_thinking_levels(&entries);
        assert_eq!(levels.get("anthropic/claude-3-7-sonnet").map(String::as_str), Some("minimal"));
        assert_eq!(levels.get("openai/o3-mini").map(String::as_str), Some("low"));
    }

    #[test]
    fn memory_caches_by_leaf_id() {
        let mut memory = SessionThinkingMemory::new();
        let entries = vec![
            json!({
                "type": "thinking_level_change",
                "thinkingLevel": "high",
                "provider": "anthropic",
                "modelId": "claude-3-7-sonnet"
            }),
        ];

        let level = memory.level_for(&entries, Some("leaf-1"), "anthropic", "claude-3-7-sonnet");
        assert_eq!(level.as_deref(), Some("high"));
        assert_eq!(memory.cached_leaf_id.as_deref(), Some("leaf-1"));
    }
}
