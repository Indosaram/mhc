//! Port of senpi packages/agent/src/tool-arguments.ts.

use serde_json::Value;

use crate::types::{AgentTool, AgentToolCall, ToolArgumentShim};

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
            let tool = AgentTool {
                prepare_arguments: Some(shim),
                ..Default::default()
            };
            let mut original = call();
            original.name = "test-tool".into();
            let result = prepare_agent_tool_call_arguments(&tool, &original);
            if kind == "object" {
                let prepared = result.unwrap();
                assert_eq!(Value::Object(prepared.arguments), json!({"normalized": true}));
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
        let tool = AgentTool {
            prepare_arguments: Some(shim),
            ..Default::default()
        };
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
