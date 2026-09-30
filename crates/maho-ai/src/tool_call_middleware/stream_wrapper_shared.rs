//! Port of senpi packages/ai/src/tool-call-middleware/stream-wrapper-shared.ts.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, TextContent, ToolCall};
use crate::utils::diagnostics::AssistantMessageDiagnostic;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::json_parse::parse_streaming_json;

use super::stream_message_metadata::{clone_assistant_message_metadata, sync_assistant_message_metadata};
use super::stream_thinking_projection::StreamThinkingProjection;
use super::types::StreamParserEvent;

/// `ToolCall & { partialJson: string }`. Content-index-keyed side state for a tool call still
/// accumulating raw JSON; the canonical `ContentBlock::ToolCall` in `message.content` carries the
/// best-effort parsed snapshot while this map carries the raw source for further parsing.
#[derive(Debug, Clone, Default)]
struct PartialToolCallState {
    partial_json: String,
}

#[derive(Debug, Clone, Default)]
pub struct ParserProjectionResult {
    pub saw_tool_call: bool,
    pub completed_tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StreamMessageProjectionOptions {
    pub preserve_source_metadata: bool,
}

/// Projects parser events into the canonical outer assistant stream.
pub struct StreamMessageProjection {
    pub message: AssistantMessage,
    stream: AssistantMessageEventStream,
    current_inner_text_index: Option<usize>,
    text_block_index_by_inner_index: HashMap<usize, Option<usize>>,
    last_text_block_index_by_inner_index: HashMap<usize, usize>,
    tool_call_index_by_parser_index: HashMap<usize, usize>,
    partial_tool_calls: HashMap<usize, PartialToolCallState>,
    projected_diagnostics: Vec<AssistantMessageDiagnostic>,
    thinking: StreamThinkingProjection,
    preserve_source_metadata: bool,
    source: AssistantMessage,
}

impl StreamMessageProjection {
    pub fn new(
        stream: AssistantMessageEventStream,
        source: AssistantMessage,
        options: StreamMessageProjectionOptions,
    ) -> Self {
        let projected_diagnostics = Vec::new();
        let message =
            clone_assistant_message_metadata(&source, Vec::new(), &projected_diagnostics, options.preserve_source_metadata);
        let thinking = StreamThinkingProjection::new(stream.clone(), options.preserve_source_metadata);
        Self {
            message,
            stream,
            current_inner_text_index: None,
            text_block_index_by_inner_index: HashMap::new(),
            last_text_block_index_by_inner_index: HashMap::new(),
            tool_call_index_by_parser_index: HashMap::new(),
            partial_tool_calls: HashMap::new(),
            projected_diagnostics,
            thinking,
            preserve_source_metadata: options.preserve_source_metadata,
            source,
        }
    }

    pub fn sync(&mut self, source: AssistantMessage) {
        self.source = source;
        sync_assistant_message_metadata(&mut self.message, &self.source, &self.projected_diagnostics, self.preserve_source_metadata);
    }

    pub fn append_diagnostic(&mut self, diagnostic: AssistantMessageDiagnostic) {
        self.projected_diagnostics.push(diagnostic);
        sync_assistant_message_metadata(&mut self.message, &self.source, &self.projected_diagnostics, self.preserve_source_metadata);
    }

    pub fn start_text(&mut self, content_index: usize, source_block: Option<&ContentBlock>) -> usize {
        self.current_inner_text_index = Some(content_index);
        let outer_index = self.message.content.len();
        if self.preserve_source_metadata {
            let block = match source_block {
                Some(ContentBlock::Text(text)) => TextContent { text: String::new(), ..text.clone() },
                _ => TextContent::default(),
            };
            self.message.content.push(ContentBlock::Text(block));
            self.text_block_index_by_inner_index.insert(content_index, Some(outer_index));
            self.last_text_block_index_by_inner_index.insert(content_index, outer_index);
        } else {
            self.text_block_index_by_inner_index.insert(content_index, None);
        }
        self.stream.push(AssistantMessageEvent::TextStart { content_index: outer_index, partial: self.message.clone() });
        outer_index
    }

    pub fn start_thinking(&mut self, content_index: usize, source: &AssistantMessage) -> usize {
        self.thinking.start(&mut self.message, content_index, source)
    }

    pub fn project_thinking_delta(&mut self, content_index: usize, delta: &str, source: &AssistantMessage) {
        self.thinking.delta(&mut self.message, content_index, delta, source);
    }

    pub fn finish_thinking(&mut self, content_index: usize, content: &str, source: &AssistantMessage) {
        self.thinking.end(&mut self.message, content_index, content, source);
    }

    pub fn project_parser_events(&mut self, events: &[StreamParserEvent]) -> ParserProjectionResult {
        let mut saw_tool_call = false;
        let mut completed_tool_calls = Vec::new();
        for event in events {
            match event {
                StreamParserEvent::Text { text } => self.push_text(text),
                StreamParserEvent::ToolcallStart { index, name, id } => {
                    saw_tool_call = true;
                    self.start_tool_call(*index, name, id);
                }
                StreamParserEvent::ToolcallDelta { index, arguments_delta } => {
                    saw_tool_call = true;
                    self.update_tool_call(*index, arguments_delta);
                }
                StreamParserEvent::ToolcallEnd { index, name, id, arguments, incomplete, error_message } => {
                    saw_tool_call = true;
                    if let Some(completed) =
                        self.end_tool_call(*index, name, id, arguments.clone(), *incomplete, error_message.clone())
                    {
                        completed_tool_calls.push(completed);
                    }
                }
            }
        }
        ParserProjectionResult { saw_tool_call, completed_tool_calls }
    }

    pub fn finish_text(&mut self) {
        let outer_content_index = self.current_inner_text_index.and_then(|inner| {
            self.text_block_index_by_inner_index
                .get(&inner)
                .copied()
                .flatten()
                .or_else(|| {
                    if self.preserve_source_metadata {
                        self.last_text_block_index_by_inner_index.get(&inner).copied()
                    } else {
                        None
                    }
                })
        });
        if let Some(outer_content_index) = outer_content_index
            && let Some(ContentBlock::Text(text)) = self.message.content.get(outer_content_index)
        {
            let content = text.text.clone();
            self.stream.push(AssistantMessageEvent::TextEnd {
                content_index: outer_content_index,
                content,
                partial: self.message.clone(),
            });
        }
        self.current_inner_text_index = None;
    }

    pub fn finalize_dangling_tool_calls(&mut self) {
        let dangling: Vec<usize> = self
            .message
            .content
            .iter()
            .enumerate()
            .filter_map(|(index, block)| {
                let is_partial = matches!(block, ContentBlock::ToolCall(_)) && self.partial_tool_calls.contains_key(&index);
                is_partial.then_some(index)
            })
            .collect();
        for content_index in dangling {
            let Some(ContentBlock::ToolCall(existing)) = self.message.content.get(content_index).cloned() else {
                continue;
            };
            let final_tool_call = ToolCall {
                id: existing.id,
                name: existing.name,
                arguments: existing.arguments,
                incomplete: Some(true),
                error_message: Some("Tool call stream ended before completion".into()),
                thought_signature: None,
                namespace: None,
            };
            self.message.content[content_index] = ContentBlock::ToolCall(final_tool_call.clone());
            self.partial_tool_calls.remove(&content_index);
            let stale_parser_indices: Vec<usize> = self
                .tool_call_index_by_parser_index
                .iter()
                .filter(|&(_, &mapped)| mapped == content_index)
                .map(|(&parser_index, _)| parser_index)
                .collect();
            for parser_index in stale_parser_indices {
                self.tool_call_index_by_parser_index.remove(&parser_index);
            }
            self.stream.push(AssistantMessageEvent::ToolcallEnd {
                content_index,
                tool_call: final_tool_call,
                partial: self.message.clone(),
            });
        }
    }

    pub fn finalize(&self, done_message: &AssistantMessage, saw_tool_call: bool) -> AssistantMessage {
        let mut final_message = clone_assistant_message_metadata(
            done_message,
            self.message.content.clone(),
            &self.projected_diagnostics,
            self.preserve_source_metadata,
        );
        if (saw_tool_call || self.has_finalized_tool_call_content())
            && matches!(final_message.stop_reason, crate::types::StopReason::Stop | crate::types::StopReason::Length)
        {
            final_message.stop_reason = crate::types::StopReason::ToolUse;
        }
        final_message
    }

    pub fn has_finalized_tool_call_content(&self) -> bool {
        self.message.content.iter().enumerate().any(|(index, block)| {
            matches!(block, ContentBlock::ToolCall(_)) && !self.partial_tool_calls.contains_key(&index)
        })
    }

    fn push_text(&mut self, text: &str) {
        let Some(inner) = self.current_inner_text_index else { return };
        let mut content_index = self.text_block_index_by_inner_index.get(&inner).copied().flatten();
        if content_index.is_none() {
            let new_index = self.message.content.len();
            self.message.content.push(ContentBlock::Text(TextContent { text: text.to_string(), ..TextContent::default() }));
            self.text_block_index_by_inner_index.insert(inner, Some(new_index));
            self.last_text_block_index_by_inner_index.insert(inner, new_index);
            content_index = Some(new_index);
        } else if let Some(index) = content_index {
            let Some(ContentBlock::Text(block)) = self.message.content.get_mut(index) else { return };
            block.text.push_str(text);
        }
        let Some(content_index) = content_index else { return };
        self.stream.push(AssistantMessageEvent::TextDelta {
            content_index,
            delta: text.to_string(),
            partial: self.message.clone(),
        });
    }

    fn start_tool_call(&mut self, parser_index: usize, name: &str, id: &str) {
        if let Some(inner) = self.current_inner_text_index {
            let text_index = self.text_block_index_by_inner_index.get(&inner).copied().flatten();
            let is_trailing_empty_text = text_index == Some(self.message.content.len().saturating_sub(1))
                && matches!(
                    text_index.and_then(|i| self.message.content.get(i)),
                    Some(ContentBlock::Text(t)) if t.text.is_empty() && t.text_signature.is_none()
                );
            if is_trailing_empty_text {
                self.message.content.pop();
            }
        }
        let content_index = self.message.content.len();
        self.message.content.push(ContentBlock::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: Map::new(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }));
        self.partial_tool_calls.insert(content_index, PartialToolCallState::default());
        self.tool_call_index_by_parser_index.insert(parser_index, content_index);
        if let Some(inner) = self.current_inner_text_index {
            self.text_block_index_by_inner_index.insert(inner, None);
        }
        self.stream.push(AssistantMessageEvent::ToolcallStart { content_index, partial: self.message.clone() });
    }

    fn update_tool_call(&mut self, parser_index: usize, delta: &str) {
        let Some(&content_index) = self.tool_call_index_by_parser_index.get(&parser_index) else { return };
        let Some(state) = self.partial_tool_calls.get_mut(&content_index) else { return };
        state.partial_json.push_str(delta);
        let arguments = parse_streaming_json(Some(&state.partial_json));
        let arguments_map = match arguments {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        if let Some(ContentBlock::ToolCall(tool_call)) = self.message.content.get_mut(content_index) {
            tool_call.arguments = arguments_map;
        }
        self.stream.push(AssistantMessageEvent::ToolcallDelta {
            content_index,
            delta: delta.to_string(),
            partial: self.message.clone(),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn end_tool_call(
        &mut self,
        parser_index: usize,
        name: &str,
        id: &str,
        arguments: Map<String, Value>,
        incomplete: bool,
        error_message: Option<String>,
    ) -> Option<ToolCall> {
        let content_index = self.tool_call_index_by_parser_index.get(&parser_index).copied()?;
        let final_tool_call = ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
            incomplete: incomplete.then_some(true),
            error_message,
            thought_signature: None,
            namespace: None,
        };
        self.message.content[content_index] = ContentBlock::ToolCall(final_tool_call.clone());
        self.partial_tool_calls.remove(&content_index);
        self.tool_call_index_by_parser_index.remove(&parser_index);
        self.stream.push(AssistantMessageEvent::ToolcallEnd {
            content_index,
            tool_call: final_tool_call.clone(),
            partial: self.message.clone(),
        });
        Some(final_tool_call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StopReason, Usage};
    use serde_json::json;

    fn message() -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "m".into(),
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
            timestamp: 0,
        }
    }

    #[tokio::test]
    async fn text_events_accumulate_into_one_block_and_emit_start_delta_end() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        projection.start_text(0, None);
        let result = projection.project_parser_events(&[
            StreamParserEvent::Text { text: "hel".into() },
            StreamParserEvent::Text { text: "lo".into() },
        ]);
        projection.finish_text();
        assert!(!result.saw_tool_call);
        match &projection.message.content[0] {
            ContentBlock::Text(text) => assert_eq!(text.text, "hello"),
            other => panic!("expected text block, got {other:?}"),
        }
        assert!(matches!(stream.next().await, Ok(Some(AssistantMessageEvent::TextStart { .. }))));
        assert!(matches!(stream.next().await, Ok(Some(AssistantMessageEvent::TextDelta { .. }))));
        assert!(matches!(stream.next().await, Ok(Some(AssistantMessageEvent::TextDelta { .. }))));
        assert!(matches!(stream.next().await, Ok(Some(AssistantMessageEvent::TextEnd { .. }))));
    }

    #[tokio::test]
    async fn tool_call_lifecycle_projects_start_delta_end_and_returns_completed() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        let result = projection.project_parser_events(&[
            StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "id-1".into() },
            StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
            StreamParserEvent::ToolcallEnd {
                index: 0,
                name: "get_weather".into(),
                id: "id-1".into(),
                arguments: json!({"city": "Seoul"}).as_object().unwrap().clone(),
                incomplete: false,
                error_message: None,
            },
        ]);
        assert!(result.saw_tool_call);
        assert_eq!(result.completed_tool_calls.len(), 1);
        assert_eq!(result.completed_tool_calls[0].name, "get_weather");
        assert!(!projection.partial_tool_calls.contains_key(&0));
        assert!(projection.has_finalized_tool_call_content());
        let _ = stream.next().await;
        let _ = stream.next().await;
        let _ = stream.next().await;
    }

    #[tokio::test]
    async fn tool_call_start_pops_trailing_empty_text_block() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        projection.start_text(0, None);
        projection.project_parser_events(&[StreamParserEvent::ToolcallStart { index: 0, name: "t".into(), id: "i".into() }]);
        assert_eq!(projection.message.content.len(), 1);
        assert!(matches!(projection.message.content[0], ContentBlock::ToolCall(_)));
        let _ = stream.next().await;
        let _ = stream.next().await;
    }

    #[tokio::test]
    async fn finalize_dangling_tool_calls_marks_incomplete_and_emits_end() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        projection.project_parser_events(&[StreamParserEvent::ToolcallStart { index: 0, name: "t".into(), id: "i".into() }]);
        let _ = stream.next().await;
        projection.finalize_dangling_tool_calls();
        match &projection.message.content[0] {
            ContentBlock::ToolCall(tc) => {
                assert_eq!(tc.incomplete, Some(true));
                assert_eq!(tc.error_message.as_deref(), Some("Tool call stream ended before completion"));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
        assert!(matches!(stream.next().await, Ok(Some(AssistantMessageEvent::ToolcallEnd { .. }))));
        // senpi's finalized block carries no partialJson, so it counts as finalized content.
        assert!(projection.has_finalized_tool_call_content());
    }

    #[test]
    fn finalize_upgrades_stop_and_length_to_tool_use_when_tool_call_seen() {
        let stream = AssistantMessageEventStream::assistant();
        let projection = StreamMessageProjection::new(stream, message(), StreamMessageProjectionOptions::default());
        let mut done = message();
        done.stop_reason = StopReason::Stop;
        let finalized = projection.finalize(&done, true);
        assert_eq!(finalized.stop_reason, StopReason::ToolUse);

        let mut done_length = message();
        done_length.stop_reason = StopReason::Length;
        let finalized_length = projection.finalize(&done_length, true);
        assert_eq!(finalized_length.stop_reason, StopReason::ToolUse);

        let mut done_error = message();
        done_error.stop_reason = StopReason::Error;
        let finalized_error = projection.finalize(&done_error, true);
        assert_eq!(finalized_error.stop_reason, StopReason::Error);
    }

    #[test]
    fn finalize_leaves_stop_reason_alone_when_no_tool_call_seen() {
        let stream = AssistantMessageEventStream::assistant();
        let projection = StreamMessageProjection::new(stream, message(), StreamMessageProjectionOptions::default());
        let mut done = message();
        done.stop_reason = StopReason::Stop;
        let finalized = projection.finalize(&done, false);
        assert_eq!(finalized.stop_reason, StopReason::Stop);
    }
}
