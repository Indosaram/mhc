//! Sign in with ChatGPT restrictions at the Responses adapter boundary.

use serde_json::{Map, Value, json};

fn tool_kind(tool: &Value) -> Option<&str> {
    tool.get("type").and_then(Value::as_str)
}

fn local_tool(tool: &Value) -> bool {
    matches!(tool_kind(tool), Some("function" | "custom"))
}

fn supported_choice(tool: &Value) -> bool {
    local_tool(tool) || tool_kind(tool) == Some("web_search")
}

/// Apply after payload hooks and tool-choice transformations, immediately before HTTP.
pub(super) fn shape_request(body: &mut Map<String, Value>) {
    body.insert("store".into(), json!(false));
    body.insert("stream".into(), json!(true));
    for key in [
        "background",
        "conversation",
        "max_output_tokens",
        "max_tool_calls",
        "metadata",
        "moderation",
        "multi_agent",
        "prompt",
        "prompt_cache_retention",
        "safety_identifier",
        "temperature",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
        "previous_response_id",
        "prompt_cache_options",
    ] {
        body.remove(key);
    }
    if let Some(input) = body.get_mut("input") {
        match input {
            Value::String(text) => *input = json!([{"role": "user", "content": text}]),
            Value::Array(items) => {
                for item in items.iter_mut().filter_map(Value::as_object_mut) {
                    if item.get("role").and_then(Value::as_str) == Some("system") {
                        item.insert("role".into(), json!("developer"));
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let mut eager = Vec::new();
        tools.retain_mut(|tool| match tool_kind(tool) {
            Some("function" | "custom") => {
                eager.push(std::mem::take(tool));
                false
            }
            Some("namespace") => {
                if let Some(nested) = tool.get_mut("tools").and_then(Value::as_array_mut) {
                    nested.retain(local_tool);
                }
                true
            }
            Some("web_search") => true,
            _ => false,
        });
        if !eager.is_empty() {
            let input = body.entry("input").or_insert_with(|| json!([]));
            if let Some(items) = input.as_array_mut() {
                items.insert(
                    0,
                    json!({"type":"additional_tools", "role":"developer", "tools":eager}),
                );
            }
        }
    }
    if let Some(choice) = body
        .get_mut("tool_choice")
        .filter(|choice| choice.is_object())
    {
        if tool_kind(choice) == Some("allowed_tools")
            && let Some(tools) = choice.get_mut("tools").and_then(Value::as_array_mut)
        {
            tools.retain(supported_choice);
            if tools.is_empty() {
                *choice = json!("none");
            }
        } else if !supported_choice(choice) {
            body.remove("tool_choice");
        }
    }
}

pub(super) fn unsupported_parameter(error: &Value) -> Option<String> {
    if error.get("code").and_then(Value::as_str)
        != Some("subscription_sharing_unsupported_capability")
    {
        return None;
    }
    error.get("param")?.as_str().map(str::to_owned)
}
