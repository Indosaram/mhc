//! Port of senpi packages/ai/src/utils/assistant-capture-record.ts.

use std::sync::{Arc, LazyLock};

use serde_json::{Map, Value};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, Usage};

use super::assistant_capture::{
    AssistantCapture, CaptureHint, CapturePosition, CaptureState, event_content_index, utf16_len,
};
use super::assistant_seed_frontier::{SeedFrontierError, seed_next_offset};

static NO_CHANGE: LazyLock<Arc<Map<String, Value>>> = LazyLock::new(|| Arc::new(Map::new()));

/// A deep, immutable, shared copy (TS `structuredClone` + deep freeze).
pub fn freeze_capture<T: Clone>(value: &T) -> Arc<T> {
    Arc::new(value.clone())
}

/// Only declared public fields survive; provider scratch never enters a seed. The Rust content
/// block types already hold only their declared public fields, so this is a copy.
pub fn public_block(block: &ContentBlock) -> ContentBlock {
    block.clone()
}

/// Structural equality over plain data.
pub fn same_value<T: PartialEq>(left: &T, right: &T) -> bool {
    left == right
}

/// The message's scalar fields (everything but `content` and `usage`) as TS object keys.
fn message_fields(message: &AssistantMessage) -> Map<String, Value> {
    let mut fields = match serde_json::to_value(message) {
        Ok(Value::Object(fields)) => fields,
        _ => Map::new(),
    };
    fields.remove("content");
    fields.remove("usage");
    fields
}

/// A terminal replaced downstream: its usage and message, plus every scalar that differs from the
/// source terminal.
pub fn terminal_capture(
    capture: &AssistantCapture,
    terminal: &AssistantMessage,
) -> AssistantCapture {
    let mut metadata = capture.metadata.as_deref().cloned().unwrap_or_default();
    let previous = capture
        .message
        .as_deref()
        .map(message_fields)
        .unwrap_or_default();
    let current = message_fields(terminal);
    for key in previous.keys().chain(current.keys()) {
        let value = current.get(key);
        if value != previous.get(key) {
            metadata.insert(key.clone(), value.cloned().unwrap_or(Value::Null));
        }
    }
    AssistantCapture {
        usage: terminal.usage,
        metadata: Some(Arc::new(metadata)),
        message: Some(Arc::new(public_message(terminal))),
        rewritten_from: capture.rewritten_from,
        ..AssistantCapture::default()
    }
}

fn public_message(message: &AssistantMessage) -> AssistantMessage {
    let mut copy = message.clone();
    copy.content = message.content.iter().map(public_block).collect();
    copy
}

fn event_message(event: &AssistantMessageEvent) -> &AssistantMessage {
    match event {
        AssistantMessageEvent::Done { message, .. } => message,
        AssistantMessageEvent::Error { error, .. } => error,
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => partial,
    }
}

fn event_delta(event: &AssistantMessageEvent) -> Option<&str> {
    match event {
        AssistantMessageEvent::TextDelta { delta, .. }
        | AssistantMessageEvent::ThinkingDelta { delta, .. }
        | AssistantMessageEvent::ToolcallDelta { delta, .. } => Some(delta),
        _ => None,
    }
}

pub fn build_capture(
    state: &mut CaptureState,
    event: &AssistantMessageEvent,
    hint: &CaptureHint,
) -> Result<Option<AssistantCapture>, SeedFrontierError> {
    let message = event_message(event);
    let usage = usage_copy(state, &message.usage);
    let Some(event_index) = event_content_index(event) else {
        return Ok(Some(AssistantCapture {
            usage,
            metadata: Some(changed_metadata(state, message)),
            message: Some(Arc::new(public_message(message))),
            rewritten_from: hint.rewritten_from,
            ..AssistantCapture::default()
        }));
    };
    let index = hint.content_index.unwrap_or(event_index);
    let own = (index != event_index).then_some(index);
    let carried = hint.carried.as_ref().and_then(Option::as_ref);
    let is_toolcall_delta = matches!(event, AssistantMessageEvent::ToolcallDelta { .. });
    if let Some(delta) = event_delta(event) {
        if let Some(carried) = carried {
            return Ok(Some(AssistantCapture {
                usage,
                offset: carried.offset,
                parsed_through: carried.parsed_through,
                ..AssistantCapture::default()
            }));
        }
        let mut position = state.positions.get(&index).copied().unwrap_or_default();
        let offset = match hint.offset {
            Some(offset) => usize::try_from(offset).unwrap_or(0),
            None => position.offset,
        };
        let delta_length = utf16_len(delta);
        position.offset = offset + delta_length;
        if is_toolcall_delta && delta_length > 0 {
            position.parsed_through = hint
                .parsed_through
                .unwrap_or_else(|| position.parsed_through.max(position.offset));
        }
        state.positions.insert(index, position);
        return Ok(Some(AssistantCapture {
            usage,
            offset: Some(offset),
            content_index: own,
            parsed_through: is_toolcall_delta.then_some(position.parsed_through),
            ..AssistantCapture::default()
        }));
    }
    let metadata = changed_metadata(state, message);
    if let Some(carried) = carried {
        return Ok(Some(AssistantCapture {
            usage,
            metadata: Some(metadata),
            message: None,
            offset: None,
            ..(**carried).clone()
        }));
    }
    let starts = matches!(
        event,
        AssistantMessageEvent::TextStart { .. }
            | AssistantMessageEvent::ThinkingStart { .. }
            | AssistantMessageEvent::ToolcallStart { .. }
    );
    let source = match event {
        AssistantMessageEvent::ToolcallEnd { tool_call, .. } => {
            Some(ContentBlock::ToolCall(tool_call.clone()))
        }
        _ => message.content.get(index).cloned(),
    };
    // A start announced before its block is appended has no seed to freeze yet.
    let Some(source) = source else {
        return Ok(None);
    };
    let block = freeze_capture(&public_block(&source));
    if !starts {
        return Ok(Some(AssistantCapture {
            usage,
            metadata: Some(metadata),
            block: Some(block),
            content_index: own,
            ..AssistantCapture::default()
        }));
    }
    let raw_argument_prefix = hint.raw_argument_prefix.clone().unwrap_or_default();
    let covered_code_units = match block.as_ref() {
        ContentBlock::Text(text) => utf16_len(&text.text),
        ContentBlock::Thinking(thinking) => utf16_len(&thinking.thinking),
        _ => utf16_len(&raw_argument_prefix),
    };
    let parsed_through = hint.parsed_through.unwrap_or(0);
    let next_offset = seed_next_offset(
        Some(hint.offset.unwrap_or(covered_code_units as i64)),
        covered_code_units,
    )?;
    state.positions.insert(
        index,
        CapturePosition {
            offset: next_offset,
            parsed_through,
        },
    );
    let is_tool_call = matches!(block.as_ref(), ContentBlock::ToolCall(_));
    Ok(Some(AssistantCapture {
        usage,
        metadata: Some(metadata),
        block: Some(block),
        covered_code_units: Some(covered_code_units),
        next_offset: Some(next_offset),
        content_index: own,
        raw_argument_prefix: is_tool_call.then_some(raw_argument_prefix),
        parsed_through: is_tool_call.then_some(parsed_through),
        ..AssistantCapture::default()
    }))
}

fn usage_copy(state: &mut CaptureState, usage: &Usage) -> Usage {
    if let Some(previous) = state.usage
        && same_value(usage, &previous)
    {
        return previous;
    }
    state.usage = Some(*usage);
    *usage
}

/// Scalar message fields changed since the last non-delta capture; the baseline moves only once
/// its copy exists.
fn changed_metadata(
    state: &mut CaptureState,
    message: &AssistantMessage,
) -> Arc<Map<String, Value>> {
    let fields = message_fields(message);
    let mut changed = Map::new();
    for (key, value) in &fields {
        if state.metadata.get(key) != Some(value) {
            changed.insert(key.clone(), value.clone());
        }
    }
    for key in state.metadata.keys() {
        if !fields.contains_key(key) {
            changed.insert(key.clone(), Value::Null);
        }
    }
    if changed.is_empty() {
        return NO_CHANGE.clone();
    }
    for (key, value) in &changed {
        match fields.get(key) {
            Some(_) => {
                state.metadata.insert(key.clone(), value.clone());
            }
            None => {
                state.metadata.remove(key);
            }
        }
    }
    Arc::new(changed)
}
