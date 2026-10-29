//! The sidecar's `session_entries` tool (latest `kibitzer/tools/session-read.ts`).
//!
//! `session_entries_since` pages a bound parent session's RAW branch entries (oldest first) after a
//! cursor: the memory-owned hidden channels and Kibitzer's own audit records are omitted and counted,
//! each returned row is bounded to the per-entry character cap, and the page reports the continuation
//! cursor and whether the page cap truncated the scan.
//!
//! The upstream file's two host-bound halves are not part of this unit: `createSessionBranchSnapshot`
//! reads a live host ctx and `createKibitzerSessionEntriesTool` is a `ToolDefinition` over a wake
//! budget. The CLI host owns both (`crates/maho-cli/src/cli/kibitzer_tools.rs` builds the tool and the
//! entries resolver), so only the pure scan is ported here.

use serde_json::{Value, json};

use crate::kibitzer_notice::{GATE_ENTRY_TYPE, NUDGED_ENTRY_TYPE, UNAVAILABLE_ENTRY_TYPE};
use crate::kibitzer_tools_caps::KibitzerToolCaps;
use crate::kibitzer_tools_result::bounded_text;
use crate::recall_session_read::{EXCLUDED_CUSTOM_TYPES, text_of};

/// The registered tool name (`session_entries`).
pub const KIBITZER_SESSION_ENTRIES_TOOL_NAME: &str = "session_entries";

/// Custom types the sidecar never sees: the memory-owned hidden channels (a previous hint is not
/// conversation) plus Kibitzer's own audit records, so the sidecar cannot read its own verdicts back.
pub const HIDDEN_SESSION_CUSTOM_TYPES: [&str; 6] = [
    EXCLUDED_CUSTOM_TYPES[0],
    EXCLUDED_CUSTOM_TYPES[1],
    EXCLUDED_CUSTOM_TYPES[2],
    NUDGED_ENTRY_TYPE,
    GATE_ENTRY_TYPE,
    UNAVAILABLE_ENTRY_TYPE,
];

/// Entries strictly after `since`, hidden types omitted, bounded to `caps.session_entries` rows.
///
/// Returns the `SessionEntriesPage` shape (`{ entries, next_since, hidden, truncated }`): `entries`
/// is the bounded page oldest first, `next_since` is the last returned cursor (the input `since` when
/// nothing new was returned), `hidden` counts the omitted hidden entries, and `truncated` is set when
/// the page cap stopped the scan early.
pub fn session_entries_since(entries: &[Value], since: i64, caps: KibitzerToolCaps) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut hidden: i64 = 0;
    let mut next_since = since;
    let mut truncated = false;
    // `Math.max(0, Math.floor(since) + 1)`: the scan starts at the entry after the cursor.
    let start = since.saturating_add(1).max(0) as usize;
    for (index, entry) in entries.iter().enumerate().skip(start) {
        if !entry.is_object() || entry.get("type").and_then(Value::as_str).is_none() {
            continue;
        }
        if is_hidden(entry) {
            hidden += 1;
            continue;
        }
        if rows.len() >= caps.session_entries {
            truncated = true;
            break;
        }
        rows.push(render_row(index, entry, caps.session_entry_chars));
        next_since = index as i64;
    }
    json!({ "entries": rows, "next_since": next_since, "hidden": hidden, "truncated": truncated })
}

/// A hidden entry is a `custom`/`custom_message` whose own `customType` is hidden, or a `message`
/// whose nested `message.customType` is hidden.
fn is_hidden(entry: &Value) -> bool {
    let entry_type = entry.get("type").and_then(Value::as_str);
    if entry_type == Some("custom") || entry_type == Some("custom_message") {
        return entry
            .get("customType")
            .and_then(Value::as_str)
            .is_some_and(|custom_type| HIDDEN_SESSION_CUSTOM_TYPES.contains(&custom_type));
    }
    if entry_type == Some("message")
        && let Some(message) = entry.get("message").filter(|message| message.is_object())
    {
        return message
            .get("customType")
            .and_then(Value::as_str)
            .is_some_and(|custom_type| HIDDEN_SESSION_CUSTOM_TYPES.contains(&custom_type));
    }
    false
}

/// One row: `message` rows carry the optional `role` and the summarized, bounded text;
/// `custom_message` rows carry the bounded top-level content; every other type is `{cursor, type}`.
fn render_row(cursor: usize, entry: &Value, cap: usize) -> Value {
    let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or_default();
    if entry_type == "message"
        && let Some(message) = entry.get("message").filter(|message| message.is_object())
    {
        let role = message.get("role").and_then(Value::as_str);
        let text = bounded_text(&message_text(message), cap);
        let mut row = json!({ "cursor": cursor, "type": entry_type, "text": text });
        if let Some(role) = role {
            row["role"] = Value::from(role);
        }
        return row;
    }
    if entry_type == "custom_message" {
        let text = bounded_text(&text_of(entry.get("content")), cap);
        return json!({ "cursor": cursor, "type": entry_type, "text": text });
    }
    json!({ "cursor": cursor, "type": entry_type })
}

/// The text of a message: its content text, then one `[tool <name>] <compact json>` line per
/// `toolCall` block, with a `[result <toolName>]` line prepended for a `toolResult` message.
fn message_text(message: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let content = message.get("content");
    let text = text_of(content);
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(Value::Array(blocks)) = content {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
            // `JSON.stringify(value) ?? ""`: a missing `arguments` renders as the empty string.
            let arguments = match block.get("arguments") {
                Some(value) => serde_json::to_string(value).unwrap_or_default(),
                None => String::new(),
            };
            parts.push(format!("[tool {name}] {arguments}"));
        }
    }
    if message.get("role").and_then(Value::as_str) == Some("toolResult")
        && let Some(tool_name) = message.get("toolName").and_then(Value::as_str)
    {
        parts.insert(0, format!("[result {tool_name}]"));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    use crate::kibitzer_tools_caps::DEFAULT_KIBITZER_TOOL_CAPS;
    use crate::prompt::MEMORY_NOTICE_CUSTOM_TYPE;
    use crate::recall_session_read::RECALL_CUSTOM_TYPE;

    /// The two `session_entries` caps overridden; every other cap stays at its default.
    fn caps(session_entries: usize, session_entry_chars: usize) -> KibitzerToolCaps {
        KibitzerToolCaps { session_entries, session_entry_chars, ..DEFAULT_KIBITZER_TOOL_CAPS }
    }

    /// Upstream `test-support.ts::message`: content is one text block.
    fn message(role: &str, text: &str, id: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "message": { "role": role, "content": [{ "type": "text", "text": text }] },
        })
    }

    /// Upstream `message(.., customType)`: the hidden channel sits on the nested `message`.
    fn message_with_custom_type(role: &str, text: &str, id: &str, custom_type: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "message": {
                "role": role,
                "content": [{ "type": "text", "text": text }],
                "customType": custom_type,
            },
        })
    }

    /// Upstream `test-support.ts::customEntry`.
    fn custom_entry(custom_type: &str, id: &str, data: Value) -> Value {
        json!({ "type": "custom", "id": id, "customType": custom_type, "data": data })
    }

    /// Upstream `test-support.ts::customMessage`.
    fn custom_message(custom_type: &str, id: &str, content: &str) -> Value {
        json!({ "type": "custom_message", "id": id, "customType": custom_type, "content": content })
    }

    fn page_rows(page: &Value) -> Vec<Value> {
        page.get("entries").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    fn row_texts(page: &Value) -> Vec<String> {
        page_rows(page)
            .iter()
            .map(|row| row.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
            .collect()
    }

    fn row_cursors(page: &Value) -> Vec<i64> {
        page_rows(page)
            .iter()
            .map(|row| row.get("cursor").and_then(Value::as_i64).unwrap_or(-1))
            .collect()
    }

    fn hidden_count(page: &Value) -> i64 {
        page.get("hidden").and_then(Value::as_i64).unwrap_or(-1)
    }

    fn next_since(page: &Value) -> i64 {
        page.get("next_since").and_then(Value::as_i64).unwrap_or(-1)
    }

    fn is_truncated(page: &Value) -> bool {
        page.get("truncated").and_then(Value::as_bool).unwrap_or(false)
    }

    // ---- five upstream pure paging cases (`session-read.test.ts` :: `sessionEntriesSince`) ----

    #[test]
    fn given_hidden_memory_channels_when_listing_then_every_hidden_type_is_omitted_and_counted() {
        let entries = vec![
            message("user", "visible user", "e0"),
            message_with_custom_type("assistant", "recall hint", "e1", RECALL_CUSTOM_TYPE),
            message_with_custom_type("assistant", "legacy hint", "e2", "omo-memorian:recall"),
            message_with_custom_type("assistant", "notice", "e3", MEMORY_NOTICE_CUSTOM_TYPE),
            custom_entry(NUDGED_ENTRY_TYPE, "e4", json!({ "version": 1, "nudges": [{ "path": "a.md", "hint": "h" }] })),
            custom_entry(GATE_ENTRY_TYPE, "e5", json!({ "version": 1, "status": "skipped", "candidateCount": 0 })),
            custom_message(UNAVAILABLE_ENTRY_TYPE, "e6", "injected"),
            message("assistant", "visible assistant", "e7"),
        ];
        assert_eq!(HIDDEN_SESSION_CUSTOM_TYPES.len(), 6);
        for hidden_type in [
            RECALL_CUSTOM_TYPE,
            "omo-memorian:recall",
            MEMORY_NOTICE_CUSTOM_TYPE,
            NUDGED_ENTRY_TYPE,
            GATE_ENTRY_TYPE,
            UNAVAILABLE_ENTRY_TYPE,
        ] {
            assert!(HIDDEN_SESSION_CUSTOM_TYPES.contains(&hidden_type), "{hidden_type} must be hidden");
        }

        let page = session_entries_since(&entries, -1, caps(30, 600));
        assert_eq!(row_texts(&page), vec!["visible user".to_string(), "visible assistant".to_string()]);
        assert_eq!(row_cursors(&page), vec![0, 7]);
        assert_eq!(hidden_count(&page), 6);
        assert_eq!(next_since(&page), 7);
        assert!(!is_truncated(&page));
    }

    #[test]
    fn given_a_since_cursor_when_listing_then_only_entries_after_the_cursor_are_returned() {
        let entries = vec![message("user", "a", "e0"), message("assistant", "b", "e1"), message("user", "c", "e2")];
        let page = session_entries_since(&entries, 1, caps(30, 600));
        assert_eq!(row_texts(&page), vec!["c".to_string()]);
        assert_eq!(next_since(&page), 2);

        let past_end = session_entries_since(&entries, 2, caps(30, 600));
        assert!(page_rows(&past_end).is_empty());
        assert_eq!(next_since(&past_end), 2);
        assert!(!is_truncated(&past_end));
    }

    #[test]
    fn given_more_entries_than_the_page_cap_when_listing_then_the_page_is_bounded_and_reports_the_continuation_cursor() {
        let entries: Vec<Value> =
            (0..10).map(|index| message("user", &format!("m{index}"), &format!("e{index}"))).collect();
        let page = session_entries_since(&entries, -1, caps(4, 600));
        assert_eq!(page_rows(&page).len(), 4);
        assert_eq!(next_since(&page), 3);
        assert!(is_truncated(&page));
    }

    #[test]
    fn given_a_long_entry_with_a_secret_when_listing_then_the_text_is_redacted_before_it_is_capped() {
        // Upstream redacts secret-like material FIRST, so a secret inside the kept head cannot survive.
        let secret = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let entries = vec![message("user", &format!("{secret} {}", "y".repeat(2000)), "e0")];
        let page = session_entries_since(&entries, -1, caps(30, 100));
        let rows = page_rows(&page);
        let text = rows.first().and_then(|row| row.get("text")).and_then(Value::as_str).unwrap_or_default();
        assert!(!text.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"));
        assert!(text.encode_utf16().count() <= 100 + 64);
        assert!(text.contains("[truncated"));
    }

    #[test]
    fn given_assistant_tool_calls_and_tool_results_when_listing_then_they_are_summarized_as_bounded_text() {
        let entries = vec![
            json!({
                "type": "message",
                "id": "e0",
                "message": {
                    "role": "assistant",
                    "content": [{ "type": "toolCall", "id": "c1", "name": "bash", "arguments": { "command": "ls" } }],
                },
            }),
            json!({
                "type": "message",
                "id": "e1",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "c1",
                    "toolName": "bash",
                    "content": [{ "type": "text", "text": "a.txt" }],
                },
            }),
        ];
        let page = session_entries_since(&entries, -1, caps(30, 600));
        let rows = page_rows(&page);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("role").and_then(Value::as_str), Some("assistant"));
        let first = rows[0].get("text").and_then(Value::as_str).unwrap_or_default();
        assert!(first.contains("bash"));
        assert!(first.contains("ls"));
        assert_eq!(rows[1].get("role").and_then(Value::as_str), Some("toolResult"));
        assert!(rows[1].get("text").and_then(Value::as_str).unwrap_or_default().contains("a.txt"));
    }

    // ---- boundary cases (queue invocation contract) ----

    #[test]
    fn given_exactly_the_page_cap_when_listing_then_the_page_is_not_truncated() {
        let entries: Vec<Value> =
            (0..3).map(|index| message("user", &format!("m{index}"), &format!("e{index}"))).collect();
        let exact = session_entries_since(&entries, -1, caps(3, 600));
        assert_eq!(page_rows(&exact).len(), 3);
        assert_eq!(next_since(&exact), 2);
        assert!(!is_truncated(&exact));

        let overflow = session_entries_since(&entries, -1, caps(2, 600));
        assert_eq!(page_rows(&overflow).len(), 2);
        assert_eq!(next_since(&overflow), 1);
        assert!(is_truncated(&overflow));
    }

    #[test]
    fn given_a_filled_page_when_hidden_rows_precede_the_next_visible_row_then_they_are_still_counted() {
        let entries = vec![
            message("user", "v0", "e0"),
            custom_entry(NUDGED_ENTRY_TYPE, "e1", json!({ "version": 1, "nudges": [] })),
            custom_entry(GATE_ENTRY_TYPE, "e2", json!({ "version": 1, "status": "skipped", "candidateCount": 0 })),
            message("assistant", "v3", "e3"),
            custom_entry(UNAVAILABLE_ENTRY_TYPE, "e4", json!({ "version": 1, "category": "c", "cause": "category_unavailable" })),
        ];
        let page = session_entries_since(&entries, -1, caps(1, 600));
        assert_eq!(page_rows(&page).len(), 1);
        assert_eq!(next_since(&page), 0);
        assert_eq!(hidden_count(&page), 2);
        assert!(is_truncated(&page));
    }

    #[test]
    fn given_a_zero_page_cap_when_listing_then_no_rows_return_and_the_input_cursor_is_preserved() {
        let entries = vec![
            custom_entry(NUDGED_ENTRY_TYPE, "e0", json!({ "version": 1, "nudges": [] })),
            message("user", "visible", "e1"),
        ];
        let page = session_entries_since(&entries, -1, caps(0, 600));
        assert!(page_rows(&page).is_empty());
        assert_eq!(next_since(&page), -1);
        assert_eq!(hidden_count(&page), 1);
        assert!(is_truncated(&page));
    }

    #[test]
    fn given_malformed_entries_when_listing_then_they_are_skipped_without_counting_as_hidden() {
        let entries = vec![
            Value::Null,
            json!(42),
            json!("str"),
            json!([]),
            json!({}),
            json!({ "type": 7 }),
            json!({ "type": "message" }),
            json!({ "type": "message", "message": "nope" }),
            message("user", "ok", "e8"),
        ];
        let page = session_entries_since(&entries, -1, caps(30, 600));
        let rows = page_rows(&page);
        assert_eq!(hidden_count(&page), 0);
        assert_eq!(row_cursors(&page), vec![6, 7, 8]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get("type").and_then(Value::as_str), Some("message"));
        assert!(rows[0].get("text").is_none());
        assert!(rows[0].get("role").is_none());
        assert_eq!(rows[2].get("text").and_then(Value::as_str), Some("ok"));
        assert_eq!(next_since(&page), 8);
    }

    #[test]
    fn given_a_visible_custom_message_when_listing_then_its_top_level_content_is_the_row_text() {
        let entries = vec![custom_message("omo-visible:thing", "e0", "hello")];
        let page = session_entries_since(&entries, -1, caps(30, 600));
        let rows = page_rows(&page);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("type").and_then(Value::as_str), Some("custom_message"));
        assert_eq!(rows[0].get("text").and_then(Value::as_str), Some("hello"));
        assert!(rows[0].get("role").is_none());
        assert_eq!(hidden_count(&page), 0);
    }

    #[test]
    fn given_a_message_with_a_non_string_role_when_listing_then_the_row_omits_role_but_keeps_text() {
        let entries = vec![json!({ "type": "message", "message": { "role": 5, "content": "hi" } })];
        let page = session_entries_since(&entries, -1, caps(30, 600));
        let rows = page_rows(&page);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].get("role").is_none());
        assert_eq!(rows[0].get("text").and_then(Value::as_str), Some("hi"));
    }

    #[test]
    fn given_a_tool_call_without_arguments_when_listing_then_its_arguments_render_empty() {
        let entries = vec![json!({
            "type": "message",
            "message": { "role": "assistant", "content": [{ "type": "toolCall", "id": "c1", "name": "bash" }] },
        })];
        let page = session_entries_since(&entries, -1, caps(30, 600));
        let rows = page_rows(&page);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("text").and_then(Value::as_str), Some("[tool bash] "));
    }
}
