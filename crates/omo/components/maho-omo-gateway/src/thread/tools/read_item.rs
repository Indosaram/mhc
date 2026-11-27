//! Transcript read item projection (`thread/tools/read-item.ts`).

use serde_json::{Map, Value, json};

use super::super::contracts_results::ThreadTranscriptItem;
use super::super::reader::ThreadTranscriptEntry;

const ERROR_TEXT_MAX_CHARS: usize = 500;

pub fn transcript_role(role: Option<&str>) -> &'static str {
    match role {
        Some("user") => "user",
        Some("assistant") => "assistant",
        Some("toolResult") => "tool",
        _ => "system",
    }
}

fn content_of(message: &ThreadTranscriptEntry) -> String {
    let stop_reason = message.get("stopReason").and_then(Value::as_str);
    let error = message.get("errorMessage").and_then(Value::as_str);
    if stop_reason != Some("error") || error.is_none() {
        let val = message
            .get("content")
            .cloned()
            .unwrap_or_else(|| Value::Object(message.clone()));
        return serde_json::to_string(&val).unwrap_or_else(|_| "null".to_owned());
    }
    let error_msg = error.unwrap_or_default();
    let truncated: String = error_msg.chars().take(ERROR_TEXT_MAX_CHARS).collect();
    let mut parts = match message.get("content") {
        None => Vec::new(),
        Some(Value::Array(arr)) => arr.clone(),
        Some(other) => vec![other.clone()],
    };
    parts.push(json!({ "type": "error", "text": truncated }));
    serde_json::to_string(&Value::Array(parts)).unwrap_or_else(|_| "[]".to_owned())
}

pub fn read_item(message: &ThreadTranscriptEntry, index: usize) -> ThreadTranscriptItem {
    let role = message.get("role").and_then(Value::as_str);
    ThreadTranscriptItem {
        seq: (index + 1) as i64,
        role: transcript_role(role).to_owned(),
        content: content_of(message),
    }
}

pub fn read_item_from_value(message: &Value, index: usize) -> ThreadTranscriptItem {
    let obj: Map<String, Value> = message.as_object().cloned().unwrap_or_default();
    read_item(&obj, index)
}
