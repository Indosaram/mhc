//! Session-shaped reads for the kibitzer recall channel (latest `recall-session-read.ts`).
//!
//! Memory-owned hidden channels are excluded so a previous hint cannot re-enter the query.

use serde_json::Value;

use crate::prompt::MEMORY_NOTICE_CUSTOM_TYPE;

/// Custom type of an injected recall block.
pub const RECALL_CUSTOM_TYPE: &str = "omo-kibitzer:recall";

/// Newest conversation texts feeding the query planner.
pub const RECALL_TEXT_WINDOW: usize = 6;

/// Memory-owned hidden channels, excluded from both the planner window and the judge transcript.
pub const EXCLUDED_CUSTOM_TYPES: [&str; 3] =
    [RECALL_CUSTOM_TYPE, "omo-memorian:recall", MEMORY_NOTICE_CUSTOM_TYPE];

/// The role of one judge-transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallRole {
    User,
    Assistant,
}

/// One line of the judge's transcript window: both roles, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallTranscriptTurn {
    pub role: RecallRole,
    pub text: String,
}

/// The ctx-derived half of a settle, captured while the context is still alive.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallSessionSnapshot {
    pub id: String,
    pub entries: Vec<Value>,
}

/// Builds a snapshot from the values the native session context exposes.
pub fn snapshot_session(id: &str, entries: &[Value]) -> Option<RecallSessionSnapshot> {
    if id.is_empty() {
        return None;
    }
    Some(RecallSessionSnapshot {
        id: id.to_string(),
        entries: entries.to_vec(),
    })
}

/// The judge's window, oldest first: both roles, memory-owned hidden channels excluded.
pub fn judge_transcript(entries: &[Value]) -> Vec<RecallTranscriptTurn> {
    let mut turns: Vec<RecallTranscriptTurn> = Vec::new();
    for entry in entries.iter().rev() {
        if turns.len() >= RECALL_TEXT_WINDOW {
            break;
        }
        if let Some(turn) = judge_turn(entry) {
            turns.push(turn);
        }
    }
    turns.reverse();
    turns
}

fn judge_turn(entry: &Value) -> Option<RecallTranscriptTurn> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.get("message")?;
    let role = match message.get("role").and_then(Value::as_str) {
        Some("user") => RecallRole::User,
        Some("assistant") => RecallRole::Assistant,
        _ => return None,
    };
    if let Some(custom_type) = message.get("customType").and_then(Value::as_str)
        && EXCLUDED_CUSTOM_TYPES.contains(&custom_type)
    {
        return None;
    }
    let text = text_of(message.get("content"));
    if text.trim().is_empty() {
        return None;
    }
    Some(RecallTranscriptTurn { role, text })
}

/// Newest-first USER texts for the planner; memory-owned hidden custom messages are skipped.
pub fn user_texts(entries: &[Value]) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();
    for entry in entries.iter().rev() {
        if texts.len() >= RECALL_TEXT_WINDOW {
            break;
        }
        if let Some(text) = user_text(entry) {
            texts.push(text);
        }
    }
    texts
}

fn user_text(entry: &Value) -> Option<String> {
    match entry.get("type").and_then(Value::as_str) {
        Some("custom_message") | Some("custom") => return None,
        Some("message") => {}
        _ => return None,
    }
    let message = entry.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if let Some(custom_type) = message.get("customType").and_then(Value::as_str)
        && EXCLUDED_CUSTOM_TYPES.contains(&custom_type)
    {
        return None;
    }
    let text = text_of(message.get("content"));
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

/// Text of a message content: a plain string, or the `text` blocks of an array joined by newlines.
pub fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                block.get("text").and_then(Value::as_str).map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_message(text: &str) -> Value {
        json!({ "type": "message", "message": { "role": "user", "content": text } })
    }

    fn assistant_message(text: &str) -> Value {
        json!({ "type": "message", "message": { "role": "assistant", "content": text } })
    }

    #[test]
    fn given_a_snapshot_when_entries_are_read_then_both_roles_are_kept_oldest_first() {
        let entries = vec![user_message("first"), assistant_message("second")];
        let turns = judge_transcript(&entries);
        assert_eq!(
            turns,
            vec![
                RecallTranscriptTurn { role: RecallRole::User, text: "first".into() },
                RecallTranscriptTurn { role: RecallRole::Assistant, text: "second".into() },
            ]
        );
    }

    #[test]
    fn given_hidden_and_non_message_entries_when_the_transcript_is_read_then_they_are_excluded() {
        let entries = vec![
            json!({ "type": "custom_message", "message": { "role": "user", "content": "hidden" } }),
            json!({ "type": "message", "message": { "role": "user", "content": "hint", "customType": RECALL_CUSTOM_TYPE } }),
            json!({ "type": "tool_result" }),
            user_message("live"),
        ];
        let turns = judge_transcript(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text, "live");
    }

    #[test]
    fn given_more_than_the_window_when_read_then_only_the_newest_window_is_kept() {
        let entries: Vec<Value> = (0..10).map(|index| user_message(&format!("m{index}"))).collect();
        let turns = judge_transcript(&entries);
        assert_eq!(turns.len(), RECALL_TEXT_WINDOW);
        assert_eq!(turns.first().map(|turn| turn.text.as_str()), Some("m4"));
        assert_eq!(turns.last().map(|turn| turn.text.as_str()), Some("m9"));
    }

    #[test]
    fn given_entries_when_user_texts_are_read_then_assistant_and_hidden_entries_are_skipped() {
        let entries = vec![
            user_message("oldest"),
            assistant_message("assistant"),
            json!({ "type": "custom", "message": { "role": "user", "content": "hidden" } }),
            user_message("newest"),
        ];
        assert_eq!(user_texts(&entries), vec!["newest".to_string(), "oldest".to_string()]);
    }

    #[test]
    fn given_array_content_when_text_is_read_then_text_blocks_join_with_newlines() {
        let content = json!([{ "type": "text", "text": "a" }, { "type": "image" }, { "type": "text", "text": "b" }]);
        assert_eq!(text_of(Some(&content)), "a\nb");
    }

    #[test]
    fn given_a_blank_id_when_a_snapshot_is_built_then_none_returns() {
        assert!(snapshot_session("", &[]).is_none());
        assert_eq!(
            snapshot_session("s1", &[]).map(|snapshot| snapshot.id),
            Some("s1".to_string())
        );
    }
}
