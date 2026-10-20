//! Port of senpi packages/ai/src/api/anthropic-tool-references.ts.

use std::borrow::Cow;

use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value, json};

use crate::utils::tool_name_match::resolve_tool_name_match;
use crate::utils::unavailable_tool_text::{demoted_tool_call_text, demoted_tool_result_text};

struct AvailableToolNames {
    defined: IndexSet<String>,
}

#[cfg(test)]
mod replay_tests {
    use super::*;

    #[test]
    fn reference_names_follow_shared_match_specificity_and_ambiguity() {
        for (requested, names, expected) in [
            ("MCP__my_server__Memory", vec!["memory"], Some("memory")),
            (
                "mcp__srv__x__Tool",
                vec!["x__tool", "Tool"],
                Some("x__tool"),
            ),
            (
                "create_issue",
                vec!["mcp_github_create_issue"],
                Some("mcp_github_create_issue"),
            ),
            (
                "create_issue",
                vec!["mcp_a_create_issue", "mcp_b_create_issue"],
                None,
            ),
            ("mcp__sandbox__run_bash", vec!["bash"], None),
            (
                "mcp__a__READ",
                vec!["mcp__a__read", "READ"],
                Some("mcp__a__read"),
            ),
            ("Read", vec!["Read", "read"], Some("Read")),
        ] {
            let tools = json!(
                names
                    .into_iter()
                    .map(|name| json!({"name":name}))
                    .collect::<Vec<_>>()
            );
            let available = collect_available_tool_names(Some(&tools));
            assert_eq!(
                resolve_available_tool_name(requested, &available).as_deref(),
                expected
            );
        }
    }

    #[test]
    fn referenced_tool_becomes_resident_when_replay_is_demoted() {
        let params = json!({"tools":[{"name":"read","defer_loading":true},{"name":"write","defer_loading":true}],
            "messages":[{"role":"assistant","content":[
                {"type":"server_tool_use","id":"search","name":"tool_search_tool_bm25"},
                {"type":"tool_search_tool_result","tool_use_id":"search","content":{"tool_references":[{"type":"tool_reference","tool_name":"read"}]}}
            ]}]}).as_object().cloned().unwrap_or_default();
        let demoted = demote_tool_reference_replay(&params);
        assert_eq!(
            demoted["messages"][0]["content"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(demoted["messages"][0]["content"][0]["type"], "text");
        assert!(demoted["tools"][0].get("defer_loading").is_none());
        assert_eq!(demoted["tools"][1]["defer_loading"], true);
        assert_eq!(params["tools"][0]["defer_loading"], true);
    }
}

/// Demote replayed references even when their declarations are available: some endpoints
/// cannot resolve references returned by an earlier request.
pub fn demote_tool_reference_replay(params: &Map<String, Value>) -> Map<String, Value> {
    let mut out = params.clone();
    let Some(messages) = out.get_mut("messages").and_then(Value::as_array_mut) else {
        return out;
    };
    let search_ids: IndexSet<String> = messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_search_tool_result")
        .filter_map(|block| block["tool_use_id"].as_str().map(str::to_owned))
        .collect();
    let mut names = IndexSet::new();
    let mut changed = false;
    messages.retain_mut(|message| {
        let assistant = message["role"] == "assistant";
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            return true;
        };
        let mut message_changed = false;
        content.retain_mut(|block| {
            if assistant && block["type"] == "server_tool_use"
                && block["id"].as_str().is_some_and(|id| search_ids.contains(id))
            {
                message_changed = true;
                return false;
            }
            if assistant && block["type"] == "tool_search_tool_result" {
                let found = reference_names(&block["content"]);
                names.extend(found.iter().cloned());
                *block = json!({"type":"text", "text": if found.is_empty() {
                    "Tool search found no tools.".to_owned()
                } else { format!("Tool search found: {}", dedupe_preserving_order(&found).join(", ")) }});
                message_changed = true;
            } else if block["type"] == "tool_result" {
                if let Some(items) = block.get_mut("content").and_then(Value::as_array_mut) {
                    let found: IndexSet<String> = items.iter()
                        .filter(|item| item["type"] == "tool_reference")
                        .filter_map(|item| item["tool_name"].as_str().map(str::to_owned))
                        .collect();
                    if !found.is_empty() {
                        items.retain(|item| item["type"] != "tool_reference");
                        items.push(json!({"type":"text", "text":format!("Tools loaded: {}", found.iter().cloned().collect::<Vec<_>>().join(", "))}));
                        names.extend(found);
                        message_changed = true;
                    }
                }
            }
            true
        });
        changed |= message_changed;
        !message_changed || !content.is_empty()
    });
    if changed {
        if let Some(tools) = out.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                if tool["defer_loading"] == true
                    && tool["name"]
                        .as_str()
                        .is_some_and(|name| names.contains(name))
                {
                    if let Some(tool) = tool.as_object_mut() {
                        tool.remove("defer_loading");
                    }
                }
            }
        }
    }
    out
}

fn collect_available_tool_names(tools: Option<&Value>) -> AvailableToolNames {
    let mut defined: IndexSet<String> = IndexSet::new();
    if let Some(tools) = tools.and_then(Value::as_array) {
        for tool in tools {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                defined.insert(name.to_owned());
            }
        }
    }
    AvailableToolNames { defined }
}

fn resolve_available_tool_name(name: &str, available: &AvailableToolNames) -> Option<String> {
    resolve_tool_name_match(name, available.defined.iter().cloned())
}

fn is_native_tool_search_result_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("tool_search_tool_result")
        && block.get("tool_use_id").and_then(Value::as_str).is_some()
        && block.get("content").and_then(Value::as_object).is_some()
        && block
            .get("content")
            .and_then(|content| content.get("tool_references"))
            .and_then(Value::as_array)
            .is_some()
}

fn reference_names(content: &Value) -> Vec<String> {
    content
        .get("tool_references")
        .and_then(Value::as_array)
        .map(|references| {
            references
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_reference"))
                .map(|item| {
                    item.get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn dedupe_preserving_order(names: &[String]) -> Vec<String> {
    let mut seen: IndexSet<String> = IndexSet::new();
    for name in names {
        seen.insert(name.clone());
    }
    seen.into_iter().collect()
}

fn tool_result_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    if let Some(items) = content.as_array() {
        let parts: Vec<String> = items
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = item.get("text").and_then(Value::as_str)
                {
                    return Some(text.to_owned());
                }
                item.get("type")
                    .and_then(Value::as_str)
                    .map(|kind| format!("[{kind}]"))
            })
            .collect();
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    "Tool output unavailable.".to_owned()
}

fn key_of(value: &Value) -> String {
    value.to_string()
}

struct Rewrite {
    kept: Vec<Value>,
    omitted: Vec<String>,
}

fn rewrite_tool_reference_items(
    items: &[Value],
    available: &AvailableToolNames,
) -> Option<Rewrite> {
    let mut kept: Vec<Value> = Vec::new();
    let mut omitted: Vec<String> = Vec::new();
    let mut rewritten = false;
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("tool_reference") {
            kept.push(item.clone());
            continue;
        }
        let Some(name) = item.get("tool_name").and_then(Value::as_str) else {
            kept.push(item.clone());
            continue;
        };
        match resolve_available_tool_name(name, available) {
            None => {
                omitted.push(name.to_owned());
                rewritten = true;
            }
            Some(resolved) if resolved != name => {
                let mut renamed = item.as_object().cloned().unwrap_or_default();
                renamed.insert("tool_name".to_owned(), Value::String(resolved));
                kept.push(Value::Object(renamed));
                rewritten = true;
            }
            Some(_) => kept.push(item.clone()),
        }
    }
    if rewritten {
        Some(Rewrite { kept, omitted })
    } else {
        None
    }
}

pub fn demote_unavailable_tool_references(params: &Map<String, Value>) -> Map<String, Value> {
    demote_unavailable_tool_references_if_changed(params).unwrap_or_else(|| params.clone())
}

/// Same rewrite as [`demote_unavailable_tool_references`], but `None` when nothing changes so
/// callers can skip copying the payload.
pub fn demote_unavailable_tool_references_if_changed(
    params: &Map<String, Value>,
) -> Option<Map<String, Value>> {
    let messages = params.get("messages").and_then(Value::as_array)?;
    if messages.is_empty() {
        return None;
    }

    let available = collect_available_tool_names(params.get("tools"));
    let resolve = |name: &str| resolve_available_tool_name(name, &available);

    let mut demoted_call_names: IndexMap<String, String> = IndexMap::new();
    let mut renamed_call_names: IndexMap<String, String> = IndexMap::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(name) = block.get("name").and_then(Value::as_str) else {
                continue;
            };
            match resolve(name) {
                None => {
                    demoted_call_names.insert(
                        key_of(&block.get("id").cloned().unwrap_or(Value::Null)),
                        name.to_owned(),
                    );
                }
                Some(resolved) if resolved != name => {
                    renamed_call_names.insert(
                        key_of(&block.get("id").cloned().unwrap_or(Value::Null)),
                        resolved,
                    );
                }
                Some(_) => {}
            }
        }
    }

    let available_tool_names: Vec<String> = available.defined.iter().cloned().collect();
    let mut seen_demoted_call_names: IndexSet<String> = IndexSet::new();
    // Stays `None` until the first change; unchanged messages are never copied before then.
    let mut rewritten_messages: Option<Vec<Value>> = None;

    for (index, message) in messages.iter().enumerate() {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            if let Some(out) = rewritten_messages.as_mut() {
                out.push(message.clone());
            }
            continue;
        };
        let is_assistant = message.get("role").and_then(Value::as_str) == Some("assistant");
        let mut message_changed = false;

        let mut dropped_search_use_ids: IndexSet<String> = IndexSet::new();
        let mut dropped_search_names: IndexMap<String, Vec<String>> = IndexMap::new();
        if is_assistant {
            for block in content {
                if !is_native_tool_search_result_block(block) {
                    continue;
                }
                let names = reference_names(block.get("content").unwrap_or(&Value::Null));
                if !names.is_empty() && names.iter().all(|name| resolve(name).is_none()) {
                    let key = key_of(block.get("tool_use_id").unwrap_or(&Value::Null));
                    dropped_search_use_ids.insert(key.clone());
                    dropped_search_names.insert(key, names);
                }
            }
        }

        let mut next_content: Vec<Cow<'_, Value>> = Vec::new();
        for block in content {
            if is_assistant && block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let key = key_of(block.get("id").unwrap_or(&Value::Null));
                if let Some(name) = demoted_call_names.get(&key) {
                    message_changed = true;
                    let first_occurrence = seen_demoted_call_names.insert(name.clone());
                    next_content.push(Cow::Owned(json!({
                        "type": "text",
                        "text": demoted_tool_call_text(name, &available_tool_names, first_occurrence),
                    })));
                    continue;
                }
                if let Some(name) = renamed_call_names.get(&key) {
                    message_changed = true;
                    let mut renamed = block.as_object().cloned().unwrap_or_default();
                    renamed.insert("name".to_owned(), Value::String(name.clone()));
                    next_content.push(Cow::Owned(Value::Object(renamed)));
                    continue;
                }
            }
            if is_assistant
                && block.get("type").and_then(Value::as_str) == Some("server_tool_use")
                && block.get("id").and_then(Value::as_str).is_some()
                && dropped_search_use_ids.contains(&key_of(block.get("id").unwrap_or(&Value::Null)))
            {
                message_changed = true;
                continue;
            }
            if is_assistant && is_native_tool_search_result_block(block) {
                let key = key_of(block.get("tool_use_id").unwrap_or(&Value::Null));
                if let Some(omitted) = dropped_search_names.get(&key) {
                    message_changed = true;
                    next_content.push(Cow::Owned(json!({
                        "type": "text",
                        "text": format!("Tool reference unavailable: {}", dedupe_preserving_order(omitted).join(", ")),
                    })));
                    continue;
                }
                let references = block
                    .get("content")
                    .and_then(|content| content.get("tool_references"))
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                if let Some(rewritten) = rewrite_tool_reference_items(references, &available) {
                    message_changed = true;
                    let mut next_block = block.as_object().cloned().unwrap_or_default();
                    let mut next_content_map = next_block
                        .get("content")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_else(Map::new);
                    next_content_map
                        .insert("tool_references".to_owned(), Value::Array(rewritten.kept));
                    next_block.insert("content".to_owned(), Value::Object(next_content_map));
                    next_content.push(Cow::Owned(Value::Object(next_block)));
                    continue;
                }
            }
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let key = key_of(block.get("tool_use_id").unwrap_or(&Value::Null));
                if let Some(name) = demoted_call_names.get(&key) {
                    message_changed = true;
                    let text = demoted_tool_result_text(
                        name,
                        &tool_result_text(block.get("content").unwrap_or(&Value::Null)),
                    );
                    next_content.push(Cow::Owned(json!({ "type": "text", "text": text })));
                    continue;
                }
                if let Some(items) = block.get("content").and_then(Value::as_array)
                    && let Some(rewritten) = rewrite_tool_reference_items(items, &available)
                {
                    message_changed = true;
                    let next = if rewritten.kept.is_empty() {
                        json!([{
                            "type": "text",
                            "text": format!(
                                "Tool reference unavailable: {}",
                                dedupe_preserving_order(&rewritten.omitted).join(", "),
                            ),
                        }])
                    } else {
                        Value::Array(rewritten.kept)
                    };
                    let mut next_block = block.as_object().cloned().unwrap_or_default();
                    next_block.insert("content".to_owned(), next);
                    next_content.push(Cow::Owned(Value::Object(next_block)));
                    continue;
                }
            }
            next_content.push(Cow::Borrowed(block));
        }

        if next_content.is_empty() {
            rewritten_messages.get_or_insert_with(|| messages[..index].to_vec());
            continue;
        }
        if message_changed {
            let mut next_message = message.as_object().cloned().unwrap_or_default();
            next_message.insert(
                "content".to_owned(),
                Value::Array(next_content.into_iter().map(Cow::into_owned).collect()),
            );
            rewritten_messages
                .get_or_insert_with(|| messages[..index].to_vec())
                .push(Value::Object(next_message));
            continue;
        }
        if let Some(out) = rewritten_messages.as_mut() {
            out.push(message.clone());
        }
    }

    let rewritten_messages = rewritten_messages?;
    // Skip deep-copying the replaced `messages`; the insert keeps its key position.
    let mut out: Map<String, Value> = params
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                if key == "messages" {
                    Value::Null
                } else {
                    value.clone()
                },
            )
        })
        .collect();
    out.insert("messages".to_owned(), Value::Array(rewritten_messages));
    Some(out)
}
