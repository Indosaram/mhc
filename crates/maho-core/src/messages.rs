//! Port of senpi packages/coding-agent/src/core/messages.ts.
//!
//! Session messages are unvalidated JSON on disk, so this module keeps the TS runtime's untyped
//! shape: messages are serde_json::Value objects and unknown fields survive every transformation.
//! The typed maho_ai::types::Message conversion happens at the provider boundary.

use serde_json::{json, Map, Value};

pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
pub const BRANCH_SUMMARY_PREFIX: &str = "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";
pub const GOAL_CONTINUATION_MESSAGE_TYPE: &str = "goal-continuation";

pub const TRANSPORT_IMAGE_BUDGET_BYTES: usize = 24 * 1024 * 1024;
pub const IMAGE_ELISION_PLACEHOLDER: &str = "[Image elided: an older image was removed to keep the request within the provider's size limit. Re-read the source file if you need to view it again.]";
pub const BLOCKED_IMAGE_PLACEHOLDER: &str = "Image reading is disabled.";

const BACKTICK: char = '\u{60}';

pub fn is_context_excluded_custom_message(_custom_type: &str) -> bool {
    false
}

pub fn filter_context_excluded_messages(messages: Vec<Value>) -> Vec<Value> {
    messages
}

fn backticks(count: usize) -> String {
    std::iter::repeat(BACKTICK).take(count).collect()
}

pub fn bash_execution_to_text(msg: &Value) -> String {
    let command = msg.get("command").and_then(Value::as_str).unwrap_or_default();
    let mut text = format!("Ran {t}{command}{t}\n", t = BACKTICK);
    let output = msg.get("output").and_then(Value::as_str).unwrap_or_default();
    if !output.is_empty() {
        text.push_str(&format!("{f}\n{output}\n{f}", f = backticks(3)));
    } else {
        text.push_str("(no output)");
    }
    if msg.get("cancelled").and_then(Value::as_bool) == Some(true) {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(exit_code) = msg.get("exitCode").and_then(Value::as_i64) {
        if exit_code != 0 {
            text.push_str(&format!("\n\nCommand exited with code {exit_code}"));
        }
    }
    if msg.get("truncated").and_then(Value::as_bool) == Some(true) {
        if let Some(path) = msg.get("fullOutputPath").and_then(Value::as_str) {
            text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
        }
    }
    text
}

pub fn create_branch_summary_message(summary: &str, from_id: &str, timestamp: &str) -> Value {
    json!({
        "role": "branchSummary",
        "summary": summary,
        "fromId": from_id,
        "timestamp": parse_timestamp(timestamp),
    })
}

pub fn create_compaction_summary_message(summary: &str, tokens_before: i64, timestamp: &str, details: Option<Value>) -> Value {
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::from("compactionSummary"));
    message.insert("summary".to_owned(), Value::from(summary));
    message.insert("tokensBefore".to_owned(), Value::from(tokens_before));
    if let Some(details) = details {
        message.insert("details".to_owned(), details);
    }
    message.insert("timestamp".to_owned(), Value::from(parse_timestamp(timestamp)));
    Value::Object(message)
}

pub fn create_custom_message(custom_type: &str, content: Value, display: bool, details: Option<Value>, timestamp: &str) -> Value {
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::from("custom"));
    message.insert("customType".to_owned(), Value::from(custom_type));
    message.insert("content".to_owned(), content);
    message.insert("display".to_owned(), Value::from(display));
    if let Some(details) = details {
        message.insert("details".to_owned(), details);
    }
    message.insert("timestamp".to_owned(), Value::from(parse_timestamp(timestamp)));
    Value::Object(message)
}

fn parse_timestamp(timestamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(timestamp).map(|value| value.timestamp_millis()).unwrap_or(0)
}

fn text_block(text: String) -> Value {
    json!({ "type": "text", "text": text })
}

fn user_message(content: Vec<Value>, timestamp: Option<i64>) -> Value {
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::from("user"));
    message.insert("content".to_owned(), Value::Array(content));
    if let Some(timestamp) = timestamp {
        message.insert("timestamp".to_owned(), Value::from(timestamp));
    }
    Value::Object(message)
}

pub fn convert_to_llm(messages: &[Value]) -> Vec<Value> {
    let mut converted: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or_default();
        let timestamp = message.get("timestamp").and_then(Value::as_i64);
        match role {
            "bashExecution" => {
                if message.get("excludeFromContext").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                converted.push(user_message(vec![text_block(bash_execution_to_text(message))], timestamp));
            }
            "custom" => {
                let custom_type = message.get("customType").and_then(Value::as_str).unwrap_or_default();
                if is_context_excluded_custom_message(custom_type) {
                    continue;
                }
                let content = match message.get("content") {
                    Some(Value::String(text)) => vec![text_block(text.clone())],
                    Some(Value::Array(blocks)) => blocks.clone(),
                    _ => Vec::new(),
                };
                converted.push(user_message(content, timestamp));
            }
            "branchSummary" => {
                let summary = message.get("summary").and_then(Value::as_str).unwrap_or_default();
                converted.push(user_message(
                    vec![text_block(format!("{BRANCH_SUMMARY_PREFIX}{summary}{BRANCH_SUMMARY_SUFFIX}"))],
                    timestamp,
                ));
            }
            "compactionSummary" => {
                let summary = message.get("summary").and_then(Value::as_str).unwrap_or_default();
                converted.push(user_message(
                    vec![text_block(format!("{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}"))],
                    timestamp,
                ));
            }
            _ => converted.push(message.clone()),
        }
    }
    drop_failed_assistant_turns(converted)
}

pub fn is_failed_stop(stop_reason: Option<&str>) -> bool {
    matches!(stop_reason, Some("error") | Some("aborted"))
}

pub fn drop_failed_assistant_turns(messages: Vec<Value>) -> Vec<Value> {
    let mut kept_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut failed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in &messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let target = if is_failed_stop(message.get("stopReason").and_then(Value::as_str)) {
            &mut failed_ids
        } else {
            &mut kept_ids
        };
        if let Some(Value::Array(blocks)) = message.get("content") {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("toolCall") {
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        target.insert(id.to_owned());
                    }
                }
            }
        }
    }
    for id in &kept_ids {
        failed_ids.remove(id);
    }
    messages
        .into_iter()
        .filter(|message| match message.get("role").and_then(Value::as_str) {
            Some("assistant") => !is_failed_stop(message.get("stopReason").and_then(Value::as_str)),
            Some("toolResult") => message
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(|id| !failed_ids.contains(id))
                .unwrap_or(true),
            _ => true,
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ElideOldImagesOptions {
    pub budget_bytes: Option<usize>,
    pub always_keep_newest: Option<usize>,
    pub max_historical_images: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct TransportConvertOptions {
    pub elide: ElideOldImagesOptions,
    pub block_images: bool,
}

pub fn dedupe_consecutive_placeholder(content: &[Value], placeholder: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(content.len());
    for block in content {
        let is_duplicate = block.get("type").and_then(Value::as_str) == Some("text")
            && block.get("text").and_then(Value::as_str) == Some(placeholder)
            && out.last().map(|previous| {
                previous.get("type").and_then(Value::as_str) == Some("text")
                    && previous.get("text").and_then(Value::as_str) == Some(placeholder)
            }) == Some(true);
        if !is_duplicate {
            out.push(block.clone());
        }
    }
    out
}

fn block_is_image(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("image")
}

fn block_data_len(block: &Value) -> usize {
    block.get("data").and_then(Value::as_str).map(|data| data.len()).unwrap_or(0)
}

pub fn elide_old_images(messages: &[Value], options: &ElideOldImagesOptions) -> Vec<Value> {
    let budget_bytes = options.budget_bytes.unwrap_or(TRANSPORT_IMAGE_BUDGET_BYTES);
    let always_keep_newest = options.always_keep_newest.unwrap_or(1);
    let max_historical_images = options.max_historical_images;
    let limit_history = max_historical_images.is_some();
    let last_assistant_index = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .map(|(index, _)| index)
        .next_back()
        .map(|index| index as i64)
        .unwrap_or(-1);

    let mut to_elide: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut kept_images = 0usize;
    let mut kept_historical_images = 0usize;
    let mut image_bytes = 0usize;
    let mut cutoff_reached = false;

    for message_index in (0..messages.len()).rev() {
        let message = &messages[message_index];
        let role = message.get("role").and_then(Value::as_str);
        if role != Some("user") && role != Some("toolResult") {
            continue;
        }
        let Some(Value::Array(content)) = message.get("content") else { continue };
        for block_index in (0..content.len()).rev() {
            let block = &content[block_index];
            if !block_is_image(block) {
                continue;
            }
            let is_current_turn = limit_history && (message_index as i64) > last_assistant_index;
            let is_protected_history = limit_history
                && !is_current_turn
                && max_historical_images.map(|limit| kept_historical_images < limit).unwrap_or(false);
            let exceeds_history_limit = limit_history && !is_current_turn && !is_protected_history;

            if exceeds_history_limit || (cutoff_reached && !is_current_turn && !is_protected_history) {
                to_elide.insert((message_index, block_index));
                continue;
            }
            if is_current_turn || is_protected_history || kept_images < always_keep_newest || image_bytes + block_data_len(block) <= budget_bytes {
                kept_images += 1;
                if !is_current_turn {
                    kept_historical_images += 1;
                }
                image_bytes += block_data_len(block);
                continue;
            }
            cutoff_reached = true;
            to_elide.insert((message_index, block_index));
        }
    }

    if to_elide.is_empty() {
        return messages.to_vec();
    }

    messages
        .iter()
        .enumerate()
        .map(|(message_index, message)| {
            let role = message.get("role").and_then(Value::as_str);
            if role != Some("user") && role != Some("toolResult") {
                return message.clone();
            }
            let Some(Value::Array(content)) = message.get("content") else { return message.clone() };
            if !content.iter().enumerate().any(|(block_index, _)| to_elide.contains(&(message_index, block_index))) {
                return message.clone();
            }
            let replaced: Vec<Value> = content
                .iter()
                .enumerate()
                .map(|(block_index, block)| {
                    if to_elide.contains(&(message_index, block_index)) {
                        text_block(IMAGE_ELISION_PLACEHOLDER.to_owned())
                    } else {
                        block.clone()
                    }
                })
                .collect();
            let mut updated = message.clone();
            updated["content"] = Value::Array(dedupe_consecutive_placeholder(&replaced, IMAGE_ELISION_PLACEHOLDER));
            updated
        })
        .collect()
}

pub fn convert_to_llm_for_transport(messages: &[Value], options: &TransportConvertOptions) -> Vec<Value> {
    let converted = convert_to_llm(messages);
    if !options.block_images {
        return elide_old_images(&converted, &options.elide);
    }
    converted
        .into_iter()
        .map(|message| {
            let role = message.get("role").and_then(Value::as_str);
            if role != Some("user") && role != Some("toolResult") {
                return message;
            }
            let Some(Value::Array(content)) = message.get("content") else { return message };
            if !content.iter().any(block_is_image) {
                return message;
            }
            let replaced: Vec<Value> = content
                .iter()
                .map(|block| {
                    if block_is_image(block) {
                        text_block(BLOCKED_IMAGE_PLACEHOLDER.to_owned())
                    } else {
                        block.clone()
                    }
                })
                .collect();
            let mut updated = message.clone();
            updated["content"] = Value::Array(dedupe_consecutive_placeholder(&replaced, BLOCKED_IMAGE_PLACEHOLDER));
            updated
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BT: &str = "\u{60}";

    #[test]
    fn bash_execution_text_matches_the_ts_shape() {
        let message = json!({
            "role": "bashExecution",
            "command": "ls",
            "output": "a\nb",
            "exitCode": 0,
            "cancelled": false,
            "truncated": false,
            "timestamp": 1,
        });
        assert_eq!(bash_execution_to_text(&message), format!("Ran {BT}ls{BT}\n{BT}{BT}{BT}\na\nb\n{BT}{BT}{BT}"));
    }

    #[test]
    fn bash_execution_text_reports_no_output_cancellation_exit_and_truncation() {
        let message = json!({
            "role": "bashExecution",
            "command": "x",
            "output": "",
            "exitCode": 2,
            "cancelled": false,
            "truncated": true,
            "fullOutputPath": "/tmp/full.txt",
            "timestamp": 1,
        });
        assert_eq!(
            bash_execution_to_text(&message),
            format!("Ran {BT}x{BT}\n(no output)\n\nCommand exited with code 2\n\n[Output truncated. Full output: /tmp/full.txt]")
        );
        let cancelled = json!({ "command": "y", "output": "", "cancelled": true, "truncated": false, "timestamp": 1 });
        assert_eq!(bash_execution_to_text(&cancelled), format!("Ran {BT}y{BT}\n(no output)\n\n(command cancelled)"));
    }

    #[test]
    fn convert_to_llm_projects_custom_roles_and_keeps_passthrough_messages() {
        let messages = vec![
            json!({ "role": "user", "content": "hi", "timestamp": 1 }),
            json!({ "role": "bashExecution", "command": "ls", "output": "o", "timestamp": 2, "extra": "kept" }),
            json!({ "role": "custom", "customType": "goal-continuation", "content": "c", "display": false, "timestamp": 3 }),
            json!({ "role": "branchSummary", "summary": "s", "fromId": "a", "timestamp": 4 }),
            json!({ "role": "compactionSummary", "summary": "c", "tokensBefore": 5, "timestamp": 6 }),
        ];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 5);
        assert_eq!(converted[0]["role"], "user");
        assert_eq!(converted[1]["role"], "user");
        assert_eq!(converted[1]["content"][0]["text"], format!("Ran {BT}ls{BT}\n{BT}{BT}{BT}\no\n{BT}{BT}{BT}"));
        assert_eq!(converted[2]["content"][0]["text"], "c");
        assert_eq!(converted[3]["content"][0]["text"], format!("{BRANCH_SUMMARY_PREFIX}s{BRANCH_SUMMARY_SUFFIX}"));
        assert_eq!(converted[4]["content"][0]["text"], format!("{COMPACTION_SUMMARY_PREFIX}c{COMPACTION_SUMMARY_SUFFIX}"));
    }

    #[test]
    fn convert_to_llm_drops_excluded_bash_messages() {
        let messages = vec![json!({ "role": "bashExecution", "command": "x", "output": "", "excludeFromContext": true, "timestamp": 1 })];
        assert!(convert_to_llm(&messages).is_empty());
        assert!(!is_context_excluded_custom_message(GOAL_CONTINUATION_MESSAGE_TYPE));
        let kept = vec![json!({ "role": "user", "content": "x", "timestamp": 1 })];
        assert_eq!(filter_context_excluded_messages(kept.clone()), kept);
    }

    #[test]
    fn convert_to_llm_drops_failed_assistant_turns_and_their_tool_results() {
        let messages = vec![
            json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "t1" }], "stopReason": "toolUse", "timestamp": 1 }),
            json!({ "role": "toolResult", "toolCallId": "t1", "toolName": "x", "content": [], "isError": false, "timestamp": 2 }),
            json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "t2" }], "stopReason": "error", "timestamp": 3 }),
            json!({ "role": "toolResult", "toolCallId": "t2", "toolName": "x", "content": [], "isError": false, "timestamp": 4 }),
        ];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["stopReason"], "toolUse");
        assert_eq!(converted[1]["toolCallId"], "t1");
    }

    #[test]
    fn a_tool_call_claimed_by_a_kept_turn_is_not_dropped() {
        let messages = vec![
            json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "t1" }], "stopReason": "error", "timestamp": 1 }),
            json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "t1" }], "stopReason": "toolUse", "timestamp": 2 }),
            json!({ "role": "toolResult", "toolCallId": "t1", "content": [], "timestamp": 3 }),
        ];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[1]["role"], "toolResult");
    }

    #[test]
    fn create_helpers_emit_iso_timestamps_as_epoch_millis() {
        let message = create_branch_summary_message("s", "id", "2026-09-24T01:49:15.521Z");
        assert_eq!(message["timestamp"], 1784935755521i64);
        assert_eq!(message["role"], "branchSummary");
        assert_eq!(message["fromId"], "id");
        let message = create_compaction_summary_message("s", 10, "2026-09-24T01:49:15.521Z", Some(json!({ "k": 1 })));
        assert_eq!(message["details"]["k"], 1);
        assert_eq!(message["tokensBefore"], 10);
        let message = create_custom_message("t", Value::from("body"), true, None, "2026-09-24T01:49:15.521Z");
        assert_eq!(message["display"], true);
        assert!(message.get("details").is_none());
    }

    #[test]
    fn dedupe_consecutive_placeholder_removes_adjacent_repeats_only() {
        let content = vec![
            text_block(IMAGE_ELISION_PLACEHOLDER.to_owned()),
            text_block(IMAGE_ELISION_PLACEHOLDER.to_owned()),
            text_block("other".to_owned()),
            text_block(IMAGE_ELISION_PLACEHOLDER.to_owned()),
        ];
        let deduped = dedupe_consecutive_placeholder(&content, IMAGE_ELISION_PLACEHOLDER);
        assert_eq!(deduped.len(), 3);
        assert_eq!(deduped[1]["text"], "other");
    }

    fn image_message(data_len: usize, timestamp: i64) -> Value {
        json!({
            "role": "user",
            "content": [{ "type": "image", "data": "x".repeat(data_len), "mimeType": "image/png" }],
            "timestamp": timestamp,
        })
    }

    #[test]
    fn elide_old_images_keeps_the_newest_and_replaces_the_rest_over_budget() {
        let messages = vec![image_message(10, 1), image_message(10, 2), image_message(10, 3)];
        let options = ElideOldImagesOptions { budget_bytes: Some(20), always_keep_newest: Some(1), ..Default::default() };
        let elided = elide_old_images(&messages, &options);
        assert_eq!(elided[2]["content"][0]["type"], "image");
        assert_eq!(elided[1]["content"][0]["type"], "image");
        assert_eq!(elided[0]["content"][0]["type"], "text");
        assert_eq!(elided[0]["content"][0]["text"], IMAGE_ELISION_PLACEHOLDER);
    }

    #[test]
    fn elide_old_images_returns_the_input_when_everything_fits() {
        let messages = vec![image_message(1, 1)];
        let elided = elide_old_images(&messages, &ElideOldImagesOptions::default());
        assert_eq!(elided, messages);
    }

    #[test]
    fn block_images_replaces_every_image() {
        let messages = vec![image_message(1, 1)];
        let options = TransportConvertOptions { block_images: true, ..Default::default() };
        let converted = convert_to_llm_for_transport(&messages, &options);
        assert_eq!(converted[0]["content"][0]["type"], "text");
        assert_eq!(converted[0]["content"][0]["text"], BLOCKED_IMAGE_PLACEHOLDER);
    }
}
