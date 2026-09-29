//! Port of senpi packages/ai/src/utils/deferred-tools.ts.

use crate::types::{ContentBlock, Context, Message, Tool};
use indexmap::IndexMap;
use std::collections::HashSet;

pub struct SplitTools {
    pub immediate: Vec<Tool>,
    pub deferred: IndexMap<String, Tool>,
}

/// Split current tools into prefix and transcript-loaded definitions.
pub fn split_deferred_tools(context: &Context, enabled: bool, normalize_name: Option<&dyn Fn(&str) -> String>) -> SplitTools {
    let normalize = |name: &str| normalize_name.map_or_else(|| name.to_owned(), |f| f(name));
    let mut unique: IndexMap<String, Tool> = IndexMap::new();
    for tool in context.tools.iter().flatten() {
        unique.insert(normalize(&tool.name), tool.clone());
    }
    if !enabled {
        return SplitTools { immediate: unique.into_values().collect(), deferred: IndexMap::new() };
    }

    let mut deferred_names = HashSet::new();
    let mut used_names = HashSet::new();
    for message in &context.messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let ContentBlock::ToolCall(call) = block {
                        used_names.insert(normalize(&call.name));
                    }
                }
            }
            Message::ToolResult(result) => {
                for name in result.added_tool_names.iter().flatten() {
                    let normalized = normalize(name);
                    if !used_names.contains(&normalized) {
                        deferred_names.insert(normalized);
                    }
                }
            }
            _ => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = IndexMap::new();
    for (name, tool) in unique {
        if deferred_names.contains(&name) {
            deferred.insert(name, tool);
        } else {
            immediate.push(tool);
        }
    }
    SplitTools { immediate, deferred }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context(value: serde_json::Value) -> Context {
        serde_json::from_value(value).expect("context")
    }

    fn tool_names(tools: impl IntoIterator<Item = Tool>) -> Vec<String> {
        tools.into_iter().map(|tool| tool.name).collect()
    }

    fn base_context() -> serde_json::Value {
        json!({
            "messages": [
                {"role": "user", "content": "hi", "timestamp": 1},
                {
                    "role": "assistant", "content": [{"type": "toolCall", "id": "call_1", "name": "base_tool", "arguments": {}}],
                    "api": "a", "provider": "p", "model": "m",
                    "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
                    "stopReason": "toolUse", "timestamp": 2,
                },
                {
                    "role": "toolResult", "toolCallId": "call_1", "toolName": "base_tool",
                    "content": [{"type": "text", "text": "done"}], "addedToolNames": ["late_tool"],
                    "isError": false, "timestamp": 3,
                },
            ],
            "tools": [
                {"name": "base_tool", "description": "d", "parameters": {}},
                {"name": "late_tool", "description": "d", "parameters": {}},
            ],
        })
    }

    #[test]
    fn defers_a_tool_marked_after_its_result_and_not_yet_used() {
        let ctx = context(base_context());
        let split = split_deferred_tools(&ctx, true, None);
        assert_eq!(tool_names(split.immediate), vec!["base_tool"]);
        assert_eq!(split.deferred.keys().cloned().collect::<Vec<_>>(), vec!["late_tool"]);
    }

    #[test]
    fn keeps_every_tool_immediate_when_disabled() {
        let ctx = context(base_context());
        let split = split_deferred_tools(&ctx, false, None);
        assert_eq!(tool_names(split.immediate), vec!["base_tool", "late_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn keeps_a_tool_immediate_when_it_was_used_before_its_marker() {
        let mut value = base_context();
        value["messages"][1]["content"] = json!([{"type": "toolCall", "id": "call_1", "name": "late_tool", "arguments": {}}]);
        let ctx = context(value);
        let split = split_deferred_tools(&ctx, true, None);
        assert_eq!(tool_names(split.immediate), vec!["base_tool", "late_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn does_not_resurrect_a_marked_tool_missing_from_context_tools() {
        let mut value = base_context();
        value["tools"] = json!([{"name": "base_tool", "description": "d", "parameters": {}}]);
        let ctx = context(value);
        let split = split_deferred_tools(&ctx, true, None);
        assert_eq!(tool_names(split.immediate), vec!["base_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn normalizes_names_before_checking_prior_usage_and_markers() {
        let mut value = base_context();
        value["messages"][1]["content"] = json!([{"type": "toolCall", "id": "call_1", "name": "LATE_TOOL", "arguments": {}}]);
        let ctx = context(value);
        let lower = |name: &str| name.to_lowercase();
        let split = split_deferred_tools(&ctx, true, Some(&lower));
        assert_eq!(tool_names(split.immediate), vec!["base_tool", "late_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn deduplicates_tools_after_normalization_keeping_the_last_definition() {
        let value = json!({
            "messages": [],
            "tools": [
                {"name": "read", "description": "old", "parameters": {}},
                {"name": "Read", "description": "canonical", "parameters": {}},
            ],
        });
        let lower = |name: &str| name.to_lowercase();
        let ctx = context(value);
        let split = split_deferred_tools(&ctx, true, Some(&lower));
        assert_eq!(split.immediate.len(), 1);
        assert_eq!(split.immediate[0].description, "canonical");
    }
}
