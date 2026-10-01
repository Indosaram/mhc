//! Port of senpi packages/ai/src/api/anthropic-tool-references.ts.

use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::utils::unavailable_tool_text::{demoted_tool_call_text, demoted_tool_result_text};

static GATEWAY_TOOL_NAMESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^mcp__[^_]+__(.+)$").unwrap_or_else(|error| panic!("{error}")));

struct AvailableToolNames {
    defined: IndexSet<String>,
    folded: IndexMap<String, String>,
}

fn fold_tool_name_key(name: &str) -> String {
    name.to_lowercase().replace(['-', '_'], "")
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
    let mut folded: IndexMap<String, String> = IndexMap::new();
    let mut ambiguous: IndexSet<String> = IndexSet::new();
    for name in &defined {
        let key = fold_tool_name_key(name);
        if folded.contains_key(&key) {
            ambiguous.insert(key);
        } else {
            folded.insert(key, name.clone());
        }
    }
    for key in &ambiguous {
        folded.shift_remove(key);
    }
    AvailableToolNames { defined, folded }
}

fn resolve_available_tool_name(name: &str, available: &AvailableToolNames) -> Option<String> {
    let suffix = GATEWAY_TOOL_NAMESPACE.captures(name).map(|captures| captures[1].to_owned());
    let candidates: Vec<&str> = match &suffix {
        Some(suffix) => vec![name, suffix.as_str()],
        None => vec![name],
    };
    for candidate in &candidates {
        if available.defined.contains(*candidate) {
            return Some((*candidate).to_owned());
        }
    }
    for candidate in &candidates {
        if let Some(folded) = available.folded.get(&fold_tool_name_key(candidate)) {
            return Some(folded.clone());
        }
    }
    None
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
                .map(|item| item.get("tool_name").and_then(Value::as_str).unwrap_or_default().to_owned())
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
                item.get("type").and_then(Value::as_str).map(|kind| format!("[{kind}]"))
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

fn rewrite_tool_reference_items(items: &[Value], available: &AvailableToolNames) -> Option<Rewrite> {
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
    if rewritten { Some(Rewrite { kept, omitted }) } else { None }
}

pub fn demote_unavailable_tool_references(params: &Map<String, Value>) -> Map<String, Value> {
    let Some(messages) = params.get("messages").and_then(Value::as_array).cloned() else {
        return params.clone();
    };
    if messages.is_empty() {
        return params.clone();
    }

    let available = collect_available_tool_names(params.get("tools"));
    let resolve = |name: &str| resolve_available_tool_name(name, &available);

    let mut demoted_call_names: IndexMap<String, String> = IndexMap::new();
    let mut renamed_call_names: IndexMap<String, String> = IndexMap::new();
    for message in &messages {
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
                    demoted_call_names.insert(key_of(&block.get("id").cloned().unwrap_or(Value::Null)), name.to_owned());
                }
                Some(resolved) if resolved != name => {
                    renamed_call_names.insert(key_of(&block.get("id").cloned().unwrap_or(Value::Null)), resolved);
                }
                Some(_) => {}
            }
        }
    }

    let mut changed = false;
    let available_tool_names: Vec<String> = available.defined.iter().cloned().collect();
    let mut seen_demoted_call_names: IndexSet<String> = IndexSet::new();
    let mut rewritten_messages: Vec<Value> = Vec::new();

    for message in &messages {
        let Some(content) = message.get("content").and_then(Value::as_array).cloned() else {
            rewritten_messages.push(message.clone());
            continue;
        };
        let is_assistant = message.get("role").and_then(Value::as_str) == Some("assistant");
        let mut message_changed = false;

        let mut dropped_search_use_ids: IndexSet<String> = IndexSet::new();
        let mut dropped_search_names: IndexMap<String, Vec<String>> = IndexMap::new();
        if is_assistant {
            for block in &content {
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

        let mut next_content: Vec<Value> = Vec::new();
        for block in &content {
            if is_assistant && block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let key = key_of(block.get("id").unwrap_or(&Value::Null));
                if let Some(name) = demoted_call_names.get(&key) {
                    message_changed = true;
                    let first_occurrence = seen_demoted_call_names.insert(name.clone());
                    next_content.push(json!({
                        "type": "text",
                        "text": demoted_tool_call_text(name, &available_tool_names, first_occurrence),
                    }));
                    continue;
                }
                if let Some(name) = renamed_call_names.get(&key) {
                    message_changed = true;
                    let mut renamed = block.as_object().cloned().unwrap_or_default();
                    renamed.insert("name".to_owned(), Value::String(name.clone()));
                    next_content.push(Value::Object(renamed));
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
                    next_content.push(json!({
                        "type": "text",
                        "text": format!("Tool reference unavailable: {}", dedupe_preserving_order(omitted).join(", ")),
                    }));
                    continue;
                }
                let references = block
                    .get("content")
                    .and_then(|content| content.get("tool_references"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(rewritten) = rewrite_tool_reference_items(&references, &available) {
                    message_changed = true;
                    let mut next_block = block.as_object().cloned().unwrap_or_default();
                    let mut next_content_map = next_block
                        .get("content")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_else(Map::new);
                    next_content_map.insert("tool_references".to_owned(), Value::Array(rewritten.kept));
                    next_block.insert("content".to_owned(), Value::Object(next_content_map));
                    next_content.push(Value::Object(next_block));
                    continue;
                }
            }
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let key = key_of(block.get("tool_use_id").unwrap_or(&Value::Null));
                if let Some(name) = demoted_call_names.get(&key) {
                    message_changed = true;
                    let text = demoted_tool_result_text(name, &tool_result_text(block.get("content").unwrap_or(&Value::Null)));
                    next_content.push(json!({ "type": "text", "text": text }));
                    continue;
                }
                if let Some(items) = block.get("content").and_then(Value::as_array).cloned()
                    && let Some(rewritten) = rewrite_tool_reference_items(&items, &available)
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
                    next_content.push(Value::Object(next_block));
                    continue;
                }
            }
            next_content.push(block.clone());
        }

        if next_content.is_empty() {
            changed = true;
            continue;
        }
        if message_changed {
            changed = true;
            let mut next_message = message.as_object().cloned().unwrap_or_default();
            next_message.insert("content".to_owned(), Value::Array(next_content));
            rewritten_messages.push(Value::Object(next_message));
            continue;
        }
        rewritten_messages.push(message.clone());
    }

    if !changed {
        return params.clone();
    }
    let mut out = params.clone();
    out.insert("messages".to_owned(), Value::Array(rewritten_messages));
    out
}
