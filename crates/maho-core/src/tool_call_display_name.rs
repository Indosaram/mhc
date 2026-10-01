//! Port of senpi packages/coding-agent/src/core/tool-call-display-name.ts.

use maho_agent::types::AgentEvent;
use maho_ai::types::{AssistantMessageEvent, ContentBlock};

/// A message_update as the session publishes it, carrying the resolved tool name for toolcall
/// start/end events so a client titles the call before tool_execution_start.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMessageUpdateEvent {
    pub event: AgentEvent,
    pub resolved_tool_name: Option<String>,
}

/// The tool name streamed by a toolcall_start/toolcall_end update, when present.
pub fn streamed_tool_call_name(event: &AgentEvent) -> Option<String> {
    let AgentEvent::MessageUpdate { assistant_message_event, .. } = event else { return None };
    match assistant_message_event {
        AssistantMessageEvent::ToolcallEnd { tool_call, .. } => Some(tool_call.name.clone()),
        AssistantMessageEvent::ToolcallStart { content_index, partial } => match partial.content.get(*content_index) {
            Some(ContentBlock::ToolCall(tool_call)) if !tool_call.name.is_empty() => Some(tool_call.name.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn with_resolved_tool_name(event: AgentEvent, resolve: impl Fn(&str) -> String) -> SessionMessageUpdateEvent {
    match streamed_tool_call_name(&event) {
        None => SessionMessageUpdateEvent { event, resolved_tool_name: None },
        Some(requested) => SessionMessageUpdateEvent { resolved_tool_name: Some(resolve(&requested)), event },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::types::AgentMessage;
    use serde_json::json;

    fn message() -> AgentMessage {
        AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(assistant_message(json!([])))))
    }

    fn assistant_message(content: serde_json::Value) -> maho_ai::types::AssistantMessage {
        serde_json::from_value(json!({
            "role": "assistant", "content": content, "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "pending", "timestamp": 0
        }))
        .expect("assistant message")
    }

    fn update(assistant_message_event: AssistantMessageEvent) -> AgentEvent {
        AgentEvent::MessageUpdate { message: message(), assistant_message_event }
    }

    #[test]
    fn a_toolcall_end_names_the_call() {
        let event = update(AssistantMessageEvent::ToolcallEnd {
            content_index: 0,
            tool_call: serde_json::from_value(json!({ "id": "1", "name": "mcp__x__Edit", "arguments": {} })).expect("tool call"),
            partial: assistant_message(json!([])),
        });
        assert_eq!(streamed_tool_call_name(&event).as_deref(), Some("mcp__x__Edit"));
    }

    #[test]
    fn a_toolcall_start_reads_the_partial_block() {
        let event = update(AssistantMessageEvent::ToolcallStart {
            content_index: 0,
            partial: assistant_message(json!([{ "type": "toolCall", "id": "1", "name": "edit", "arguments": {} }])),
        });
        assert_eq!(streamed_tool_call_name(&event).as_deref(), Some("edit"));
    }

    #[test]
    fn other_updates_have_no_name() {
        let event = update(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".into(),
            partial: assistant_message(json!([])),
        });
        assert!(streamed_tool_call_name(&event).is_none());
    }

    #[test]
    fn resolving_attaches_the_mapped_name_only_for_tool_calls() {
        let event = update(AssistantMessageEvent::ToolcallEnd {
            content_index: 0,
            tool_call: serde_json::from_value(json!({ "id": "1", "name": "mcp__x__Edit", "arguments": {} })).expect("tool call"),
            partial: assistant_message(json!([])),
        });
        let resolved =
            with_resolved_tool_name(event, |requested| requested.rsplit("__").next().unwrap_or(requested).to_lowercase());
        assert_eq!(resolved.resolved_tool_name.as_deref(), Some("edit"));
        let plain = with_resolved_tool_name(
            update(AssistantMessageEvent::TextStart { content_index: 0, partial: assistant_message(json!([])) }),
            |r| r.to_owned(),
        );
        assert!(plain.resolved_tool_name.is_none());
    }
}
