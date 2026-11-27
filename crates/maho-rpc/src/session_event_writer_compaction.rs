//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-compaction.ts`.
//!
//! Record-compaction primitives of the shared stdio lane.

use serde_json::{Map, Value};

pub type RpcRecord = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactDeltaType {
    TextDelta,
    ThinkingDelta,
    ToolcallDelta,
}

impl CompactDeltaType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextDelta => "text_delta",
            Self::ThinkingDelta => "thinking_delta",
            Self::ToolcallDelta => "toolcall_delta",
        }
    }
}

pub const MESSAGE_KEY: &str = "message";

#[derive(Debug, Clone, PartialEq)]
pub struct CompactDelta<'a> {
    pub kind: CompactDeltaType,
    pub content_index: f64,
    pub delta: &'a str,
}

pub fn compact_delta(value: &RpcRecord) -> Option<CompactDelta<'_>> {
    if value.get("type").and_then(Value::as_str) != Some("message_update")
        || !value.contains_key("message")
    {
        return None;
    }
    let event = value.get("assistantMessageEvent")?.as_object()?;
    let kind = match event.get("type").and_then(Value::as_str)? {
        "text_delta" => CompactDeltaType::TextDelta,
        "thinking_delta" => CompactDeltaType::ThinkingDelta,
        "toolcall_delta" => CompactDeltaType::ToolcallDelta,
        _ => return None,
    };
    let content_index = event.get("contentIndex")?.as_f64()?;
    let delta = event.get("delta")?.as_str()?;
    Some(CompactDelta {
        kind,
        content_index,
        delta,
    })
}

pub fn demote_to_delta_only(value: &RpcRecord) -> RpcRecord {
    let mut out = value.clone();
    out.insert("message".to_owned(), Value::Null);
    if let Some(Value::Object(mut event)) = out.get("assistantMessageEvent").cloned() {
        event.insert("partial".to_owned(), Value::Null);
        out.insert("assistantMessageEvent".to_owned(), Value::Object(event));
    }
    out
}

pub fn tool_update_key(value: &RpcRecord) -> Option<String> {
    if value.get("type").and_then(Value::as_str) == Some("tool_execution_update")
        && let Some(id) = value.get("toolCallId").and_then(Value::as_str)
    {
        return Some(format!("tool:{id}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_compact_delta_and_demotes() {
        let rec = json!({
            "type": "message_update",
            "message": {"role": "assistant"},
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "hi", "partial": {"x": 1}}
        });
        let map = rec.as_object().expect("obj");
        let delta = compact_delta(map).expect("delta");
        assert_eq!(delta.kind, CompactDeltaType::TextDelta);
        assert_eq!(delta.delta, "hi");
        let demoted = demote_to_delta_only(map);
        assert_eq!(demoted["message"], Value::Null);
        assert_eq!(demoted["assistantMessageEvent"]["partial"], Value::Null);
    }
}
