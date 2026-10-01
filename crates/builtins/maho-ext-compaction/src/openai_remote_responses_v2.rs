use std::collections::BTreeMap;
use maho_ai::types::Model;
use serde_json::Value;

pub fn with_remote_compaction_v2_header(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let key = headers.keys().find(|key| key.eq_ignore_ascii_case("x-codex-beta-features"));
    let mut tokens = Vec::new();
    if let Some(value) = key.and_then(|key| headers.get(key)) {
        for token in value.split(',').map(str::trim).filter(|token| !token.is_empty()) {
            if !tokens.contains(&token) { tokens.push(token); }
        }
    }
    if !tokens.contains(&"remote_compaction_v2") { tokens.push("remote_compaction_v2"); }
    let mut next = headers.clone();
    if let Some(key) = key { next.remove(key); }
    next.insert("x-codex-beta-features".into(), tokens.join(","));
    next
}

fn openai_hostname(model: &Model) -> bool {
    url::Url::parse(if model.base_url.is_empty() { "https://api.openai.com/v1" } else { &model.base_url }).is_ok_and(|url| url.host_str() == Some("api.openai.com"))
}

pub fn supports_openai_responses_remote_compaction_v2(model: &Model) -> bool {
    if let Some(value) = model.compat.as_ref().and_then(|compat| compat.get("supportsRemoteCompactionV2")).and_then(Value::as_bool) { return value; }
    model.provider == "openai" && openai_hostname(model)
}

pub fn supports_openai_responses_websocket(model: &Model) -> bool {
    if model.provider != "openai" || model.api != "openai-responses" { return false; }
    model.compat.as_ref().and_then(|compat| compat.get("supportsWebSocket")).and_then(Value::as_bool).unwrap_or_else(|| openai_hostname(model))
}

pub fn rewrite_v2_payload(payload: &Value, input: &[Value]) -> Option<Value> {
    let mut object = payload.as_object()?.clone();
    object.insert("input".into(), Value::Array(input.iter().cloned().chain(std::iter::once(serde_json::json!({"type":"compaction_trigger"}))).collect()));
    Some(Value::Object(object))
}

pub fn has_v2_trigger(payload: &Value) -> bool {
    payload.get("input").and_then(Value::as_array).is_some_and(|items| items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("compaction_trigger")))
}
