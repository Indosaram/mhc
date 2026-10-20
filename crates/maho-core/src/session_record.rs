//! Port of senpi packages/coding-agent/src/core/session-record.ts.

use serde_json::Value;

#[derive(Default)]
pub(crate) struct NestedCallRecorder {
    calls: Vec<maho_ai::types::NestedToolCallRecord>,
    argument_bytes: usize,
    incomplete: bool,
    pub(crate) usage: Option<maho_ai::types::Usage>,
}

impl NestedCallRecorder {
    pub(crate) fn start(&mut self, id: &str, name: &str, arguments: &Value) -> Option<usize> {
        if self.calls.len() >= 256 {
            self.incomplete = true;
            return None;
        }
        let bytes = arguments.to_string().len();
        let retained = bytes <= 8 * 1024 && self.argument_bytes.saturating_add(bytes) <= 32 * 1024;
        if retained { self.argument_bytes += bytes; } else { self.incomplete = true; }
        let index = self.calls.len();
        self.calls.push(maho_ai::types::NestedToolCallRecord {
            id: id.to_owned(), name: name.to_owned(),
            arguments: retained.then(|| arguments.as_object().cloned().unwrap_or_default()),
            arguments_bytes: (!retained).then_some(bytes),
            status: maho_ai::types::NestedToolCallStatus::Unfinished,
            duration_ms: None, error: None,
        });
        Some(index)
    }

    pub(crate) fn finish(&mut self, index: Option<usize>, elapsed: std::time::Duration, result: &maho_agent::types::AgentToolResult) {
        if let Some(usage) = &result.usage {
            self.usage = Some(match &self.usage {
                Some(previous) => crate::compaction::combine_usage(previous, usage),
                None => *usage,
            });
        }
        if let Some(index) = index {
            let record = &mut self.calls[index];
            let failed = result.is_error == Some(true);
            record.status = if failed { maho_ai::types::NestedToolCallStatus::Error } else { maho_ai::types::NestedToolCallStatus::Ok };
            record.duration_ms = Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
            if failed {
                let text = maho_ai::utils::text::content_text(&result.content, "\n");
                if !text.is_empty() { record.error = Some(text.chars().take(500).collect()); }
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Option<maho_ai::types::NestedToolCalls> {
        if self.calls.is_empty() && !self.incomplete { return None; }
        Some(maho_ai::types::NestedToolCalls {
            calls: self.calls.clone(), complete: !self.incomplete && self.calls.iter()
                .all(|call| call.status != maho_ai::types::NestedToolCallStatus::Unfinished),
        })
    }
}

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
    fn nested_arguments_total_budget_omits_only_over_budget_payloads() {
        // Given
        let mut recorder = NestedCallRecorder::default();
        let arguments = json!({"data": "x".repeat(7000)});
        let result = maho_agent::types::AgentToolResult::text("ok");
        // When
        for index in 0..5 {
            let record = recorder.start(&format!("root/{index}"), "read", &arguments);
            recorder.finish(record, std::time::Duration::ZERO, &result);
        }
        // Then
        let snapshot = recorder.snapshot().expect("nested calls");
        assert_eq!(snapshot.calls.len(), 5);
        assert!(snapshot.calls[..4].iter().all(|call| call.arguments.is_some()));
        assert!(snapshot.calls[4].arguments.is_none());
        assert!(!snapshot.complete);
    }

    #[test]
    fn nested_recorder_bounds_arguments_calls_and_errors_without_fabrication() {
        // Given
        let mut recorder = NestedCallRecorder::default();
        assert!(recorder.snapshot().is_none());
        let args = json!({"value": "x".repeat(8192)});
        let first = recorder.start("parent/1", "read", &args);
        let mut result = maho_agent::types::AgentToolResult::text("e".repeat(600));
        result.is_error = Some(true);
        // When
        recorder.finish(first, std::time::Duration::from_millis(23), &result);
        for index in 1..257 { recorder.start(&format!("parent/{index}"), "read", &json!({})); }
        // Then
        let snapshot = recorder.snapshot().expect("calls were made");
        assert!(!snapshot.complete);
        assert_eq!(snapshot.calls.len(), 256);
        let first = &snapshot.calls[0];
        assert!(first.arguments.is_none());
        assert_eq!(first.arguments_bytes, Some(args.to_string().len()));
        assert_eq!(first.duration_ms, Some(23));
        assert_eq!(first.error.as_ref().expect("bounded error").len(), 500);
        assert_eq!(first.status, maho_ai::types::NestedToolCallStatus::Error);
        assert_eq!(snapshot.calls[1].status, maho_ai::types::NestedToolCallStatus::Unfinished);
    }

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
