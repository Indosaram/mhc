//! Port of senpi's compaction `retained-message-projection.ts`. It lives in maho-core because
//! `session_manager` (upstream session-manager.ts:895-912) projects with it and core cannot
//! depend on the compaction extension crate.
use crate::retained_message_safety::has_unsafe_retained_content;
use serde_json::{Value, json};
use std::collections::HashSet;

/// Bound normalized JSON before estimating tokens or replaying provider state.
pub fn is_safe_bounded_value(value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match value {
        Value::Array(items) => items.iter().all(|item| is_safe_bounded_value(item, depth + 1)),
        Value::Object(items) => items.values().all(|item| is_safe_bounded_value(item, depth + 1)),
        _ => true,
    }
}

/// #3060: a fallback projects the same JSON as the session writer, then omits unsafe
/// payloads rather than poisoning every suffix containing them. The positional
/// mapping is preserved so SessionManager can carry entry provenance onto copies.
pub fn project_retained_messages(messages: &[Value]) -> Vec<Value> {
    let mut omitted_calls: HashSet<String> = HashSet::new();
    messages
        .iter()
        .map(|message| {
            // Values are already the writer's JSON, so only the safety checks remain.
            let safe = is_safe_bounded_value(message, 0) && !has_unsafe_retained_content(std::slice::from_ref(message));
            let role = message.get("role").and_then(Value::as_str);
            if role == Some("assistant") {
                for block in message.get("content").and_then(Value::as_array).into_iter().flatten() {
                    if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                        continue;
                    }
                    let Some(id) = block.get("id").and_then(Value::as_str) else { continue };
                    if safe {
                        omitted_calls.remove(id);
                    } else {
                        omitted_calls.insert(id.to_owned());
                    }
                }
            }
            let orphaned_by_omission = role == Some("toolResult")
                && message.get("toolCallId").and_then(Value::as_str).is_some_and(|id| omitted_calls.contains(id));
            if safe && !orphaned_by_omission {
                return message.clone();
            }
            let timestamp = message.get("timestamp").filter(|value| value.is_number()).cloned().unwrap_or_else(|| json!(0));
            let tool_call_id = message.get("toolCallId").and_then(Value::as_str).filter(|id| !id.is_empty());
            let tool_name = message.get("toolName").and_then(Value::as_str).filter(|name| !name.is_empty());
            if role == Some("toolResult")
                && !orphaned_by_omission
                && let (Some(tool_call_id), Some(tool_name)) = (tool_call_id, tool_name)
            {
                return json!({
                    "role": "toolResult",
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "content": [{"type": "text", "text": "[tool result omitted from retained context: not replay-safe]"}],
                    "isError": true,
                    "timestamp": timestamp,
                });
            }
            // Never strip a signature and replay the now-unsigned assistant/tool call.
            // Omit that assistant and its results together. For a user, retain readable
            // request text even when an attachment or envelope is malformed.
            let text = if role == Some("user") {
                match message.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|block| block.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            let prefix = if text.is_empty() { String::new() } else { format!("{text}\n") };
            json!({
                "role": "user",
                "content": format!("{prefix}[message omitted from retained context: not replay-safe]"),
                "timestamp": timestamp,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_tool_result_becomes_an_error_result_and_keeps_its_pair() {
        let call = json!({"role":"assistant","content":[{"type":"toolCall","id":"c","name":"read","arguments":{}}],"api":"a","provider":"p","model":"m",
            "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
            "stopReason":"toolUse","timestamp":1});
        let result = json!({"role":"toolResult","toolCallId":"c","toolName":"read","content":[{"type":"image","mimeType":"image/png"}],"isError":false,"timestamp":2});
        let projected = project_retained_messages(&[call.clone(), result]);
        assert_eq!(projected[0], call);
        assert_eq!(projected[1]["role"], "toolResult");
        assert_eq!(projected[1]["toolCallId"], "c");
        assert_eq!(projected[1]["isError"], true);
        assert!(!projected[1].to_string().contains("image/png"));
    }

    #[test]
    fn unsafe_assistant_is_omitted_together_with_its_results() {
        let call = json!({"role":"assistant","content":[{"type":"toolCall","id":"c","name":"read","arguments":{}}],"timestamp":1});
        let result = json!({"role":"toolResult","toolCallId":"c","toolName":"read","content":[{"type":"text","text":"ok"}],"isError":false,"timestamp":2});
        let projected = project_retained_messages(&[call, result]);
        for message in &projected {
            assert_eq!(message["role"], "user");
            assert_eq!(message["content"], "[message omitted from retained context: not replay-safe]");
        }
    }

    #[test]
    fn user_text_survives_a_malformed_attachment() {
        let message = json!({"role":"user","content":[{"type":"text","text":"hello"},{"type":"image"}],"timestamp":3});
        let projected = project_retained_messages(&[message]);
        assert_eq!(projected[0]["content"], "hello\n[message omitted from retained context: not replay-safe]");
        assert_eq!(projected[0]["timestamp"], 3);
    }

    #[test]
    fn safe_messages_pass_through_and_missing_timestamp_is_zero() {
        let safe = json!({"role":"user","content":"hi","timestamp":1});
        assert_eq!(project_retained_messages(std::slice::from_ref(&safe)), vec![safe]);
        let projected = project_retained_messages(&[json!({"role":"custom"})]);
        assert_eq!(projected[0]["timestamp"], 0);
    }
}
