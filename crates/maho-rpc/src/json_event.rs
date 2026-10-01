//! Streaming wire events omit cumulative snapshots while preserving constant-size metadata.
use std::borrow::Cow;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum JsonEventError {
    #[error("message_update message is not an assistant message")]
    NotAssistant,
    #[error("toolcall_start content at index {0} is not a tool call")]
    NotToolCall(Value),
}

pub fn to_json_event(event: &Value) -> Result<Cow<'_, Value>, JsonEventError> {
    if event.get("type").and_then(Value::as_str) != Some("message_update") { return Ok(Cow::Borrowed(event)); }
    let message = &event["message"];
    if message["role"].as_str() != Some("assistant") { return Err(JsonEventError::NotAssistant); }
    let mut delta = event["assistantMessageEvent"].clone();
    if delta["type"].as_str() == Some("toolcall_start") {
        let index = delta["contentIndex"].as_u64().and_then(|n| usize::try_from(n).ok());
        let tool = index.and_then(|i| delta["partial"]["content"].get(i));
        let Some(tool) = tool.filter(|tool| tool["type"].as_str() == Some("toolCall")) else { return Err(JsonEventError::NotToolCall(delta["contentIndex"].clone())); };
        let id = tool.get("id").cloned();
        let name = tool.get("name").cloned();
        if let Some(object) = delta.as_object_mut() {
            object.remove("partial");
            if let Some(id) = id { object.insert("id".into(),id); }
            if let Some(name) = name { object.insert("toolName".into(),name); }
        }
    } else if let Some(object) = delta.as_object_mut() { object.remove("partial"); }
    let mut result = json!({"type":"message_update","usage":message["usage"],"assistantMessageEvent":delta});
    if let Some(name) = event.get("resolvedToolName") { result["resolvedToolName"] = name.clone(); }
    Ok(Cow::Owned(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn removes_snapshots_and_preserves_usage() {
        let event = json!({"type":"message_update","message":{"role":"assistant","usage":{"input":2}},"assistantMessageEvent":{"type":"text_delta","delta":"hello","partial":{"content":[]}},"resolvedToolName":"bash"});
        assert_eq!(to_json_event(&event).unwrap().as_ref(),&json!({"type":"message_update","usage":{"input":2},"assistantMessageEvent":{"type":"text_delta","delta":"hello"},"resolvedToolName":"bash"}));
    }
    #[test] fn tool_start_retains_id_and_name() {
        let event = json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"toolcall_start","contentIndex":0,"partial":{"content":[{"type":"toolCall","id":"call1","name":"bash"}]}}});
        assert_eq!(to_json_event(&event).unwrap()["assistantMessageEvent"],json!({"type":"toolcall_start","contentIndex":0,"id":"call1","toolName":"bash"}));
    }
    #[test] fn refuses_invalid_tool_start() {
        let event = json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"toolcall_start","contentIndex":0,"partial":{"content":[{"type":"text","text":"oops"}]}}});
        assert!(matches!(to_json_event(&event),Err(JsonEventError::NotToolCall(_))));
    }
    #[test] fn unrelated_event_is_borrowed() { assert!(matches!(to_json_event(&json!({"type":"agent_end"})).unwrap(),Cow::Borrowed(_))); }
}
