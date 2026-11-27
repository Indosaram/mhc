//! Port of senpi `packages/agent/src/execution/tool-outcome-events.ts`.

use std::sync::Arc;

use maho_ai::types::{BoxFuture, ContentBlock, Message, ToolResultMessage};
use serde_json::Value;

use crate::assistant_terminal_state::now_ms;
use crate::types::{
    AgentEvent, AgentMessage, AgentToolCall, AgentToolCallOutcome, AgentToolResult,
};

pub type EventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

pub fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(message)],
        details: Value::Object(Default::default()),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

pub fn emit_tool_execution_update(
    tool_call: AgentToolCall,
    emit: EventSink,
) -> crate::types::AgentToolUpdateCallback {
    let args = Value::Object(tool_call.arguments.clone());
    Arc::new(move |partial_result: AgentToolResult| {
        let emit = Arc::clone(&emit);
        let id = tool_call.id.clone();
        let name = tool_call.name.clone();
        let args = args.clone();
        let partial = serde_json::to_value(&partial_result).unwrap_or(Value::Null);
        let fut = emit(AgentEvent::ToolExecutionUpdate {
            parent_tool_call_id: None,
            tool_call_id: id,
            tool_name: name,
            args,
            partial_result: partial,
        });
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(fut);
        }
    })
}

pub async fn emit_tool_execution_end(finalized: &AgentToolCallOutcome, emit: &EventSink) {
    emit(AgentEvent::ToolExecutionEnd {
        parent_tool_call_id: None,
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: serde_json::to_value(&finalized.result).unwrap_or(Value::Null),
        is_error: finalized.is_error,
    })
    .await;
}

pub fn create_tool_result_message(finalized: &AgentToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: Some(serde_json::to_value(&finalized.result.details).unwrap_or(Value::Null)),
        usage: finalized.result.usage,
        nested_calls: finalized.result.nested_calls.clone(),
        added_tool_names: match finalized.result.added_tool_names.as_ref() {
            Some(names) if !names.is_empty() => Some(names.clone()),
            _ => None,
        },
        is_error: finalized.is_error,
        timestamp: now_ms(),
    }
}

pub async fn emit_tool_result_message(tool_result_message: &ToolResultMessage, emit: &EventSink) {
    let message = AgentMessage::Llm(Message::ToolResult(tool_result_message.clone()));
    emit(AgentEvent::MessageStart {
        message: message.clone(),
        streaming: None,
    })
    .await;
    emit(AgentEvent::MessageEnd { message }).await;
}

pub fn should_terminate_tool_batch(finalized_calls: &[AgentToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}
