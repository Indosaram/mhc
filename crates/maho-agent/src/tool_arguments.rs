//! Port of senpi packages/agent/src/tool-arguments.ts.

use serde_json::{Map, Value};

use crate::types::{AgentTool, AgentToolCall, ToolArgumentShim};

fn schema_types(schema: &Map<String, Value>) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn accepted_json_types(schema: &Value) -> Option<Vec<String>> {
    let obj = schema.as_object()?;
    let types = schema_types(obj);
    if !types.is_empty() {
        return Some(types.into_iter().map(str::to_owned).collect());
    }
    let members = obj
        .get("anyOf")
        .or_else(|| obj.get("oneOf"))
        .and_then(Value::as_array)?;
    if members.is_empty() {
        return None;
    }
    let mut accepted = Vec::new();
    for member in members {
        let member_types = accepted_json_types(member)?;
        for t in member_types {
            if !accepted.contains(&t) {
                accepted.push(t);
            }
        }
    }
    Some(accepted)
}

fn decode_value_against_schema(value: Value, schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return value;
    };
    if let Value::String(ref text) = value {
        let Some(accepted) = accepted_json_types(schema) else {
            return value;
        };
        if accepted.iter().any(|t| t == "string") {
            return value;
        }
        let trimmed = text.trim();
        let wants_array = accepted.iter().any(|t| t == "array") && trimmed.starts_with('[');
        let wants_object = accepted.iter().any(|t| t == "object") && trimmed.starts_with('{');
        if (wants_array || wants_object)
            && let Ok(parsed) = serde_json::from_str::<Value>(trimmed)
            && ((wants_array && parsed.is_array()) || (wants_object && parsed.is_object()))
        {
            return decode_value_against_schema(parsed, schema);
        }
        return value;
    }
    let types = schema_types(obj);
    match value {
        Value::Array(items) if types.contains(&"array") => Value::Array(match obj.get("items") {
            Some(Value::Array(tuple)) => items
                .into_iter()
                .enumerate()
                .map(|(i, item)| match tuple.get(i) {
                    Some(s) => decode_value_against_schema(item, s),
                    None => item,
                })
                .collect(),
            Some(item_schema) if item_schema.is_object() => items
                .into_iter()
                .map(|item| decode_value_against_schema(item, item_schema))
                .collect(),
            _ => items,
        }),
        Value::Object(mut record) if types.contains(&"object") => {
            let props = obj.get("properties").and_then(Value::as_object);
            let additional = obj.get("additionalProperties").filter(|v| v.is_object());
            for (k, slot) in &mut record {
                let prop_schema = props.and_then(|p| p.get(k)).or(additional);
                if let Some(s) = prop_schema {
                    let taken = slot.take();
                    *slot = decode_value_against_schema(taken, s);
                }
            }
            Value::Object(record)
        }
        other => other,
    }
}

/// Decodes JSON-stringified array/object tool call arguments when the declared JSON schema expects
/// an array or object rather than a string (`senpi#6265`).
pub fn decode_json_encoded_structures(arguments: &mut Map<String, Value>, schema: &Value) {
    let taken = std::mem::take(arguments);
    if let Value::Object(decoded) = decode_value_against_schema(Value::Object(taken), schema) {
        *arguments = decoded;
    }
}

/// The shim runs against a detached copy because several shims normalize by mutating the object
/// they were handed (eval's summary clamp, the harness edit shim's `edits` coercion) and return
/// that same reference. That object is the one the assistant message holds, so an in-place
/// normalization rewrites the answer the provider actually produced: the Claude SDK continuity
/// fingerprint then reports `assistant_rewritten` and the next turn re-sends the whole
/// conversation (senpi#1472). `validateToolArguments` already detaches for the same reason.
pub fn prepare_tool_arguments(
    shim: Option<&ToolArgumentShim>,
    args: Value,
) -> Result<Value, String> {
    match shim {
        None => Ok(args),
        Some(shim) => shim(args),
    }
}

pub fn prepare_agent_tool_call_arguments(
    tool: &AgentTool,
    tool_call: &AgentToolCall,
) -> Result<AgentToolCall, String> {
    match tool.prepare_arguments.as_ref() {
        None => Ok(tool_call.clone()),
        Some(shim) => {
            let prepared =
                prepare_tool_arguments(Some(shim), Value::Object(tool_call.arguments.clone()))?;
            let mut tool_call = tool_call.clone();
            tool_call.arguments = match prepared {
                Value::Object(arguments) => arguments,
                other => {
                    let kind = match other {
                        Value::Array(_) => "array",
                        Value::String(_) => "string",
                        Value::Number(_) => "number",
                        Value::Bool(_) => "boolean",
                        Value::Null => "null",
                        Value::Object(_) => unreachable!(),
                    };
                    // SAFETY-DIVERGENCE(packages/agent/src/tool-arguments.ts:17-23): upstream's unchecked cast leaves non-objects in place, where downstream reads yield undefined; mhc returns a visible error instead of silently losing arguments.
                    return Err(format!(
                        "tool argument shim for `{}` returned {kind}, expected an object",
                        tool_call.name
                    ));
                }
            };
            Ok(tool_call)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_agent_tool_call_arguments, prepare_tool_arguments};
    use crate::types::{AgentTool, AgentToolCall, ToolArgumentShim};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn call() -> AgentToolCall {
        AgentToolCall {
            id: "call-1".into(),
            name: "tool".into(),
            arguments: json!({"original": true}).as_object().unwrap().clone(),
            ..Default::default()
        }
    }

    /// `AgentTool` has no `Default` (it carries a label, an executor and the provider-facing
    /// definition), so a shim-only tool is spelled out here instead of spread from a default.
    fn tool_with(shim: Option<ToolArgumentShim>) -> AgentTool {
        let execute: crate::types::AgentToolExecute =
            Arc::new(|_id, _args, _signal, _on_update| {
                Box::pin(async { crate::types::AgentToolResult::text("") })
            });
        AgentTool {
            label: "Test tool".into(),
            prepare_arguments: shim,
            execute,
            replay: None,
            execution_mode: None,
            tool: maho_ai::types::Tool {
                name: "test-tool".into(),
                description: "Exercises the argument shim path.".into(),
                parameters: json!({"type": "object"}),
                output_schema: None,
                freeform: None,
                constrained_sampling: None,
            },
        }
    }

    #[test]
    fn shim_output_class_matches_upstream() {
        let classes = [
            (json!({"normalized": true}), "object"),
            (json!([1, 2]), "array"),
            (json!("text"), "string"),
            (json!(3), "number"),
            (json!(false), "boolean"),
            (Value::Null, "null"),
        ];
        for (shim_output, kind) in classes {
            let shim: ToolArgumentShim = Arc::new(move |_| Ok(shim_output.clone()));
            let tool = tool_with(Some(shim));
            let mut original = call();
            original.name = "test-tool".into();
            let result = prepare_agent_tool_call_arguments(&tool, &original);
            if kind == "object" {
                let prepared = result.unwrap();
                assert_eq!(
                    Value::Object(prepared.arguments),
                    json!({"normalized": true})
                );
            } else {
                let error = result.unwrap_err();
                assert!(error.contains("test-tool"), "{error}");
                assert!(error.contains(kind), "{error}");
            }
        }
    }

    #[test]
    fn mutating_shim_does_not_alter_original_tool_call() {
        let shim: ToolArgumentShim = Arc::new(|mut args| {
            args["original"] = json!(false);
            Ok(args)
        });
        let tool = tool_with(Some(shim));
        let original = call();
        let _prepared = prepare_agent_tool_call_arguments(&tool, &original).unwrap();
        assert_eq!(original.arguments["original"], json!(true));
    }

    #[test]
    fn preserves_prepared_non_object_values_until_call_shape_conversion() {
        let shim: ToolArgumentShim = Arc::new(|_| Ok(json!("normalized")));
        assert_eq!(
            prepare_tool_arguments(Some(&shim), json!({"input": true})),
            Ok(Value::String("normalized".into()))
        );
    }
}
