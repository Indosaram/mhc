//! Port of sanitize-openai-responses-payload.ts.
use serde_json::{Value, json};
use std::collections::BTreeSet;

const SYNTHETIC_OUTPUT: &str = "Tool output unavailable (interrupted before result)";

fn call_id(item: &Value) -> Option<&str> {
    item.get("call_id").and_then(Value::as_str).filter(|id| !id.is_empty())
}

pub fn sanitize_openai_responses_payload(payload: &Value) -> Value {
    let Some(input) = payload.get("input").and_then(Value::as_array) else { return payload.clone() };
    if payload.get("previous_response_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty()) { return payload.clone(); }
    let mut seen_functions = BTreeSet::new();
    let mut seen_custom = BTreeSet::new();
    let mut function_outputs = BTreeSet::new();
    let mut custom_outputs = BTreeSet::new();
    for item in input {
        let Some(id) = call_id(item) else { continue };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call" | "local_shell_call") => { seen_functions.insert(id); }
            Some("custom_tool_call") => { seen_custom.insert(id); }
            Some("function_call_output") if seen_functions.contains(id) => { function_outputs.insert(id); }
            Some("custom_tool_call_output") if seen_custom.contains(id) => { custom_outputs.insert(id); }
            _ => {}
        }
    }
    let mut emitted_functions = BTreeSet::new();
    let mut emitted_custom = BTreeSet::new();
    let mut sanitized = Vec::new();
    let mut changed = false;
    for item in input {
        let id = call_id(item);
        match item.get("type").and_then(Value::as_str) {
            Some("function_call_output") => {
                if id.is_some_and(|id| function_outputs.contains(id) && emitted_functions.insert(id)) { sanitized.push(item.clone()); }
                else { changed = true; }
            }
            Some("custom_tool_call_output") => {
                if id.is_some_and(|id| custom_outputs.contains(id) && emitted_custom.insert(id)) { sanitized.push(item.clone()); }
                else { changed = true; }
            }
            kind => {
                sanitized.push(item.clone());
                if let Some(id) = id {
                    if matches!(kind, Some("function_call" | "local_shell_call")) && !function_outputs.contains(id) {
                        sanitized.push(json!({"type":"function_call_output", "call_id":id, "output":SYNTHETIC_OUTPUT}));
                        changed = true;
                    } else if kind == Some("custom_tool_call") && !custom_outputs.contains(id) {
                        let mut output = json!({"type":"custom_tool_call_output", "call_id":id, "output":SYNTHETIC_OUTPUT});
                        if let Some(name) = item.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()) { output["name"] = json!(name); }
                        sanitized.push(output);
                        changed = true;
                    }
                }
            }
        }
    }
    if !changed { return payload.clone(); }
    let mut result = payload.clone();
    result["input"] = Value::Array(sanitized);
    result
}
