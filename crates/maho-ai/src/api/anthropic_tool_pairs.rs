//! Port of senpi packages/ai/src/api/anthropic-tool-pairs.ts.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

const SYNTHETIC_OUTPUT: &str = "Tool output unavailable (interrupted before result)";

fn has_messages_array(payload: &Map<String, Value>) -> Option<Vec<Value>> {
    payload.get("messages").and_then(Value::as_array).cloned()
}

fn is_content_block_param(value: &Value) -> bool {
    value.is_object() && value.get("type").and_then(Value::as_str).is_some()
}

fn tool_use_id(block: &Value) -> Option<&str> {
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    block.get("id").and_then(Value::as_str)
}

fn is_tool_result_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("tool_result")
}

fn message_with_array_content(value: &Value, role: &str) -> Option<Vec<Value>> {
    if value.get("role").and_then(Value::as_str) != Some(role) {
        return None;
    }
    let content = value.get("content").and_then(Value::as_array)?;
    if !content.iter().all(is_content_block_param) {
        return None;
    }
    Some(content.clone())
}

fn is_user_message_with_string_content(value: &Value) -> Option<&str> {
    if value.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    value.get("content").and_then(Value::as_str)
}

fn clone_with(block: &Value, key: &str, value: Value) -> Value {
    let mut clone = block.as_object().cloned().unwrap_or_default();
    clone.insert(key.to_owned(), value);
    Value::Object(clone)
}

fn with_content(message: &Value, content: Vec<Value>) -> Value {
    let mut message = message.as_object().cloned().unwrap_or_default();
    message.insert("content".to_owned(), Value::Array(content));
    Value::Object(message)
}

struct AssistantToolUseDedupe {
    message: Value,
    changed: bool,
    expected_ids: Vec<String>,
    remap_queues: BTreeMap<String, Vec<String>>,
}

fn dedupe_assistant_tool_uses(message: &Value, used_ids: &mut HashSet<String>) -> AssistantToolUseDedupe {
    let blocks = message.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut content: Vec<Value> = Vec::new();
    let mut expected_ids: Vec<String> = Vec::new();
    let mut remap_queues: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut changed = false;

    for block in &blocks {
        let Some(id) = tool_use_id(block) else {
            content.push(block.clone());
            continue;
        };
        if id.is_empty() {
            content.push(block.clone());
            continue;
        }
        let mut final_id = id.to_owned();
        if used_ids.contains(&final_id) {
            let mut suffix = 2;
            while used_ids.contains(&format!("{id}__dedup{suffix}")) {
                suffix += 1;
            }
            final_id = format!("{id}__dedup{suffix}");
            changed = true;
        }
        used_ids.insert(final_id.clone());
        expected_ids.push(final_id.clone());
        remap_queues.entry(id.to_owned()).or_default().push(final_id.clone());
        if final_id == id {
            content.push(block.clone());
        } else {
            content.push(clone_with(block, "id", Value::String(final_id)));
        }
    }

    if !changed {
        return AssistantToolUseDedupe { message: message.clone(), changed: false, expected_ids, remap_queues };
    }
    AssistantToolUseDedupe { message: with_content(message, content), changed: true, expected_ids, remap_queues }
}

fn synthetic_tool_result(tool_use_id: &str) -> Value {
    let mut block = Map::new();
    block.insert("type".to_owned(), Value::String("tool_result".into()));
    block.insert("tool_use_id".to_owned(), Value::String(tool_use_id.into()));
    block.insert("content".to_owned(), Value::String(SYNTHETIC_OUTPUT.into()));
    block.insert("is_error".to_owned(), Value::Bool(true));
    Value::Object(block)
}

// The TS `sameBlocks` compares by reference identity; every block here is an untouched element of
// `message.content` or a rewritten copy, so element-wise equality decides the same way.
fn same_blocks(left: &[Value], right: &[Value]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(block, other)| block == other)
}

fn repair_following_user_message(
    message: &Value,
    expected_ids: &[String],
    remap_queues: &mut BTreeMap<String, Vec<String>>,
) -> (Value, bool) {
    let expected: HashSet<&String> = expected_ids.iter().collect();
    let mut found: HashSet<String> = HashSet::new();
    let mut results: Vec<Value> = Vec::new();
    let mut ordinary: Vec<Value> = Vec::new();

    let blocks = message.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
    for block in &blocks {
        if !is_tool_result_block(block) {
            ordinary.push(block.clone());
            continue;
        }
        let raw_id = block.get("tool_use_id").cloned().unwrap_or(Value::Null);
        let mut id = raw_id.clone();
        if let Some(raw) = raw_id.as_str()
            && let Some(queue) = remap_queues.get_mut(raw)
            && !queue.is_empty()
        {
            id = Value::String(queue.remove(0));
        }
        let Some(id) = id.as_str() else { continue };
        if !expected.contains(&id.to_owned()) || found.contains(id) {
            continue;
        }
        found.insert(id.to_owned());
        if Some(id) == raw_id.as_str() {
            results.push(block.clone());
        } else {
            results.push(clone_with(block, "tool_use_id", Value::String(id.to_owned())));
        }
    }

    for id in expected_ids {
        if !found.contains(id) {
            results.push(synthetic_tool_result(id));
        }
    }

    let mut content = results;
    content.extend(ordinary);
    if same_blocks(&content, &blocks) {
        (message.clone(), false)
    } else {
        (with_content(message, content), true)
    }
}

fn remove_orphan_results(message: &Value) -> (Option<Value>, bool) {
    let blocks = message.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
    let content: Vec<Value> = blocks.iter().filter(|block| !is_tool_result_block(block)).cloned().collect();
    if content.len() == blocks.len() {
        return (Some(message.clone()), false);
    }
    if content.is_empty() {
        return (None, true);
    }
    (Some(with_content(message, content)), true)
}

/// Repairs Anthropic client tool_use/tool_result adjacency and duplicate tool_use ids without
/// mutating the input payload.
pub fn sanitize_anthropic_tool_pairs(payload: &Map<String, Value>) -> Map<String, Value> {
    let Some(messages) = has_messages_array(payload) else {
        return payload.clone();
    };

    let mut changed = false;
    let mut used_tool_use_ids: HashSet<String> = HashSet::new();
    let mut sanitized: Vec<Value> = Vec::new();

    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        if message_with_array_content(message, "assistant").is_some() {
            let AssistantToolUseDedupe { message: deduped, changed: deduped_changed, expected_ids, mut remap_queues } =
                dedupe_assistant_tool_uses(message, &mut used_tool_use_ids);
            sanitized.push(deduped);
            changed |= deduped_changed;
            if expected_ids.is_empty() {
                index += 1;
                continue;
            }

            let next = messages.get(index + 1);
            if let Some(next) = next
                && message_with_array_content(next, "user").is_some()
            {
                let (repaired, repaired_changed) = repair_following_user_message(next, &expected_ids, &mut remap_queues);
                sanitized.push(repaired);
                changed |= repaired_changed;
                index += 2;
                continue;
            }

            if let Some(text) = next.and_then(is_user_message_with_string_content) {
                let mut content: Vec<Value> = expected_ids.iter().map(|id| synthetic_tool_result(id)).collect();
                content.push(serde_json::json!({ "type": "text", "text": text }));
                sanitized.push(with_content(next.expect("next message"), content));
                changed = true;
                index += 2;
                continue;
            }

            sanitized.push(serde_json::json!({
                "role": "user",
                "content": expected_ids.iter().map(|id| synthetic_tool_result(id)).collect::<Vec<_>>(),
            }));
            changed = true;
            index += 1;
            continue;
        }

        if message_with_array_content(message, "user").is_some() {
            let (repaired, repaired_changed) = remove_orphan_results(message);
            if let Some(repaired) = repaired {
                sanitized.push(repaired);
            }
            changed |= repaired_changed;
            index += 1;
            continue;
        }

        sanitized.push(message.clone());
        index += 1;
    }

    if !changed {
        return payload.clone();
    }
    let mut out = payload.clone();
    out.insert("messages".to_owned(), Value::Array(sanitized));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sanitize(payload: &Value) -> Value {
        Value::Object(sanitize_anthropic_tool_pairs(payload.as_object().expect("object payload")))
    }

    fn tool_use(id: &str, name: &str) -> Value {
        json!({ "type": "tool_use", "id": id, "name": name, "input": {} })
    }

    fn tool_result(id: &str, content: &str) -> Value {
        json!({ "type": "tool_result", "tool_use_id": id, "content": content })
    }

    fn messages_of(payload: &Value) -> Vec<Value> {
        payload.get("messages").and_then(Value::as_array).cloned().expect("messages")
    }

    fn content_of(message: &Value) -> Vec<Value> {
        message.get("content").and_then(Value::as_array).cloned().expect("content")
    }

    #[test]
    fn renames_duplicate_ids_within_one_assistant_message_and_remaps_results_in_call_order() {
        let payload = json!({
            "messages": [
                { "role": "assistant", "content": [tool_use("StrReplace_0_aa-1", "read"), tool_use("StrReplace_0_aa-1", "write")] },
                { "role": "user", "content": [tool_result("StrReplace_0_aa-1", "read output"), tool_result("StrReplace_0_aa-1", "write output")] },
            ],
        });

        let sanitized = sanitize(&payload);
        let messages = messages_of(&sanitized);
        let assistant = &messages[0];
        let user = &messages[1];

        let use_ids: Vec<String> = content_of(assistant)
            .iter()
            .map(|block| block.get("id").and_then(Value::as_str).expect("id").to_owned())
            .collect();
        assert_eq!(use_ids.len(), 2);
        assert_eq!(use_ids[0], "StrReplace_0_aa-1");
        assert_ne!(use_ids[1], "StrReplace_0_aa-1");
        assert_eq!(use_ids.iter().collect::<HashSet<_>>().len(), 2);

        let results: Vec<Value> = content_of(user)
            .into_iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].get("tool_use_id").and_then(Value::as_str), Some(use_ids[0].as_str()));
        assert_eq!(results[0].get("content").and_then(Value::as_str), Some("read output"));
        assert_eq!(results[1].get("tool_use_id").and_then(Value::as_str), Some(use_ids[1].as_str()));
        assert_eq!(results[1].get("content").and_then(Value::as_str), Some("write output"));
    }

    #[test]
    fn renames_ids_duplicated_across_assistant_messages() {
        let payload = json!({
            "messages": [
                { "role": "assistant", "content": [tool_use("dup-1", "read")] },
                { "role": "user", "content": [tool_result("dup-1", "first")] },
                { "role": "assistant", "content": [tool_use("dup-1", "write")] },
                { "role": "user", "content": [tool_result("dup-1", "second")] },
            ],
        });

        let sanitized = sanitize(&payload);
        let messages = messages_of(&sanitized);

        let first_id = content_of(&messages[0])[0].get("id").and_then(Value::as_str).expect("id").to_owned();
        let second_id = content_of(&messages[2])[0].get("id").and_then(Value::as_str).expect("id").to_owned();
        assert_eq!(first_id, "dup-1");
        assert_ne!(second_id, "dup-1");

        assert_eq!(content_of(&messages[1])[0].get("tool_use_id").and_then(Value::as_str), Some(first_id.as_str()));
        assert_eq!(content_of(&messages[3])[0].get("tool_use_id").and_then(Value::as_str), Some(second_id.as_str()));
    }

    #[test]
    fn synthesizes_an_error_result_for_a_renamed_call_whose_result_is_missing() {
        let payload = json!({
            "messages": [
                { "role": "assistant", "content": [tool_use("solo-1", "read"), tool_use("solo-1", "write")] },
                { "role": "user", "content": [tool_result("solo-1", "only result")] },
            ],
        });

        let sanitized = sanitize(&payload);
        let messages = messages_of(&sanitized);
        let use_ids: Vec<String> = content_of(&messages[0])
            .iter()
            .map(|block| block.get("id").and_then(Value::as_str).expect("id").to_owned())
            .collect();
        let results: Vec<Value> = content_of(&messages[1])
            .into_iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect();

        assert_eq!(use_ids.iter().collect::<HashSet<_>>().len(), 2);
        assert_eq!(results.len(), 2);
        let mut result_ids: Vec<String> = results
            .iter()
            .map(|block| block.get("tool_use_id").and_then(Value::as_str).expect("tool_use_id").to_owned())
            .collect();
        let mut sorted_use_ids = use_ids.clone();
        result_ids.sort();
        sorted_use_ids.sort();
        assert_eq!(result_ids, sorted_use_ids);
    }

    #[test]
    fn returns_the_payload_unchanged_when_every_id_is_already_unique() {
        let payload = json!({
            "messages": [
                { "role": "assistant", "content": [tool_use("a-1", "read"), tool_use("b-2", "write")] },
                { "role": "user", "content": [tool_result("a-1", "one"), tool_result("b-2", "two")] },
            ],
        });

        assert_eq!(sanitize(&payload), payload);
    }
}
