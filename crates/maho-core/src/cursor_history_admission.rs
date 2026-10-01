//! Port of senpi packages/coding-agent/src/core/cursor-history-admission.ts.
//!
//! Cursor request admission: each tool result is capped on its own, then the oldest result bodies
//! are blanked while the aggregate model input exceeds the model window. Admission NEVER deletes a
//! turn; a history that still exceeds the budget after blanking is admitted as-is.

use serde_json::Value;

use crate::messages::convert_to_llm;

pub const CURSOR_TOOL_RESULT_MAX_CHARS: usize = 2000;
const CURSOR_TRUNCATION_MARKER: &str = "\n...[truncated]";
const CHARS_PER_TOKEN: usize = 4;

/// Aggregate admission budget for a model whose window is contextWindowTokens.
pub fn cursor_admission_budget_bytes(context_window_tokens: f64) -> usize {
    if !context_window_tokens.is_finite() || context_window_tokens <= 0.0 {
        return 0;
    }
    context_window_tokens.floor() as usize * CHARS_PER_TOKEN
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAdmissionResult {
    pub messages: Option<Vec<Value>>,
    pub changed: bool,
    pub blanked_tool_results: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub over_budget: bool,
}

fn grapheme_count(text: &str) -> usize {
    text.chars().count()
}

fn cap_tool_result_bodies(messages: &[Value], max_chars: usize) -> (Vec<Value>, bool) {
    let marker_chars = grapheme_count(CURSOR_TRUNCATION_MARKER);
    let mut result = messages.to_vec();
    let mut changed = false;
    for message_index in (0..result.len()).rev() {
        let Some(message) = result[message_index].as_object() else { continue };
        if message.get("role").and_then(Value::as_str) != Some("toolResult") {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else { continue };
        let mut content = content.clone();
        let mut content_changed = false;
        for part_index in (0..content.len()).rev() {
            let Some(part) = content[part_index].as_object() else { continue };
            match part.get("type").and_then(Value::as_str) {
                Some("image") if part.get("data").and_then(Value::as_str).is_some() => continue,
                Some("text") => {}
                _ => continue,
            }
            let Some(text) = part.get("text").and_then(Value::as_str) else { continue };
            if grapheme_count(text) <= max_chars {
                continue;
            }
            let keep = max_chars.saturating_sub(marker_chars);
            let kept: String = text.chars().take(keep).collect();
            let mut updated = part.clone();
            updated.insert("text".into(), Value::String(format!("{kept}{CURSOR_TRUNCATION_MARKER}")));
            content[part_index] = Value::Object(updated);
            content_changed = true;
            changed = true;
        }
        if content_changed
            && let Some(object) = result[message_index].as_object_mut()
        {
            object.insert("content".into(), Value::Array(content));
        }
    }
    (result, changed)
}

fn empty_tool_result(message: &Value) -> Value {
    let Some(object) = message.as_object() else { return message.clone() };
    if object.get("role").and_then(Value::as_str) != Some("toolResult") {
        return message.clone();
    }
    let Some(content) = object.get("content").and_then(Value::as_array) else { return message.clone() };
    let emptied: Vec<Value> = content
        .iter()
        .map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                let mut updated = part.clone();
                if let Some(part_object) = updated.as_object_mut() {
                    part_object.insert("text".into(), Value::String(String::new()));
                }
                updated
            }
            Some("image") => {
                let mut updated = part.clone();
                if let Some(part_object) = updated.as_object_mut() {
                    part_object.insert("data".into(), Value::String(String::new()));
                }
                updated
            }
            _ => part.clone(),
        })
        .collect();
    let mut filtered: Vec<Value> = Vec::new();
    for (index, part) in emptied.iter().enumerate() {
        if index == 0 {
            filtered.push(part.clone());
            continue;
        }
        let previous = &emptied[index - 1];
        if !part_is_empty(part) || !part_is_empty(previous) {
            filtered.push(part.clone());
        }
    }
    let mut updated = object.clone();
    updated.insert("content".into(), Value::Array(filtered));
    Value::Object(updated)
}

fn part_is_empty(part: &Value) -> bool {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => part.get("text").and_then(Value::as_str).map(str::is_empty).unwrap_or(false),
        Some("image") => part.get("data").and_then(Value::as_str).map(str::is_empty).unwrap_or(false),
        _ => false,
    }
}

/// Empties the oldest result bodies until the rest fits; the prefix search is monotonic.
fn blank_oldest_tool_results(
    messages: &[Value],
    fits: impl Fn(&[Value]) -> bool,
) -> (Vec<Value>, usize) {
    let tool_result_indexes: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message.get("role").and_then(Value::as_str) == Some("toolResult")
                && message.get("content").and_then(Value::as_array).is_some()
        })
        .map(|(index, _)| index)
        .collect();
    let with_empty_prefix = |count: usize| -> Vec<Value> {
        let mut candidate = messages.to_vec();
        for index in tool_result_indexes.iter().take(count) {
            candidate[*index] = empty_tool_result(&candidate[*index]);
        }
        candidate
    };
    let (mut low, mut high) = (0usize, tool_result_indexes.len());
    while low < high {
        let middle = (low + high) / 2;
        if fits(&with_empty_prefix(middle + 1)) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let count = (low + 1).min(tool_result_indexes.len());
    (if count > 0 { with_empty_prefix(count) } else { messages.to_vec() }, count)
}

fn measure(converted: &[Value]) -> usize {
    let active_user_message_index = converted
        .last()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(|_| converted.len() - 1);
    let messages: Vec<maho_ai::types::Message> =
        converted.iter().filter_map(|value| serde_json::from_value(value.clone()).ok()).collect();
    maho_ai::api::cursor_agent::measure::measure_cursor_model_input_serialized_bytes(&messages, active_user_message_index)
}

/// Bounds one Cursor request: per-result caps first, then blanking the oldest tool result bodies
/// while the model input exceeds budgetBytes. Conversation turns are never removed.
pub fn admit_cursor_history(messages: Option<&[Value]>, budget_bytes: usize, max_chars: Option<usize>) -> CursorAdmissionResult {
    let Some(messages) = messages.filter(|messages| !messages.is_empty()) else {
        return CursorAdmissionResult {
            messages: messages.map(<[Value]>::to_vec),
            changed: false,
            blanked_tool_results: 0,
            bytes_before: 0,
            bytes_after: 0,
            over_budget: false,
        };
    };
    let (capped, capped_changed) = cap_tool_result_bodies(messages, max_chars.unwrap_or(CURSOR_TOOL_RESULT_MAX_CHARS));
    let measure_candidate = |candidate: &[Value]| measure(&convert_to_llm(candidate));

    let bytes_before = measure_candidate(&capped);
    if bytes_before <= budget_bytes {
        return CursorAdmissionResult {
            messages: Some(if capped_changed { capped } else { messages.to_vec() }),
            changed: capped_changed,
            blanked_tool_results: 0,
            bytes_before,
            bytes_after: bytes_before,
            over_budget: false,
        };
    }

    let (blanked, count) = blank_oldest_tool_results(&capped, |candidate| measure_candidate(candidate) <= budget_bytes);
    let bytes_after = if count > 0 { measure_candidate(&blanked) } else { bytes_before };
    let changed = capped_changed || count > 0;
    CursorAdmissionResult {
        messages: Some(if changed { blanked } else { messages.to_vec() }),
        changed,
        blanked_tool_results: count,
        bytes_before,
        bytes_after,
        over_budget: bytes_after > budget_bytes,
    }
}

/// Positional entry point kept for existing callers: an omitted maxBytes applies the per-result cap
/// only.
pub fn truncate_tool_result_bodies(messages: Option<&[Value]>, max_chars: Option<usize>, max_bytes: Option<usize>) -> (Option<Vec<Value>>, bool) {
    let admission = admit_cursor_history(messages, max_bytes.unwrap_or(usize::MAX), max_chars);
    (admission.messages, admission.changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_result(text: &str) -> Value {
        json!({ "role": "toolResult", "toolCallId": "1", "toolName": "read", "isError": false, "timestamp": 0,
            "content": [{ "type": "text", "text": text }] })
    }

    #[test]
    fn the_budget_scales_with_the_window() {
        assert_eq!(cursor_admission_budget_bytes(1000.0), 4000);
        assert_eq!(cursor_admission_budget_bytes(0.0), 0);
        assert_eq!(cursor_admission_budget_bytes(f64::NAN), 0);
    }

    #[test]
    fn a_short_result_is_untouched() {
        let messages = vec![tool_result("short")];
        let result = admit_cursor_history(Some(&messages), usize::MAX, None);
        assert!(!result.changed);
        assert_eq!(result.blanked_tool_results, 0);
    }

    #[test]
    fn a_long_result_is_capped_with_the_marker() {
        let long = "a".repeat(5000);
        let messages = vec![tool_result(&long)];
        let result = admit_cursor_history(Some(&messages), usize::MAX, Some(2000));
        assert!(result.changed);
        let text = result.messages.expect("messages")[0]["content"][0]["text"].as_str().unwrap_or_default().to_owned();
        assert!(text.ends_with("...[truncated]"));
        assert!(grapheme_count(&text) <= 2000);
    }

    #[test]
    fn an_empty_history_is_unchanged() {
        let result = admit_cursor_history(Some(&[]), 100, None);
        assert!(!result.changed);
        assert!(!result.over_budget);
    }

    #[test]
    fn an_over_budget_history_blanks_oldest_results() {
        let long = "b".repeat(3000);
        let messages = vec![tool_result(&long), tool_result(&long)];
        let result = admit_cursor_history(Some(&messages), 10, Some(3000));
        assert!(result.changed);
        assert!(result.blanked_tool_results <= 2);
        assert!(result.bytes_after <= result.bytes_before);
    }
}
