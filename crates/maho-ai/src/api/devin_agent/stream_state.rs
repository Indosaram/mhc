//! Port of senpi packages/ai/src/api/devin-agent/stream-state.ts.
//!
//! Cascade delta bookkeeping. One Cascade response frame can carry a thinking delta, a text delta
//! and a finished tool call at once, and blocks are implicit: the server never opens or closes them.
//! This module owns the open-block state so the adapter emits senpi's start/delta/end triples in the
//! right order.

use std::collections::HashMap;

use crate::api::devin_agent::r#gen::cascade_pb::{ChatToolCall, GetChatMessageResponse, StopReason};
use crate::types::{AssistantMessage, ContentBlock, StopReason as MessageStopReason, ToolCall};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::json_parse::parse_streaming_json;

/// `DevinToolCallState`.
pub struct DevinToolCallState {
    pub content_index: usize,
    /// Raw JSON accumulated across chunks; the content block holds the parsed view.
    pub arguments_json: String,
}

/// `DevinStreamState`.
#[derive(Default)]
pub struct DevinStreamState {
    pub text_index: Option<usize>,
    pub thinking_index: Option<usize>,
    pub tool_calls: HashMap<String, DevinToolCallState>,
    /// Chunks after the first arrive without an id; they belong to this call.
    pub active_tool_call_id: Option<String>,
}

/// `createDevinStreamState`.
pub fn create_devin_stream_state() -> DevinStreamState {
    DevinStreamState::default()
}

/// `applyDevinResponse`.
pub fn apply_devin_response(
    message: &GetChatMessageResponse,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
    state: &mut DevinStreamState,
) {
    if !message.message_id.is_empty() && output.response_id.is_none() {
        output.response_id = Some(message.message_id.clone());
    }
    if let Some(actual_model_uid) = message.actual_model_uid.as_ref() {
        output.response_model = Some(actual_model_uid.clone());
    }

    if !message.delta_thinking.is_empty() {
        append_thinking(message, output, events, state);
    }
    if !message.delta_text.is_empty() {
        append_text(&message.delta_text, output, events, state);
    }
    for call in &message.delta_tool_calls {
        append_tool_call(call, output, events, state);
    }
    if let Some(usage) = message.usage.as_ref() {
        output.usage.input = usage.input_tokens;
        output.usage.output = usage.output_tokens;
        output.usage.cache_read = usage.cache_read_tokens;
        output.usage.cache_write = usage.cache_write_tokens;
        output.usage.total_tokens =
            output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write;
    }
    if message.stop_reason != StopReason::Unspecified as i32
        && let Ok(reason) = StopReason::try_from(message.stop_reason)
    {
        output.stop_reason = map_stop_reason(reason);
    }
}

fn append_thinking(
    message: &GetChatMessageResponse,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
    state: &mut DevinStreamState,
) {
    // A thinking block interleaved after text starts a new block, matching the order Cascade
    // streamed it in.
    if state.text_index.is_some() {
        close_text(output, events, state);
    }
    if state.thinking_index.is_none() {
        output.content.push(ContentBlock::Thinking(crate::types::ThinkingContent {
            thinking: String::new(),
            started_at: None,
            ended_at: None,
            thinking_signature: None,
            redacted: message.thinking_redacted.then_some(true),
        }));
        let index = output.content.len() - 1;
        state.thinking_index = Some(index);
        events.push(crate::types::AssistantMessageEvent::ThinkingStart {
            content_index: index,
            partial: output.clone(),
        });
    }
    let index = state.thinking_index.expect("thinking index is set");
    let ContentBlock::Thinking(block) = &mut output.content[index] else { return };
    block.thinking.push_str(&message.delta_thinking);
    if !message.delta_signature.is_empty() {
        block.thinking_signature = Some(message.delta_signature.clone());
    }
    events.push(crate::types::AssistantMessageEvent::ThinkingDelta {
        content_index: index,
        delta: message.delta_thinking.clone(),
        partial: output.clone(),
    });
}

fn append_text(
    delta: &str,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
    state: &mut DevinStreamState,
) {
    if state.thinking_index.is_some() {
        close_thinking(output, events, state);
    }
    if state.text_index.is_none() {
        output.content.push(ContentBlock::Text(crate::types::TextContent {
            text: String::new(),
            audience: None,
            text_signature: None,
        }));
        let index = output.content.len() - 1;
        state.text_index = Some(index);
        events.push(crate::types::AssistantMessageEvent::TextStart { content_index: index, partial: output.clone() });
    }
    let index = state.text_index.expect("text index is set");
    let ContentBlock::Text(block) = &mut output.content[index] else { return };
    block.text.push_str(delta);
    events.push(crate::types::AssistantMessageEvent::TextDelta {
        content_index: index,
        delta: delta.to_owned(),
        partial: output.clone(),
    });
}

fn append_tool_call(
    call: &ChatToolCall,
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
    state: &mut DevinStreamState,
) {
    if state.text_index.is_some() {
        close_text(output, events, state);
    }
    if state.thinking_index.is_some() {
        close_thinking(output, events, state);
    }
    let tool_call_id = if !call.id.is_empty() { Some(call.id.clone()) } else { state.active_tool_call_id.clone() };
    let Some(tool_call_id) = tool_call_id else { return };
    state.active_tool_call_id = Some(tool_call_id.clone());
    if !state.tool_calls.contains_key(&tool_call_id) {
        output.content.push(ContentBlock::ToolCall(ToolCall {
            id: tool_call_id.clone(),
            name: call.name.clone(),
            arguments: serde_json::Map::new(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }));
        let content_index = output.content.len() - 1;
        state.tool_calls.insert(tool_call_id.clone(), DevinToolCallState { content_index, arguments_json: String::new() });
        events.push(crate::types::AssistantMessageEvent::ToolcallStart { content_index, partial: output.clone() });
    }
    let entry = state.tool_calls.get_mut(&tool_call_id).expect("tool call entry exists");
    let content_index = entry.content_index;
    let ContentBlock::ToolCall(block) = &mut output.content[content_index] else { return };
    if !call.name.is_empty() {
        block.name = call.name.clone();
    }
    if call.arguments_json.is_empty() {
        return;
    }
    // A chunk either repeats everything so far plus new bytes, or carries only the new bytes.
    let accumulated = if call.arguments_json.starts_with(&entry.arguments_json) {
        call.arguments_json.clone()
    } else {
        format!("{}{}", entry.arguments_json, call.arguments_json)
    };
    let delta = accumulated[entry.arguments_json.len()..].to_owned();
    entry.arguments_json = accumulated;
    let arguments = parse_streaming_json(Some(&entry.arguments_json));
    block.arguments = arguments.as_object().cloned().unwrap_or_default();
    if !delta.is_empty() {
        events.push(crate::types::AssistantMessageEvent::ToolcallDelta {
            content_index,
            delta,
            partial: output.clone(),
        });
    }
}

fn close_text(output: &mut AssistantMessage, events: &AssistantMessageEventStream, state: &mut DevinStreamState) {
    let Some(index) = state.text_index.take() else { return };
    if let ContentBlock::Text(block) = &output.content[index] {
        events.push(crate::types::AssistantMessageEvent::TextEnd {
            content_index: index,
            content: block.text.clone(),
            partial: output.clone(),
        });
    }
}

fn close_thinking(output: &mut AssistantMessage, events: &AssistantMessageEventStream, state: &mut DevinStreamState) {
    let Some(index) = state.thinking_index.take() else { return };
    if let ContentBlock::Thinking(block) = &output.content[index] {
        events.push(crate::types::AssistantMessageEvent::ThinkingEnd {
            content_index: index,
            content: block.thinking.clone(),
            partial: output.clone(),
        });
    }
}

/// `finalizeBlocks`.
pub fn finalize_blocks(
    output: &mut AssistantMessage,
    events: &AssistantMessageEventStream,
    state: &mut DevinStreamState,
) {
    if let Some(index) = state.thinking_index.take()
        && let ContentBlock::Thinking(block) = &output.content[index]
    {
        events.push(crate::types::AssistantMessageEvent::ThinkingEnd {
            content_index: index,
            content: block.thinking.clone(),
            partial: output.clone(),
        });
    }
    if let Some(index) = state.text_index.take()
        && let ContentBlock::Text(block) = &output.content[index]
    {
        events.push(crate::types::AssistantMessageEvent::TextEnd {
            content_index: index,
            content: block.text.clone(),
            partial: output.clone(),
        });
    }
    for (_, entry) in state.tool_calls.iter() {
        if let ContentBlock::ToolCall(block) = &output.content[entry.content_index] {
            events.push(crate::types::AssistantMessageEvent::ToolcallEnd {
                content_index: entry.content_index,
                tool_call: block.clone(),
                partial: output.clone(),
            });
        }
    }
    state.tool_calls.clear();
}

/// Cascade's stop vocabulary reduced to senpi's.
fn map_stop_reason(reason: StopReason) -> MessageStopReason {
    match reason {
        StopReason::FunctionCall => MessageStopReason::ToolUse,
        StopReason::MaxTokens | StopReason::MaxNewlines => MessageStopReason::Length,
        StopReason::Error | StopReason::ContentFilter => MessageStopReason::Error,
        _ => MessageStopReason::Stop,
    }
}
