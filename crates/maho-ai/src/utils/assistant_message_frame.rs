//! Port of senpi packages/ai/src/utils/assistant-message-frame.ts.
//!
//! Frames are the durable, replayable form of an assistant stream: the encoder turns live
//! `AssistantMessageEvent`s (whose `partial` may already be ahead of the queued event) into
//! frames without duplicate content, and the reducer rebuilds the message from frames.
//! Character bookkeeping for text/thinking deltas is in UTF-16 code units, as in JS.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, StopReason, TextContent, ThinkingContent, ToolCall};
use crate::utils::json_parse::parse_streaming_json;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageFrame {
    Start {
        partial: Box<AssistantMessage>,
    },
    #[serde(rename_all = "camelCase")]
    TextStart { content_index: usize, content: TextContent },
    #[serde(rename_all = "camelCase")]
    TextDelta { content_index: usize, delta: String },
    #[serde(rename_all = "camelCase")]
    TextEnd {
        content_index: usize,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ThinkingStart { content_index: usize, content: ThinkingContent },
    #[serde(rename_all = "camelCase")]
    ThinkingDelta { content_index: usize, delta: String },
    #[serde(rename_all = "camelCase")]
    ThinkingEnd {
        content_index: usize,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    #[serde(rename = "toolcall_start", rename_all = "camelCase")]
    ToolcallStart { content_index: usize, tool_call: ToolCall },
    #[serde(rename = "toolcall_checkpoint", rename_all = "camelCase")]
    ToolcallCheckpoint { content_index: usize, json: String },
    #[serde(rename = "toolcall_delta", rename_all = "camelCase")]
    ToolcallDelta { content_index: usize, delta: String },
    #[serde(rename = "toolcall_end", rename_all = "camelCase")]
    ToolcallEnd {
        content_index: usize,
        id: String,
        name: String,
        arguments: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
}

impl AssistantMessageFrame {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::TextStart { .. } => "text_start",
            Self::TextDelta { .. } => "text_delta",
            Self::TextEnd { .. } => "text_end",
            Self::ThinkingStart { .. } => "thinking_start",
            Self::ThinkingDelta { .. } => "thinking_delta",
            Self::ThinkingEnd { .. } => "thinking_end",
            Self::ToolcallStart { .. } => "toolcall_start",
            Self::ToolcallCheckpoint { .. } => "toolcall_checkpoint",
            Self::ToolcallDelta { .. } => "toolcall_delta",
            Self::ToolcallEnd { .. } => "toolcall_end",
        }
    }
}

/// A protocol violation in an event or frame sequence (the TS module throws `Error`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FrameError(pub String);

fn fail<T>(message: String) -> Result<T, FrameError> {
    Err(FrameError(message))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolCall,
}

impl BlockKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Thinking => "thinking",
            Self::ToolCall => "toolCall",
        }
    }
}

fn block_type(block: &ContentBlock) -> &'static str {
    match block {
        ContentBlock::Text(_) => "text",
        ContentBlock::Thinking(_) => "thinking",
        ContentBlock::Image(_) => "image",
        ContentBlock::ToolCall(_) => "toolCall",
        ContentBlock::ProviderNative(_) => "providerNative",
    }
}

enum EncoderBlockState {
    Text { covered_chars: usize, delta_chars: usize },
    Thinking { covered_chars: usize, delta_chars: usize },
    ToolCall { caught_up: bool, catchup_json: String, snapshot_arguments: String },
}

impl EncoderBlockState {
    fn kind(&self) -> BlockKind {
        match self {
            Self::Text { .. } => BlockKind::Text,
            Self::Thinking { .. } => BlockKind::Thinking,
            Self::ToolCall { .. } => BlockKind::ToolCall,
        }
    }
}

fn clone_text_content(content: &TextContent) -> TextContent {
    TextContent { text: content.text.clone(), text_signature: content.text_signature.clone(), ..TextContent::default() }
}

fn clone_thinking_content(content: &ThinkingContent) -> ThinkingContent {
    ThinkingContent {
        thinking: content.thinking.clone(),
        thinking_signature: content.thinking_signature.clone(),
        redacted: content.redacted,
        ..ThinkingContent::default()
    }
}

fn clone_tool_call(tool_call: &ToolCall) -> ToolCall {
    ToolCall {
        id: tool_call.id.clone(),
        name: tool_call.name.clone(),
        arguments: tool_call.arguments.clone(),
        incomplete: None,
        error_message: None,
        thought_signature: tool_call.thought_signature.clone(),
        namespace: tool_call.namespace.clone(),
    }
}

fn clone_start_message(message: &AssistantMessage) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: message.api.clone(),
        provider: message.provider.clone(),
        model: message.model.clone(),
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        provider_thinking_level: message.provider_thinking_level.clone(),
        diagnostics: message.diagnostics.clone(),
        usage: message.usage,
        stop_reason: StopReason::Pending,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: message.timestamp,
    }
}

fn event_block<'a>(event_type: &str, partial: &'a AssistantMessage, content_index: usize) -> Result<&'a ContentBlock, FrameError> {
    match partial.content.get(content_index) {
        Some(block) => Ok(block),
        None => fail(format!("{event_type} event has no content block at index {content_index}")),
    }
}

fn wrong_block<T>(event_type: &str, block: &ContentBlock, content_index: usize) -> Result<T, FrameError> {
    fail(format!("{event_type} event points to {} block at index {content_index}", block_type(block)))
}

fn serialized_arguments(arguments: &Value) -> String {
    serde_json::to_string(arguments).unwrap_or_default()
}

/// `serializedArguments(parseStreamingJson(""))`.
const EMPTY_PARSED_TOOL_ARGUMENTS: &str = "{}";

fn is_json_prefix(snapshot: &Value, current: &Value) -> bool {
    match snapshot {
        Value::String(prefix) => current.as_str().is_some_and(|text| text.starts_with(prefix.as_str())),
        Value::Array(items) => current
            .as_array()
            .is_some_and(|now| items.len() <= now.len() && items.iter().zip(now).all(|(a, b)| is_json_prefix(a, b))),
        Value::Object(entries) => current
            .as_object()
            .is_some_and(|now| entries.iter().all(|(key, value)| now.get(key).is_some_and(|b| is_json_prefix(value, b)))),
        Value::Number(n) => current.as_f64() == n.as_f64() && current.is_number(),
        _ => snapshot == current,
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn utf16_tail(text: &str, from: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().skip(from).collect();
    String::from_utf16_lossy(&units)
}

#[derive(Default)]
pub struct AssistantMessageFrameEncoder {
    started: bool,
    terminal: bool,
    blocks: HashMap<usize, EncoderBlockState>,
}

impl AssistantMessageFrameEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn encode(&mut self, event: &AssistantMessageEvent) -> Result<Option<AssistantMessageFrame>, FrameError> {
        use AssistantMessageEvent as E;
        let event_type = event_type(event);
        if self.terminal {
            return fail(format!("Assistant message event {event_type} follows a terminal event"));
        }
        match event {
            E::Start { partial } => {
                if self.started {
                    return fail("Assistant message stream contains more than one start event".into());
                }
                self.started = true;
                return Ok(Some(AssistantMessageFrame::Start { partial: Box::new(clone_start_message(partial)) }));
            }
            E::Done { .. } => {
                if !self.started {
                    return fail("Assistant message done event appears before start".into());
                }
                self.terminal = true;
                return Ok(None);
            }
            E::Error { .. } => {
                self.terminal = true;
                return Ok(None);
            }
            _ => {}
        }
        if !self.started {
            return fail(format!("Assistant message {event_type} event appears before start"));
        }
        match event {
            E::TextStart { content_index, partial } => {
                let index = *content_index;
                let ContentBlock::Text(content) = event_block(event_type, partial, index)? else {
                    return wrong_block(event_type, event_block(event_type, partial, index)?, index);
                };
                self.start_block(index, EncoderBlockState::Text { covered_chars: utf16_len(&content.text), delta_chars: 0 })?;
                Ok(Some(AssistantMessageFrame::TextStart { content_index: index, content: clone_text_content(content) }))
            }
            E::TextDelta { content_index, delta, .. } => self.encode_text_delta(*content_index, delta, BlockKind::Text),
            E::TextEnd { content_index, content, partial } => {
                let index = *content_index;
                let block = event_block(event_type, partial, index)?;
                let ContentBlock::Text(text) = block else { return wrong_block(event_type, block, index) };
                self.end_block(index, BlockKind::Text)?;
                Ok(Some(AssistantMessageFrame::TextEnd { content_index: index, content: content.clone(), text_signature: text.text_signature.clone() }))
            }
            E::ThinkingStart { content_index, partial } => {
                let index = *content_index;
                let block = event_block(event_type, partial, index)?;
                let ContentBlock::Thinking(content) = block else { return wrong_block(event_type, block, index) };
                self.start_block(index, EncoderBlockState::Thinking { covered_chars: utf16_len(&content.thinking), delta_chars: 0 })?;
                Ok(Some(AssistantMessageFrame::ThinkingStart { content_index: index, content: clone_thinking_content(content) }))
            }
            E::ThinkingDelta { content_index, delta, .. } => self.encode_text_delta(*content_index, delta, BlockKind::Thinking),
            E::ThinkingEnd { content_index, content, partial } => {
                let index = *content_index;
                let block = event_block(event_type, partial, index)?;
                let ContentBlock::Thinking(thinking) = block else { return wrong_block(event_type, block, index) };
                self.end_block(index, BlockKind::Thinking)?;
                Ok(Some(AssistantMessageFrame::ThinkingEnd {
                    content_index: index,
                    content: content.clone(),
                    thinking_signature: thinking.thinking_signature.clone(),
                    redacted: thinking.redacted,
                }))
            }
            E::ToolcallStart { content_index, partial } => {
                let index = *content_index;
                let block = event_block(event_type, partial, index)?;
                let ContentBlock::ToolCall(tool_call) = block else { return wrong_block(event_type, block, index) };
                let snapshot = serialized_arguments(&Value::Object(tool_call.arguments.clone()));
                let caught_up = snapshot == EMPTY_PARSED_TOOL_ARGUMENTS;
                self.start_block(
                    index,
                    EncoderBlockState::ToolCall {
                        caught_up,
                        catchup_json: String::new(),
                        snapshot_arguments: if caught_up { String::new() } else { snapshot },
                    },
                )?;
                Ok(Some(AssistantMessageFrame::ToolcallStart { content_index: index, tool_call: clone_tool_call(tool_call) }))
            }
            E::ToolcallDelta { content_index, delta, .. } => {
                let index = *content_index;
                let EncoderBlockState::ToolCall { caught_up, catchup_json, snapshot_arguments } = self.block(index, BlockKind::ToolCall)? else {
                    return fail("Unreachable tool-call encoder state".into());
                };
                if *caught_up {
                    return Ok((!delta.is_empty()).then(|| AssistantMessageFrame::ToolcallDelta { content_index: index, delta: delta.clone() }));
                }
                catchup_json.push_str(delta);
                let arguments = parse_streaming_json(Some(catchup_json));
                if serialized_arguments(&arguments) != *snapshot_arguments {
                    let snapshot = parse_streaming_json(Some(snapshot_arguments));
                    if !is_json_prefix(&snapshot, &arguments) {
                        return Ok(None);
                    }
                }
                *caught_up = true;
                snapshot_arguments.clear();
                let json = std::mem::take(catchup_json);
                Ok((!json.is_empty()).then_some(AssistantMessageFrame::ToolcallCheckpoint { content_index: index, json }))
            }
            E::ToolcallEnd { content_index, tool_call, partial } => {
                let index = *content_index;
                let block = event_block(event_type, partial, index)?;
                if !matches!(block, ContentBlock::ToolCall(_)) {
                    return wrong_block(event_type, block, index);
                }
                self.end_block(index, BlockKind::ToolCall)?;
                Ok(Some(AssistantMessageFrame::ToolcallEnd {
                    content_index: index,
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    thought_signature: tool_call.thought_signature.clone(),
                    namespace: tool_call.namespace.clone(),
                }))
            }
            E::Start { .. } | E::Done { .. } | E::Error { .. } => Ok(None),
        }
    }

    fn start_block(&mut self, content_index: usize, state: EncoderBlockState) -> Result<(), FrameError> {
        if self.blocks.contains_key(&content_index) {
            return fail(format!("Assistant message block {content_index} starts more than once"));
        }
        self.blocks.insert(content_index, state);
        Ok(())
    }

    fn block(&mut self, content_index: usize, kind: BlockKind) -> Result<&mut EncoderBlockState, FrameError> {
        let Some(state) = self.blocks.get_mut(&content_index) else {
            return fail(format!("Assistant message {} block {content_index} has not started", kind.as_str()));
        };
        if state.kind() != kind {
            return fail(format!("Assistant message block {content_index} is {}, not {}", state.kind().as_str(), kind.as_str()));
        }
        Ok(state)
    }

    fn end_block(&mut self, content_index: usize, kind: BlockKind) -> Result<(), FrameError> {
        self.block(content_index, kind)?;
        self.blocks.remove(&content_index);
        Ok(())
    }

    fn encode_text_delta(&mut self, content_index: usize, delta: &str, kind: BlockKind) -> Result<Option<AssistantMessageFrame>, FrameError> {
        let (EncoderBlockState::Text { covered_chars, delta_chars } | EncoderBlockState::Thinking { covered_chars, delta_chars }) =
            self.block(content_index, kind)?
        else {
            return fail("Unreachable text encoder state".into());
        };
        let delta_start = *delta_chars;
        let length = utf16_len(delta);
        *delta_chars += length;
        let covered = covered_chars.saturating_sub(delta_start);
        if covered >= length {
            return Ok(None);
        }
        let uncovered = if covered == 0 { delta.to_owned() } else { utf16_tail(delta, covered) };
        Ok(Some(match kind {
            BlockKind::Text => AssistantMessageFrame::TextDelta { content_index, delta: uncovered },
            _ => AssistantMessageFrame::ThinkingDelta { content_index, delta: uncovered },
        }))
    }
}

fn event_type(event: &AssistantMessageEvent) -> &'static str {
    use AssistantMessageEvent as E;
    match event {
        E::Start { .. } => "start",
        E::TextStart { .. } => "text_start",
        E::TextDelta { .. } => "text_delta",
        E::TextEnd { .. } => "text_end",
        E::ThinkingStart { .. } => "thinking_start",
        E::ThinkingDelta { .. } => "thinking_delta",
        E::ThinkingEnd { .. } => "thinking_end",
        E::ToolcallStart { .. } => "toolcall_start",
        E::ToolcallDelta { .. } => "toolcall_delta",
        E::ToolcallEnd { .. } => "toolcall_end",
        E::Done { .. } => "done",
        E::Error { .. } => "error",
    }
}

struct ReducerBlockState {
    kind: BlockKind,
    ended: bool,
    json: String,
}

fn append_block(
    message: &mut AssistantMessage,
    states: &mut BTreeMap<usize, ReducerBlockState>,
    content_index: usize,
    block: ContentBlock,
    kind: BlockKind,
) -> Result<(), FrameError> {
    if content_index != message.content.len() {
        let reason = if content_index < message.content.len() { "already exists" } else { "would leave a gap" };
        return fail(format!("Cannot start assistant message block at index {content_index}: {reason}"));
    }
    message.content.push(block);
    states.insert(content_index, ReducerBlockState { kind, ended: false, json: String::new() });
    Ok(())
}

fn active_block<'a>(
    message: &'a mut AssistantMessage,
    states: &'a mut BTreeMap<usize, ReducerBlockState>,
    content_index: usize,
    expected: BlockKind,
    frame_type: &str,
) -> Result<(&'a mut ContentBlock, &'a mut ReducerBlockState), FrameError> {
    let (Some(state), Some(block)) = (states.get_mut(&content_index), message.content.get_mut(content_index)) else {
        return fail(format!("{frame_type} frame has no started block at index {content_index}"));
    };
    if state.kind != expected || block_type(block) != expected.as_str() {
        return fail(format!(
            "{frame_type} frame expected {} block at index {content_index}, found {}",
            expected.as_str(),
            block_type(block)
        ));
    }
    if state.ended {
        return fail(format!("{frame_type} frame follows the end of block at index {content_index}"));
    }
    Ok((block, state))
}

pub fn reduce_assistant_message_frames<'a>(
    frames: impl IntoIterator<Item = &'a AssistantMessageFrame>,
) -> Result<Option<AssistantMessage>, FrameError> {
    use AssistantMessageFrame as F;
    let mut message: Option<AssistantMessage> = None;
    let mut frame_before_start: Option<&'static str> = None;
    let mut states: BTreeMap<usize, ReducerBlockState> = BTreeMap::new();
    for frame in frames {
        if let F::Start { partial } = frame {
            if message.is_some() {
                return fail("Assistant message frame sequence contains more than one start frame".into());
            }
            if let Some(before) = frame_before_start {
                return fail(format!("{before} frame appears before the start frame"));
            }
            message = Some(partial.as_ref().clone());
            continue;
        }
        let Some(message) = message.as_mut() else {
            frame_before_start.get_or_insert(frame.type_name());
            continue;
        };
        let frame_type = frame.type_name();
        match frame {
            F::Start { .. } => {}
            F::TextStart { content_index, content } => {
                append_block(message, &mut states, *content_index, ContentBlock::Text(content.clone()), BlockKind::Text)?;
            }
            F::TextDelta { content_index, delta } => {
                if let (ContentBlock::Text(block), _) = active_block(message, &mut states, *content_index, BlockKind::Text, frame_type)? {
                    block.text.push_str(delta);
                }
            }
            F::TextEnd { content_index, content, text_signature } => {
                if let (ContentBlock::Text(block), state) = active_block(message, &mut states, *content_index, BlockKind::Text, frame_type)? {
                    block.text = content.clone();
                    block.text_signature = text_signature.clone();
                    state.ended = true;
                }
            }
            F::ThinkingStart { content_index, content } => {
                append_block(message, &mut states, *content_index, ContentBlock::Thinking(content.clone()), BlockKind::Thinking)?;
            }
            F::ThinkingDelta { content_index, delta } => {
                if let (ContentBlock::Thinking(block), _) = active_block(message, &mut states, *content_index, BlockKind::Thinking, frame_type)? {
                    block.thinking.push_str(delta);
                }
            }
            F::ThinkingEnd { content_index, content, thinking_signature, redacted } => {
                if let (ContentBlock::Thinking(block), state) = active_block(message, &mut states, *content_index, BlockKind::Thinking, frame_type)? {
                    block.thinking = content.clone();
                    block.thinking_signature = thinking_signature.clone();
                    block.redacted = *redacted;
                    state.ended = true;
                }
            }
            F::ToolcallStart { content_index, tool_call } => {
                append_block(message, &mut states, *content_index, ContentBlock::ToolCall(tool_call.clone()), BlockKind::ToolCall)?;
            }
            F::ToolcallCheckpoint { content_index, json } => {
                if let (ContentBlock::ToolCall(block), state) = active_block(message, &mut states, *content_index, BlockKind::ToolCall, frame_type)? {
                    state.json = json.clone();
                    block.arguments = object_or_empty(parse_streaming_json(Some(json)));
                }
            }
            F::ToolcallDelta { content_index, delta } => {
                let (_, state) = active_block(message, &mut states, *content_index, BlockKind::ToolCall, frame_type)?;
                state.json.push_str(delta);
            }
            F::ToolcallEnd { content_index, id, name, arguments, thought_signature, namespace } => {
                if let (ContentBlock::ToolCall(block), state) = active_block(message, &mut states, *content_index, BlockKind::ToolCall, frame_type)? {
                    block.id = id.clone();
                    block.name = name.clone();
                    block.arguments = arguments.clone();
                    block.thought_signature = thought_signature.clone();
                    block.namespace = namespace.clone();
                    state.ended = true;
                }
            }
        }
    }
    let Some(mut message) = message else { return Ok(None) };
    for (content_index, state) in &states {
        if state.kind != BlockKind::ToolCall || state.ended || state.json.is_empty() {
            continue;
        }
        let Some(ContentBlock::ToolCall(block)) = message.content.get_mut(*content_index) else {
            return fail("Unreachable tool-call frame state".into());
        };
        block.arguments = object_or_empty(parse_streaming_json(Some(&state.json)));
    }
    Ok(Some(message))
}

/// Rust `ToolCall.arguments` is an object; a non-object parse (TS would store it as-is) becomes `{}`.
fn object_or_empty(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssistantMessageDiagnostic, DoneReason, ErrorReason, Usage};
    use serde_json::{Map, json};

    fn seed() -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "test-api".into(),
            provider: "test-provider".into(),
            model: "test-model".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1,
        }
    }

    fn frame(encoder: &mut AssistantMessageFrameEncoder, event: AssistantMessageEvent) -> AssistantMessageFrame {
        encoder.encode(&event).expect("event should not error").expect("event should produce a frame")
    }

    fn text_content(text: &str) -> TextContent {
        TextContent { text: text.into(), ..TextContent::default() }
    }

    fn thinking_content(thinking: &str) -> ThinkingContent {
        ThinkingContent { thinking: thinking.into(), ..ThinkingContent::default() }
    }

    fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: object_or_empty(arguments),
            ..ToolCall::default()
        }
    }

    fn obj(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    }

    #[test]
    fn uses_authoritative_text_end_content_and_signature() {
        let mut partial = seed();
        let mut encoder = AssistantMessageFrameEncoder::new();
        let mut frames = vec![frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() })];
        partial.content.push(ContentBlock::Text(text_content("Hello ")));
        frames.push(frame(&mut encoder, AssistantMessageEvent::TextStart { content_index: 0, partial: partial.clone() }));
        partial.content[0] = ContentBlock::Text(TextContent { text: "Hello world".into(), text_signature: Some("sig-text".into()), ..TextContent::default() });
        frames.push(frame(&mut encoder, AssistantMessageEvent::TextDelta { content_index: 0, delta: "incorrect".into(), partial: partial.clone() }));
        frames.push(frame(&mut encoder, AssistantMessageEvent::TextEnd { content_index: 0, content: "Hello world".into(), partial: partial.clone() }));

        assert_eq!(
            frames.last(),
            Some(&AssistantMessageFrame::TextEnd { content_index: 0, content: "Hello world".into(), text_signature: Some("sig-text".into()) })
        );
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(
            reduced.content,
            vec![ContentBlock::Text(TextContent { text: "Hello world".into(), text_signature: Some("sig-text".into()), ..TextContent::default() })]
        );
    }

    #[test]
    fn preserves_provider_thinking_level_from_the_stream_start() {
        let mut partial = seed();
        partial.provider_thinking_level = Some("high".into());
        let mut encoder = AssistantMessageFrameEncoder::new();
        let start = frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() });

        let AssistantMessageFrame::Start { partial: started } = &start else { panic!("expected start frame") };
        assert_eq!(started.provider_thinking_level.as_deref(), Some("high"));
        let reduced = reduce_assistant_message_frames(&[start]).unwrap().unwrap();
        assert_eq!(reduced.provider_thinking_level.as_deref(), Some("high"));
    }

    #[test]
    fn preserves_initial_and_final_thinking_metadata_including_redaction() {
        let mut partial = seed();
        let mut encoder = AssistantMessageFrameEncoder::new();
        let mut frames = vec![frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() })];
        partial.content.push(ContentBlock::Thinking(ThinkingContent {
            thinking: "[redacted]".into(),
            thinking_signature: Some("encrypted-start".into()),
            redacted: Some(true),
            ..ThinkingContent::default()
        }));
        frames.push(frame(&mut encoder, AssistantMessageEvent::ThinkingStart { content_index: 0, partial: partial.clone() }));
        partial.content[0] = ContentBlock::Thinking(ThinkingContent {
            thinking: "[redacted]".into(),
            thinking_signature: Some("encrypted-final".into()),
            redacted: Some(true),
            ..ThinkingContent::default()
        });
        frames.push(frame(&mut encoder, AssistantMessageEvent::ThinkingEnd { content_index: 0, content: "[redacted]".into(), partial: partial.clone() }));

        assert_eq!(
            frames.last(),
            Some(&AssistantMessageFrame::ThinkingEnd {
                content_index: 0,
                content: "[redacted]".into(),
                thinking_signature: Some("encrypted-final".into()),
                redacted: Some(true),
            })
        );
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(
            reduced.content[0],
            ContentBlock::Thinking(ThinkingContent {
                thinking: "[redacted]".into(),
                thinking_signature: Some("encrypted-final".into()),
                redacted: Some(true),
                ..ThinkingContent::default()
            })
        );
    }

    #[test]
    fn parses_unfinished_tool_json_once_and_uses_authoritative_completed_arguments() {
        let initial_frames = vec![
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
            AssistantMessageFrame::ToolcallStart { content_index: 0, tool_call: tool_call("initial-id", "write", json!({})) },
            AssistantMessageFrame::ToolcallDelta { content_index: 0, delta: r#"{"path":"READ"#.into() },
        ];

        let reduced = reduce_assistant_message_frames(&initial_frames).unwrap().unwrap();
        let ContentBlock::ToolCall(first) = &reduced.content[0] else { panic!("expected tool call") };
        assert_eq!(first.arguments, obj(&[("path", json!("READ"))]));

        let mut complete_frames = initial_frames;
        complete_frames.push(AssistantMessageFrame::ToolcallDelta { content_index: 0, delta: r#"ME.md","lines":[1,2]}"#.into() });
        complete_frames.push(AssistantMessageFrame::ToolcallEnd {
            content_index: 0,
            id: "final-id".into(),
            name: "write_file".into(),
            arguments: obj(&[("path", json!("final.md")), ("lines", json!([3]))]),
            thought_signature: Some("thought".into()),
            namespace: Some("files".into()),
        });
        let reduced = reduce_assistant_message_frames(&complete_frames).unwrap().unwrap();
        assert_eq!(
            reduced.content[0],
            ContentBlock::ToolCall(ToolCall {
                id: "final-id".into(),
                name: "write_file".into(),
                arguments: obj(&[("path", json!("final.md")), ("lines", json!([3]))]),
                thought_signature: Some("thought".into()),
                namespace: Some("files".into()),
                ..ToolCall::default()
            })
        );
    }

    #[test]
    fn reconciles_queued_text_events_against_one_advanced_live_partial_without_duplicate_content() {
        let mut partial = seed();
        // TS pushes events that all reference the same live `partial` object, then encodes
        // in a separate pass once every mutation has already happened; each event therefore
        // sees the *final* partial regardless of when it was constructed. Model that by
        // building the event kinds first and stamping every one with the final `partial`
        // clone right before encoding.
        enum Kind {
            Start,
            TextStart { content_index: usize },
            TextDelta { content_index: usize, delta: String },
        }
        let mut kinds = vec![Kind::Start];
        partial.content.push(ContentBlock::Text(text_content("")));
        kinds.push(Kind::TextStart { content_index: 0 });
        let mut text = String::new();
        for delta in ["Hel", "lo", " ", "world"] {
            text.push_str(delta);
            partial.content[0] = ContentBlock::Text(text_content(&text));
            kinds.push(Kind::TextDelta { content_index: 0, delta: delta.into() });
        }

        let mut encoder = AssistantMessageFrameEncoder::new();
        let frames: Vec<AssistantMessageFrame> = kinds
            .into_iter()
            .filter_map(|kind| {
                let event = match kind {
                    Kind::Start => AssistantMessageEvent::Start { partial: partial.clone() },
                    Kind::TextStart { content_index } => AssistantMessageEvent::TextStart { content_index, partial: partial.clone() },
                    Kind::TextDelta { content_index, delta } => AssistantMessageEvent::TextDelta { content_index, delta, partial: partial.clone() },
                };
                encoder.encode(&event).expect("no encoder error")
            })
            .collect();

        assert_eq!(frames.iter().map(AssistantMessageFrame::type_name).collect::<Vec<_>>(), vec!["start", "text_start"]);
        let AssistantMessageFrame::Start { partial: started } = &frames[0] else { panic!("expected start") };
        assert!(started.content.is_empty());
        assert_eq!(started.stop_reason, StopReason::Pending);
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(reduced.content, vec![ContentBlock::Text(text_content("Hello world"))]);
    }

    #[test]
    fn trims_only_the_covered_prefix_when_a_start_snapshot_lands_inside_a_delta() {
        let mut partial = seed();
        let mut encoder = AssistantMessageFrameEncoder::new();
        let mut frames = vec![frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() })];
        partial.content.push(ContentBlock::Text(text_content("Hel")));
        frames.push(frame(&mut encoder, AssistantMessageEvent::TextStart { content_index: 0, partial: partial.clone() }));
        assert_eq!(
            encoder
                .encode(&AssistantMessageEvent::TextDelta { content_index: 0, delta: "He".into(), partial: partial.clone() })
                .unwrap(),
            None
        );
        let remainder = encoder
            .encode(&AssistantMessageEvent::TextDelta { content_index: 0, delta: "llo".into(), partial: partial.clone() })
            .unwrap()
            .expect("expected uncovered text delta");
        frames.push(remainder.clone());

        assert_eq!(remainder, AssistantMessageFrame::TextDelta { content_index: 0, delta: "lo".into() });
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(reduced.content, vec![ContentBlock::Text(text_content("Hello"))]);
    }

    #[test]
    fn checkpoints_queued_tool_json_without_replaying_covered_deltas() {
        let mut partial = seed();
        // Same live-partial semantics as the text case above: build event kinds first, then
        // stamp every one with the final mutated `partial` right before encoding.
        enum Kind {
            Start,
            ToolcallStart { content_index: usize },
            ToolcallDelta { content_index: usize, delta: String },
        }
        let mut kinds = vec![Kind::Start];
        partial.content.push(ContentBlock::ToolCall(tool_call("call", "write", json!({}))));
        kinds.push(Kind::ToolcallStart { content_index: 0 });
        partial.content[0] = ContentBlock::ToolCall(tool_call("call", "write", json!({"path": "README.md"})));
        kinds.push(Kind::ToolcallDelta { content_index: 0, delta: r#"{"path":"READ"#.into() });
        kinds.push(Kind::ToolcallDelta { content_index: 0, delta: r#"ME.md"}"#.into() });

        let mut encoder = AssistantMessageFrameEncoder::new();
        let frames: Vec<AssistantMessageFrame> = kinds
            .into_iter()
            .filter_map(|kind| {
                let event = match kind {
                    Kind::Start => AssistantMessageEvent::Start { partial: partial.clone() },
                    Kind::ToolcallStart { content_index } => AssistantMessageEvent::ToolcallStart { content_index, partial: partial.clone() },
                    Kind::ToolcallDelta { content_index, delta } => AssistantMessageEvent::ToolcallDelta { content_index, delta, partial: partial.clone() },
                };
                encoder.encode(&event).expect("no encoder error")
            })
            .collect();
        assert_eq!(
            frames.iter().map(AssistantMessageFrame::type_name).collect::<Vec<_>>(),
            vec!["start", "toolcall_start", "toolcall_checkpoint"]
        );
        assert_eq!(
            frames.last(),
            Some(&AssistantMessageFrame::ToolcallCheckpoint { content_index: 0, json: r#"{"path":"README.md"}"#.into() })
        );
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(
            reduced.content,
            vec![ContentBlock::ToolCall(tool_call("call", "write", json!({"path": "README.md"})))]
        );
    }

    #[test]
    fn resumes_legacy_grammar_tool_json_from_initial_arguments() {
        let mut partial = seed();
        let mut encoder = AssistantMessageFrameEncoder::new();
        let mut frames = vec![frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() })];
        partial.content.push(ContentBlock::ToolCall(tool_call("call", "bash", json!({"input": "a"}))));
        frames.push(frame(&mut encoder, AssistantMessageEvent::ToolcallStart { content_index: 0, partial: partial.clone() }));
        partial.content[0] = ContentBlock::ToolCall(tool_call("call", "bash", json!({"input": "ab"})));
        frames.push(frame(&mut encoder, AssistantMessageEvent::ToolcallDelta { content_index: 0, delta: r#"{"input":"ab"#.into(), partial: partial.clone() }));
        partial.content[0] = ContentBlock::ToolCall(tool_call("call", "bash", json!({"input": "abc"})));
        frames.push(frame(&mut encoder, AssistantMessageEvent::ToolcallDelta { content_index: 0, delta: r#"c"}"#.into(), partial: partial.clone() }));

        assert_eq!(
            &frames[2..],
            &[
                AssistantMessageFrame::ToolcallCheckpoint { content_index: 0, json: r#"{"input":"ab"#.into() },
                AssistantMessageFrame::ToolcallDelta { content_index: 0, delta: r#"c"}"#.into() },
            ]
        );
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(reduced.content, vec![ContentBlock::ToolCall(tool_call("call", "bash", json!({"input": "abc"})))]);
    }

    #[test]
    fn streams_tool_json_compactly_from_an_empty_argument_start() {
        let mut partial = seed();
        let mut encoder = AssistantMessageFrameEncoder::new();
        let mut frames = vec![frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() })];
        partial.content.push(ContentBlock::ToolCall(tool_call("call", "bash", json!({}))));
        frames.push(frame(&mut encoder, AssistantMessageEvent::ToolcallStart { content_index: 0, partial: partial.clone() }));
        partial.content[0] = ContentBlock::ToolCall(tool_call("call", "bash", json!({"command": "ls -la /tmp"})));
        frames.push(frame(
            &mut encoder,
            AssistantMessageEvent::ToolcallDelta { content_index: 0, delta: r#"{"command":"ls -la /tmp"}"#.into(), partial: partial.clone() },
        ));

        assert_eq!(
            frames.last(),
            Some(&AssistantMessageFrame::ToolcallDelta { content_index: 0, delta: r#"{"command":"ls -la /tmp"}"#.into() })
        );
        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        let ContentBlock::ToolCall(first) = &reduced.content[0] else { panic!("expected tool call") };
        assert_eq!(first.arguments, obj(&[("command", json!("ls -la /tmp"))]));
    }

    #[test]
    fn accepts_a_pre_generation_error_but_rejects_success_or_updates_before_start() {
        let mut failed = seed();
        failed.stop_reason = StopReason::Error;
        failed.error_message = Some("setup failed".into());
        assert_eq!(
            AssistantMessageFrameEncoder::new().encode(&AssistantMessageEvent::Error { reason: ErrorReason::Error, error: failed }).unwrap(),
            None
        );

        let mut completed = seed();
        completed.stop_reason = StopReason::Stop;
        let err = AssistantMessageFrameEncoder::new()
            .encode(&AssistantMessageEvent::Done { reason: DoneReason::Stop, message: completed })
            .unwrap_err();
        assert!(err.0.contains("done event appears before start"), "{}", err.0);

        let err = AssistantMessageFrameEncoder::new()
            .encode(&AssistantMessageEvent::TextDelta { content_index: 0, delta: "x".into(), partial: seed() })
            .unwrap_err();
        assert!(err.0.contains("text_delta event appears before start"), "{}", err.0);
    }

    #[test]
    fn treats_end_signature_metadata_including_absence_as_authoritative() {
        let frames = vec![
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
            AssistantMessageFrame::TextStart {
                content_index: 0,
                content: TextContent { text: "".into(), text_signature: Some("stale-text".into()), ..TextContent::default() },
            },
            AssistantMessageFrame::TextEnd { content_index: 0, content: "".into(), text_signature: None },
            AssistantMessageFrame::ThinkingStart {
                content_index: 1,
                content: ThinkingContent {
                    thinking: "".into(),
                    thinking_signature: Some("stale-thinking".into()),
                    redacted: Some(true),
                    ..ThinkingContent::default()
                },
            },
            AssistantMessageFrame::ThinkingEnd {
                content_index: 1,
                content: "".into(),
                thinking_signature: Some("".into()),
                redacted: Some(false),
            },
            AssistantMessageFrame::ToolcallStart {
                content_index: 2,
                tool_call: ToolCall {
                    id: "call".into(),
                    name: "read".into(),
                    arguments: Map::new(),
                    thought_signature: Some("stale-tool".into()),
                    namespace: Some("stale-namespace".into()),
                    ..ToolCall::default()
                },
            },
            AssistantMessageFrame::ToolcallEnd {
                content_index: 2,
                id: "call".into(),
                name: "read".into(),
                arguments: Map::new(),
                thought_signature: None,
                namespace: None,
            },
        ];

        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(
            reduced.content,
            vec![
                ContentBlock::Text(text_content("")),
                ContentBlock::Thinking(ThinkingContent { thinking: "".into(), thinking_signature: Some("".into()), redacted: Some(false), ..ThinkingContent::default() }),
                ContentBlock::ToolCall(ToolCall { id: "call".into(), name: "read".into(), arguments: Map::new(), ..ToolCall::default() }),
            ]
        );
    }

    #[test]
    fn stores_authoritative_final_arguments_in_toolcall_end_frames() {
        let mut partial = seed();
        let tool_call = ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: obj(&[("path", json!("README.md"))]),
            thought_signature: Some("thought".into()),
            namespace: Some("files".into()),
            ..ToolCall::default()
        };
        partial.content.push(ContentBlock::ToolCall(tool_call.clone()));

        let mut encoder = AssistantMessageFrameEncoder::new();
        frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() });
        frame(&mut encoder, AssistantMessageEvent::ToolcallStart { content_index: 0, partial: partial.clone() });
        let end = frame(&mut encoder, AssistantMessageEvent::ToolcallEnd { content_index: 0, tool_call: tool_call.clone(), partial: partial.clone() });
        assert_eq!(
            end,
            AssistantMessageFrame::ToolcallEnd {
                content_index: 0,
                id: "call-1".into(),
                name: "read".into(),
                arguments: obj(&[("path", json!("README.md"))]),
                thought_signature: Some("thought".into()),
                namespace: Some("files".into()),
            }
        );
    }

    #[test]
    fn supports_interleaved_streams_by_content_index() {
        let frames = vec![
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
            AssistantMessageFrame::TextStart { content_index: 0, content: text_content("") },
            AssistantMessageFrame::ToolcallStart { content_index: 1, tool_call: tool_call("call", "lookup", json!({})) },
            AssistantMessageFrame::ThinkingStart { content_index: 2, content: thinking_content("") },
            AssistantMessageFrame::TextDelta { content_index: 0, delta: "answer".into() },
            AssistantMessageFrame::ToolcallDelta { content_index: 1, delta: r#"{"query":"pi"}"#.into() },
            AssistantMessageFrame::ThinkingDelta { content_index: 2, delta: "check".into() },
            AssistantMessageFrame::ToolcallEnd {
                content_index: 1,
                id: "call".into(),
                name: "lookup".into(),
                arguments: obj(&[("query", json!("pi"))]),
                thought_signature: None,
                namespace: None,
            },
            AssistantMessageFrame::TextEnd { content_index: 0, content: "answer".into(), text_signature: None },
            AssistantMessageFrame::ThinkingEnd { content_index: 2, content: "check".into(), thinking_signature: None, redacted: None },
        ];

        let reduced = reduce_assistant_message_frames(&frames).unwrap().unwrap();
        assert_eq!(
            reduced.content,
            vec![
                ContentBlock::Text(text_content("answer")),
                ContentBlock::ToolCall(tool_call("call", "lookup", json!({"query": "pi"}))),
                ContentBlock::Thinking(thinking_content("check")),
            ]
        );
    }

    #[test]
    fn snapshots_mutable_event_data_and_keeps_reduction_pure() {
        let mut partial = seed();
        partial.diagnostics = Some(vec![AssistantMessageDiagnostic {
            kind: "test".into(),
            timestamp: 2,
            error: None,
            details: Some(obj(&[("value", json!("original"))])),
        }]);
        let mut encoder = AssistantMessageFrameEncoder::new();
        let start = frame(&mut encoder, AssistantMessageEvent::Start { partial: partial.clone() });
        if let Some(diagnostics) = partial.diagnostics.as_mut() {
            diagnostics[0].details.as_mut().unwrap().insert("value".into(), json!("mutated"));
        }
        partial.usage.cost.total = 99.0;

        partial.content.push(ContentBlock::ToolCall(tool_call("call", "run", json!({"nested": {"value": "original"}}))));
        let tool_start = frame(&mut encoder, AssistantMessageEvent::ToolcallStart { content_index: 0, partial: partial.clone() });
        if let ContentBlock::ToolCall(source_tool) = &mut partial.content[0] {
            source_tool.arguments.insert("nested".into(), json!("mutated"));
        }

        let reduced = reduce_assistant_message_frames(&[start, tool_start.clone()]).unwrap().unwrap();
        assert_eq!(
            reduced.diagnostics.as_ref().and_then(|d| d[0].details.as_ref()).and_then(|d| d.get("value")),
            Some(&json!("original"))
        );
        assert_eq!(reduced.usage.cost.total, 0.0);
        let ContentBlock::ToolCall(reduced_tool) = &reduced.content[0] else { panic!("expected tool call") };
        assert_eq!(reduced_tool.arguments.get("nested"), Some(&json!({"value": "original"})));

        let mut reduced = reduced;
        if let ContentBlock::ToolCall(reduced_tool) = &mut reduced.content[0] {
            reduced_tool.arguments.insert("nested".into(), json!("changed-output"));
        }
        let AssistantMessageFrame::ToolcallStart { tool_call: started_tool, .. } = &tool_start else { panic!("expected toolcall_start") };
        assert_eq!(started_tool.arguments.get("nested"), Some(&json!({"value": "original"})));
    }

    #[test]
    fn omits_terminal_events_because_settlement_is_separate() {
        let mut message = seed();
        let mut completed = AssistantMessageFrameEncoder::new();
        completed.encode(&AssistantMessageEvent::Start { partial: message.clone() }).unwrap();
        message.stop_reason = StopReason::Stop;
        assert_eq!(
            completed.encode(&AssistantMessageEvent::Done { reason: DoneReason::Stop, message: message.clone() }).unwrap(),
            None
        );
        message.stop_reason = StopReason::Error;
        message.error_message = Some("failed".into());
        assert_eq!(
            AssistantMessageFrameEncoder::new().encode(&AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message }).unwrap(),
            None
        );
    }

    #[test]
    fn returns_none_when_there_is_no_start_frame() {
        assert_eq!(reduce_assistant_message_frames(&[]).unwrap(), None);
        assert_eq!(
            reduce_assistant_message_frames(&[AssistantMessageFrame::TextDelta { content_index: 0, delta: "x".into() }]).unwrap(),
            None
        );
    }

    #[test]
    fn rejects_frames_before_start_wrong_block_kinds_duplicate_ends_and_index_gaps() {
        let err = reduce_assistant_message_frames(&[
            AssistantMessageFrame::TextDelta { content_index: 0, delta: "x".into() },
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
        ])
        .unwrap_err();
        assert!(err.0.contains("before the start frame"), "{}", err.0);

        let err = reduce_assistant_message_frames(&[
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
            AssistantMessageFrame::ToolcallStart { content_index: 0, tool_call: tool_call("call", "run", json!({})) },
            AssistantMessageFrame::TextDelta { content_index: 0, delta: "wrong".into() },
        ])
        .unwrap_err();
        assert!(err.0.contains("expected text block"), "{}", err.0);

        let err = reduce_assistant_message_frames(&[
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
            AssistantMessageFrame::TextStart { content_index: 0, content: text_content("") },
            AssistantMessageFrame::TextEnd { content_index: 0, content: "".into(), text_signature: None },
            AssistantMessageFrame::TextEnd { content_index: 0, content: "".into(), text_signature: None },
        ])
        .unwrap_err();
        assert!(err.0.contains("follows the end"), "{}", err.0);

        let err = reduce_assistant_message_frames(&[
            AssistantMessageFrame::Start { partial: Box::new(seed()) },
            AssistantMessageFrame::TextStart { content_index: 1, content: text_content("") },
        ])
        .unwrap_err();
        assert!(err.0.contains("would leave a gap"), "{}", err.0);
    }

    #[test]
    fn rejects_conversion_events_whose_content_index_points_to_the_wrong_block_kind() {
        let mut partial = seed();
        let mut encoder = AssistantMessageFrameEncoder::new();
        encoder.encode(&AssistantMessageEvent::Start { partial: partial.clone() }).unwrap();
        partial.content.push(ContentBlock::Thinking(thinking_content("")));
        let err = encoder.encode(&AssistantMessageEvent::TextStart { content_index: 0, partial }).unwrap_err();
        assert!(err.0.contains("text_start event points to thinking block"), "{}", err.0);
    }
}

