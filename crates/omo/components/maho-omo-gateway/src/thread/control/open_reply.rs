//! Open-session reply helpers (`thread/control/open-reply.ts`).

use serde_json::{Map, Value, json};

use super::super::errors::thread_tool_failure;

fn non_empty<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub fn opened_engine_id(data: &Map<String, Value>, state: &Map<String, Value>) -> Option<String> {
    non_empty(state, "durableSessionId")
        .or_else(|| non_empty(data, "durableSessionId"))
        .or_else(|| non_empty(state, "sessionId"))
        .or_else(|| non_empty(data, "sessionId"))
        .map(str::to_owned)
}

pub fn model_refusal(text: &str, model: Option<&str>, device: Option<&str>) -> Option<Value> {
    let model = model?;
    if !text.contains("model_unavailable") {
        return None;
    }
    let where_str = match device {
        Some(d) => format!("device {d}"),
        None => "this device".to_owned(),
    };
    let mut details = json!({ "reason": "model_unavailable", "model": model });
    if let Some(d) = device
        && let Some(obj) = details.as_object_mut()
    {
        obj.insert("device".into(), Value::String(d.to_owned()));
    }
    Some(json!({
        "kind": "error",
        "error": thread_tool_failure(
            "model_not_found",
            format!("No thread was created: {where_str} cannot run {model}."),
            format!("Pass a provider/id {where_str} has (thread_set_model, tool.thread_set_model() in an eval cell, lists them), or leave model unset for its default."),
            Some(details),
        )
    }))
}
