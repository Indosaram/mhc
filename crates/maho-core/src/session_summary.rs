//! Port of senpi packages/coding-agent/src/core/session-summary.ts.

use std::io::BufRead;

use serde_json::Value;

use crate::session_record::{parse_entry_line, session_info_name, visible_message};

/// Everything a picker row needs from one session file, derived from every record in the file.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub header: Value,
    pub name: Option<String>,
    pub first_user_message: String,
    pub message_count: usize,
    pub last_activity_time: Option<i64>,
    pub all_messages_text: String,
}

#[derive(Default)]
struct SummaryAccumulator {
    header: Option<Value>,
    name: Option<String>,
    first_user_message: String,
    message_count: usize,
    last_activity_time: Option<i64>,
    texts: Vec<String>,
}

/// Folds one JSONL line into the accumulator. Returns false when the first parsable record is not
/// a session header, which means the file is not a session and the caller stops reading.
fn accumulate_line(accumulator: &mut SummaryAccumulator, line: &str) -> bool {
    let Some(entry) = parse_entry_line(line) else { return true };

    if accumulator.header.is_none() {
        if entry.get("type").and_then(Value::as_str) != Some("session") || entry.get("id").and_then(Value::as_str).is_none() {
            return false;
        }
        accumulator.header = Some(entry);
        return true;
    }

    if let Some(info_name) = session_info_name(&entry) {
        accumulator.name = info_name;
        return true;
    }

    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return true;
    }
    accumulator.message_count += 1;

    let Some(visible) = visible_message(&entry) else { return true };
    if let Some(time) = visible.time {
        accumulator.last_activity_time = Some(accumulator.last_activity_time.unwrap_or(0).max(time));
    }
    if visible.text.is_empty() {
        return true;
    }
    if accumulator.first_user_message.is_empty() && visible.role == "user" {
        accumulator.first_user_message = visible.text.clone();
    }
    accumulator.texts.push(visible.text);
    true
}

/// Streams one session file line by line and folds it into an exact summary. A file whose first
/// record is not a session header, or that cannot be read, yields None.
pub fn read_session_summary(file_path: &str) -> Option<SessionSummary> {
    let file = std::fs::File::open(file_path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut accumulator = SummaryAccumulator::default();
    for line in reader.lines() {
        let line = line.ok()?;
        if !accumulate_line(&mut accumulator, &line) {
            return None;
        }
    }
    let header = accumulator.header?;
    Some(SessionSummary {
        header,
        name: accumulator.name,
        first_user_message: accumulator.first_user_message,
        message_count: accumulator.message_count,
        last_activity_time: accumulator.last_activity_time,
        all_messages_text: accumulator.texts.join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &std::path::Path, content: &str) -> String {
        let path = dir.join("session.jsonl");
        std::fs::write(&path, content).expect("write");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn summarizes_a_session_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let content = [
            json!({ "type": "session", "version": 3, "id": "s1", "timestamp": "2020-01-01T00:00:00.000Z", "cwd": "/w" }).to_string(),
            json!({ "type": "message", "message": { "role": "user", "content": [{ "type": "text", "text": "hello" }], "timestamp": 10 } }).to_string(),
            json!({ "type": "message", "message": { "role": "assistant", "content": "world", "timestamp": 20 } }).to_string(),
            json!({ "type": "session_info", "name": "named" }).to_string(),
        ]
        .join("\n");
        let summary = read_session_summary(&write(tmp.path(), &content)).expect("summary");
        assert_eq!(summary.name.as_deref(), Some("named"));
        assert_eq!(summary.first_user_message, "hello");
        assert_eq!(summary.message_count, 2);
        assert_eq!(summary.last_activity_time, Some(20));
        assert_eq!(summary.all_messages_text, "hello world");
    }

    #[test]
    fn a_file_that_does_not_start_with_a_session_header_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let content = json!({ "type": "message", "message": { "role": "user", "content": "x" } }).to_string();
        assert!(read_session_summary(&write(tmp.path(), &content)).is_none());
    }

    #[test]
    fn a_missing_file_is_none() {
        assert!(read_session_summary("/definitely/not/here.jsonl").is_none());
    }

    #[test]
    fn the_first_user_message_wins_even_out_of_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let content = [
            json!({ "type": "session", "id": "s1" }).to_string(),
            json!({ "type": "message", "message": { "role": "assistant", "content": "first", "timestamp": 5 } }).to_string(),
            json!({ "type": "message", "message": { "role": "user", "content": "second", "timestamp": 1 } }).to_string(),
        ]
        .join("\n");
        let summary = read_session_summary(&write(tmp.path(), &content)).expect("summary");
        assert_eq!(summary.first_user_message, "second");
        assert_eq!(summary.last_activity_time, Some(5));
    }
}
