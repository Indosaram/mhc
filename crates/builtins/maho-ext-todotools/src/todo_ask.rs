// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi (`todotools/todo-ask.ts`).
use crate::todo_types::{TODO_STATE_ENTRY_TYPE, TodoAsk};
use serde_json::Value;

const TODO_RESTORE_REQUEST_TYPE: &str = "compaction.todo-restore-request";

pub const ASK_TEXT_LIMIT: usize = 200;

/// senpi `firstTextBlock`: a string content, or the first text block of an array content.
pub fn first_text_block(content: &Value) -> Option<&str> {
    if let Some(text) = content.as_str() {
        return Some(text);
    }
    content.as_array()?.iter().find_map(|block| {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| block.get("text").and_then(Value::as_str))
            .flatten()
    })
}

/// senpi `truncateAskText`: cut at [`ASK_TEXT_LIMIT`] code points with `… (+N chars)`.
pub fn truncate_ask_text(text: &str) -> String {
    let count = text.chars().count();
    if count <= ASK_TEXT_LIMIT {
        return text.to_owned();
    }
    let kept: String = text.chars().take(ASK_TEXT_LIMIT).collect();
    format!("{kept}… (+{} chars)", count - ASK_TEXT_LIMIT)
}

/// senpi `isTodoAsk`: string `entryId` and `text`, numeric `capturedAt`.
fn todo_ask_of(value: &Value) -> Option<TodoAsk> {
    let entry_id = value.get("entryId")?.as_str()?;
    let text = value.get("text")?.as_str()?;
    let captured_at = value.get("capturedAt")?.as_f64()?;
    Some(TodoAsk {
        entry_id: entry_id.to_owned(),
        text: text.to_owned(),
        // JS numbers; a TodoAsk stores integral milliseconds.
        captured_at: captured_at.max(0.0) as u64,
    })
}

/// The ask a newly created list anchors to. The first list of a branch anchors to the
/// session's first user request; a later list-creating call is an explicit redirect and
/// anchors to the newest user request. Ask-user answer frames are replies, not requests.
/// A compaction restore request newer than both the last list and the last user request
/// re-emits the ask its snapshot carried; the model never supplies an ask itself.
pub fn capture_list_ask(entries: &[Value], captured_at: u64) -> Option<TodoAsk> {
    let mut last_state_index: Option<usize> = None;
    let mut restore: Option<(usize, TodoAsk)> = None;
    let mut requests: Vec<(usize, String, String)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let kind = entry.get("type").and_then(Value::as_str);
        let custom_type = entry.get("customType").and_then(Value::as_str);
        if kind == Some("custom") && custom_type == Some(TODO_STATE_ENTRY_TYPE) {
            last_state_index = Some(index);
            continue;
        }
        if kind == Some("custom_message") && custom_type == Some(TODO_RESTORE_REQUEST_TYPE) {
            // A later invalid restore request leaves the earlier valid one in place.
            if let Some(ask) = entry.pointer("/details/ask").and_then(todo_ask_of) {
                restore = Some((index, ask));
            }
            continue;
        }
        if kind != Some("message")
            || entry.pointer("/message/role").and_then(Value::as_str) != Some("user")
        {
            continue;
        }
        let Some(raw) = entry.pointer("/message/content").and_then(first_text_block) else {
            continue;
        };
        if crate::todo_storage::is_ask_user_answer_frame(raw) {
            continue;
        }
        let text = crate::todo_format::sanitize_todo_text(raw);
        if !text.is_empty() {
            let entry_id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            requests.push((index, entry_id, text));
        }
    }
    let latest = requests.last();
    if let Some((index, ask)) = restore
        && last_state_index.is_none_or(|state| index > state)
        && latest.is_none_or(|request| index > request.0)
    {
        return Some(ask);
    }
    let chosen = if last_state_index.is_none() {
        requests.first()
    } else {
        latest
    }?;
    Some(TodoAsk {
        entry_id: chosen.1.clone(),
        text: truncate_ask_text(&chosen.2),
        captured_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(id: &str, content: Value) -> Value {
        json!({"type": "message", "id": id, "message": {"role": "user", "content": content}})
    }
    fn state() -> Value {
        json!({"type": "custom", "customType": TODO_STATE_ENTRY_TYPE, "data": {"schema": "v2", "phases": []}})
    }
    fn restore(ask: Value) -> Value {
        json!({"type": "custom_message", "customType": TODO_RESTORE_REQUEST_TYPE, "details": {"ask": ask}})
    }

    #[test]
    fn first_text_block_reads_strings_and_the_first_text_part() {
        assert_eq!(first_text_block(&json!("plain")), Some("plain"));
        assert_eq!(
            first_text_block(
                &json!([{"type": "image"}, {"type": "text", "text": "first"}, {"type": "text", "text": "second"}])
            ),
            Some("first")
        );
        assert_eq!(
            first_text_block(&json!([{"type": "text", "text": 1}])),
            None
        );
        assert_eq!(first_text_block(&json!({"type": "text"})), None);
    }

    #[test]
    fn truncation_counts_code_points() {
        let exact = "😀".repeat(ASK_TEXT_LIMIT);
        assert_eq!(truncate_ask_text(&exact), exact);
        let long = "a".repeat(ASK_TEXT_LIMIT + 5);
        assert_eq!(
            truncate_ask_text(&long),
            format!("{}… (+5 chars)", "a".repeat(ASK_TEXT_LIMIT))
        );
    }

    #[test]
    fn first_list_anchors_to_first_request_and_later_lists_to_the_newest() {
        let entries = [
            user("m1", json!("First request")),
            state(),
            user("m2", json!("Redirect")),
        ];
        assert_eq!(
            capture_list_ask(&entries[..1], 7),
            Some(TodoAsk {
                entry_id: "m1".into(),
                text: "First request".into(),
                captured_at: 7
            })
        );
        assert_eq!(
            capture_list_ask(&entries, 7).map(|ask| ask.entry_id),
            Some("m2".into())
        );
        assert_eq!(capture_list_ask(&[], 7), None);
    }

    #[test]
    fn answer_frames_are_skipped_and_a_newer_restore_request_wins() {
        let ask = json!({"entryId": "old", "text": "Original", "capturedAt": 1});
        let entries = [
            user("m1", json!("[Answer to question q1]\nnot a request")),
            restore(ask),
        ];
        assert_eq!(
            capture_list_ask(&entries, 9).map(|ask| (ask.text, ask.captured_at)),
            Some(("Original".into(), 1))
        );
        let stale = [
            restore(json!({"entryId": "old", "text": "Original", "capturedAt": 1})),
            user("m2", json!("Newer")),
        ];
        assert_eq!(
            capture_list_ask(&stale, 9).map(|ask| ask.text),
            Some("Newer".into())
        );
    }

    #[test]
    fn invalid_later_restore_request_keeps_the_earlier_valid_one() {
        let entries = [
            restore(json!({"entryId": "old", "text": "Original", "capturedAt": 1})),
            restore(json!({"text": "missing identity"})),
        ];
        assert_eq!(
            capture_list_ask(&entries, 9).map(|ask| ask.entry_id),
            Some("old".into())
        );
    }
}
