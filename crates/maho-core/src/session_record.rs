//! Port of senpi packages/coding-agent/src/core/session-record.ts.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleMessage {
    pub text: String,
    pub role: String,
    pub time: Option<i64>,
}

/// Deserializes one JSONL line. Malformed lines are skipped, matching full-file session loading.
pub fn parse_entry_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

fn extract_text_content(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The record's visible message, or None when it is not a user/assistant message.
pub fn visible_message(entry: &Value) -> Option<VisibleMessage> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.get("message")?;
    let role = message.get("role").and_then(Value::as_str)?;
    if role != "user" && role != "assistant" {
        return None;
    }
    message.get("content")?;
    let message_time = message.get("timestamp").and_then(Value::as_i64);
    let entry_time = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .map(|parsed| parsed.timestamp_millis());
    let time = message_time.or(entry_time);
    Some(VisibleMessage { text: extract_text_content(message), role: role.to_owned(), time })
}

/// The record's display name, or None when the record is not a session_info entry.
pub fn session_info_name(entry: &Value) -> Option<Option<String>> {
    if entry.get("type").and_then(Value::as_str) != Some("session_info") {
        return None;
    }
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_and_blank_lines_are_skipped() {
        assert!(parse_entry_line("").is_none());
        assert!(parse_entry_line("   ").is_none());
        assert!(parse_entry_line("not json").is_none());
        assert_eq!(parse_entry_line(r#"{"type":"message"}"#).and_then(|v| v.get("type").cloned()), Some(json!("message")));
    }

    #[test]
    fn a_user_message_is_visible() {
        let entry = json!({ "type": "message", "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }] } });
        let visible = visible_message(&entry).expect("visible");
        assert_eq!(visible.role, "user");
        assert_eq!(visible.text, "hi");
    }

    #[test]
    fn a_tool_result_is_not_visible() {
        let entry = json!({ "type": "message", "message": { "role": "toolResult", "content": [] } });
        assert!(visible_message(&entry).is_none());
    }

    #[test]
    fn a_non_message_entry_is_not_visible() {
        assert!(visible_message(&json!({ "type": "custom" })).is_none());
    }

    #[test]
    fn the_message_timestamp_wins_over_the_entry_timestamp() {
        let entry = json!({ "type": "message", "timestamp": "2020-01-01T00:00:00.000Z",
            "message": { "role": "assistant", "content": "x", "timestamp": 42 } });
        assert_eq!(visible_message(&entry).expect("visible").time, Some(42));
    }

    #[test]
    fn session_info_name_returns_none_for_other_entries() {
        assert!(session_info_name(&json!({ "type": "message" })).is_none());
        assert_eq!(session_info_name(&json!({ "type": "session_info", "name": "  " })), Some(None));
        assert_eq!(session_info_name(&json!({ "type": "session_info", "name": "named" })), Some(Some("named".into())));
    }
}
