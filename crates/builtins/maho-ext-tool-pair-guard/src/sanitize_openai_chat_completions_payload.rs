//! Port of sanitize-openai-chat-completions-payload.ts.
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::BTreeSet;

const SYNTHETIC_OUTPUT: &str = "Tool output unavailable (interrupted before result)";

fn flush(pending: &mut Vec<String>, messages: &mut Vec<Cow<'_, Value>>) -> bool {
    let changed = !pending.is_empty();
    for id in pending.drain(..) {
        messages.push(Cow::Owned(json!({"role":"tool", "tool_call_id":id, "content":SYNTHETIC_OUTPUT})));
    }
    changed
}

pub fn sanitize_openai_chat_completions_payload(payload: &Value) -> Value {
    sanitize_openai_chat_completions_payload_if_changed(payload).unwrap_or_else(|| payload.clone())
}

/// `None` when the payload needs no change, so callers can skip copying it.
pub fn sanitize_openai_chat_completions_payload_if_changed(payload: &Value) -> Option<Value> {
    let messages = payload.get("messages").and_then(Value::as_array)?;
    let mut changed = false;
    let mut sanitized = Vec::new();
    let mut pending = Vec::new();
    let mut ids = BTreeSet::new();
    for message in messages {
        let calls = message.get("tool_calls").and_then(Value::as_array).filter(|calls| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && calls.iter().all(|call| call.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty()))
        });
        if let Some(calls) = calls {
            if flush(&mut pending, &mut sanitized) { ids.clear(); changed = true; }
            sanitized.push(Cow::Borrowed(message));
            for call in calls {
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    pending.push(id.to_owned());
                    ids.insert(id.to_owned());
                }
            }
            continue;
        }
        if message.get("role").and_then(Value::as_str) == Some("tool") {
            let id = message.get("tool_call_id").and_then(Value::as_str);
            if let Some(id) = id.filter(|id| !id.is_empty() && ids.contains(*id)) {
                sanitized.push(Cow::Borrowed(message));
                ids.remove(id);
                if let Some(index) = pending.iter().position(|candidate| candidate == id) { pending.remove(index); }
            } else { changed = true; }
            continue;
        }
        if flush(&mut pending, &mut sanitized) { ids.clear(); changed = true; }
        sanitized.push(Cow::Borrowed(message));
    }
    changed |= flush(&mut pending, &mut sanitized);
    if !changed { return None; }
    let mut result = payload.clone();
    result["messages"] = Value::Array(sanitized.into_iter().map(Cow::into_owned).collect());
    Some(result)
}
