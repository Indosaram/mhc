//! Port of senpi packages/agent/src/tool-arguments.ts.

use serde_json::Value;

use crate::types::{AgentTool, AgentToolCall, ToolArgumentShim};

/// The shim runs against a detached copy because several shims normalize by mutating the object
/// they were handed (eval's summary clamp, the harness edit shim's `edits` coercion) and return
/// that same reference. That object is the one the assistant message holds, so an in-place
/// normalization rewrites the answer the provider actually produced: the Claude SDK continuity
/// fingerprint then reports `assistant_rewritten` and the next turn re-sends the whole
/// conversation (senpi#1472). `validateToolArguments` already detaches for the same reason.
pub fn prepare_tool_arguments(shim: Option<&ToolArgumentShim>, args: Value) -> Value {
    match shim {
        None => args,
        Some(shim) => shim(args),
    }
}

pub fn prepare_agent_tool_call_arguments(tool: &AgentTool, tool_call: &AgentToolCall) -> AgentToolCall {
    match tool.prepare_arguments.as_ref() {
        None => tool_call.clone(),
        Some(shim) => {
            let prepared = prepare_tool_arguments(Some(shim), Value::Object(tool_call.arguments.clone()));
            let mut tool_call = tool_call.clone();
            if let Value::Object(arguments) = prepared {
                tool_call.arguments = arguments;
            }
            tool_call
        }
    }
}
