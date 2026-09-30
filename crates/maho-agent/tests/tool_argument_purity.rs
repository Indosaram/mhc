mod support;

use std::sync::Arc;

use maho_agent::{AgentTool, AgentToolCall, ToolArgumentShim, prepare_agent_tool_call_arguments};
use maho_ai::types::Tool;

use serde_json::{Value, json};

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "nested": { "type": "object", "properties": { "value": { "type": "string" } } },
        },
    })
}

fn in_place_normalizing_tool() -> AgentTool {
    let shim: ToolArgumentShim = Arc::new(|args: Value| {
        let mut args = args;
        if let Some(record) = args.as_object_mut() {
            if let Some(Value::String(summary)) = record.get_mut("summary") {
                *summary = summary.chars().take(3).collect();
            }
            if let Some(Value::Object(nested)) = record.get_mut("nested")
                && let Some(value) = nested.get_mut("value")
            {
                *value = Value::String("normalized".to_owned());
            }
        }
        args
    });
    let execute: support::ToolExecuteFn = Arc::new(|_id, _args, _signal, _on_update| {
        Box::pin(async { maho_agent::AgentToolResult::text("") })
    });
    AgentTool {
        label: "In-place normalizer".to_owned(),
        prepare_arguments: Some(shim),
        execute,
        replay: None,
        execution_mode: None,
        tool: Tool {
            name: "in_place_normalizer".to_owned(),
            description: "Normalizes its arguments in place and returns the same reference.".to_owned(),
            parameters: parameters(),
            freeform: None,
            constrained_sampling: None,
        },
    }
}

fn key_deleting_tool() -> AgentTool {
    let shim: ToolArgumentShim = Arc::new(|args: Value| {
        let mut args = args;
        if let Some(record) = args.as_object_mut() {
            record.remove("summary");
        }
        args
    });
    AgentTool { prepare_arguments: Some(shim), ..in_place_normalizing_tool() }
}

fn tool_call_with(args: Value) -> AgentToolCall {
    AgentToolCall {
        id: "call-1".to_owned(),
        name: "in_place_normalizer".to_owned(),
        arguments: args.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    }
}

#[test]
fn keeps_the_providers_arguments_when_the_shim_normalizes_in_place() {
    let tool_call = tool_call_with(json!({ "summary": "original summary", "nested": { "value": "original" } }));

    let prepared = prepare_agent_tool_call_arguments(&in_place_normalizing_tool(), &tool_call);

    assert_eq!(
        Value::Object(tool_call.arguments.clone()),
        json!({ "summary": "original summary", "nested": { "value": "original" } })
    );
    assert_eq!(
        Value::Object(prepared.arguments.clone()),
        json!({ "summary": "ori", "nested": { "value": "normalized" } })
    );
}

#[test]
fn keeps_the_providers_arguments_when_the_shim_deletes_a_key() {
    let tool_call = tool_call_with(json!({ "summary": "original summary" }));

    let prepared = prepare_agent_tool_call_arguments(&key_deleting_tool(), &tool_call);

    assert_eq!(Value::Object(tool_call.arguments.clone()), json!({ "summary": "original summary" }));
    assert_eq!(Value::Object(prepared.arguments.clone()), json!({}));
}

#[test]
fn keeps_nested_provider_arguments_when_the_shim_mutates_a_nested_object() {
    let tool_call = tool_call_with(json!({ "nested": { "value": "original" } }));

    prepare_agent_tool_call_arguments(&in_place_normalizing_tool(), &tool_call);

    assert_eq!(tool_call.arguments["nested"]["value"], json!("original"));
}

#[test]
fn leaves_a_tool_without_a_shim_on_its_original_arguments_object() {
    let tool_call = tool_call_with(json!({ "summary": "original summary" }));
    let without_shim = AgentTool { prepare_arguments: None, ..in_place_normalizing_tool() };

    let prepared = prepare_agent_tool_call_arguments(&without_shim, &tool_call);

    assert_eq!(prepared.arguments, tool_call.arguments);
    assert_eq!(prepared.id, tool_call.id);
}
