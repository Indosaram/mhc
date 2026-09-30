//! Port of senpi packages/ai/src/tool-call-middleware/stream-thinking-projection.ts.

use std::collections::HashMap;

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, ThinkingContent};
use crate::utils::event_stream::AssistantMessageEventStream;

fn clone_thinking(block: &ThinkingContent, thinking: String) -> ThinkingContent {
    ThinkingContent { thinking, ..block.clone() }
}

/// Replays canonical thinking events without exposing their content to text parsers.
pub struct StreamThinkingProjection {
    stream: AssistantMessageEventStream,
    outer_index_by_inner_index: HashMap<usize, usize>,
    preserve_source_metadata: bool,
}

impl StreamThinkingProjection {
    pub fn new(stream: AssistantMessageEventStream, preserve_source_metadata: bool) -> Self {
        Self { stream, outer_index_by_inner_index: HashMap::new(), preserve_source_metadata }
    }

    pub fn start(&mut self, message: &mut AssistantMessage, inner_index: usize, source: &AssistantMessage) -> usize {
        let source_block = source.content.get(inner_index);
        let block = match source_block {
            Some(ContentBlock::Thinking(thinking)) if self.preserve_source_metadata => {
                clone_thinking(thinking, thinking.thinking.clone())
            }
            _ => ThinkingContent::default(),
        };
        let outer_index = message.content.len();
        message.content.push(ContentBlock::Thinking(block));
        self.outer_index_by_inner_index.insert(inner_index, outer_index);
        self.stream.push(AssistantMessageEvent::ThinkingStart { content_index: outer_index, partial: message.clone() });
        outer_index
    }

    pub fn delta(&mut self, message: &mut AssistantMessage, inner_index: usize, delta: &str, source: &AssistantMessage) {
        let Some(&outer_index) = self.outer_index_by_inner_index.get(&inner_index) else { return };
        let Some(ContentBlock::Thinking(current)) = message.content.get(outer_index) else { return };
        let source_block = source.content.get(inner_index);
        let updated = match source_block {
            Some(ContentBlock::Thinking(thinking)) if self.preserve_source_metadata => {
                clone_thinking(thinking, thinking.thinking.clone())
            }
            _ => clone_thinking(current, format!("{}{delta}", current.thinking)),
        };
        message.content[outer_index] = ContentBlock::Thinking(updated);
        self.stream.push(AssistantMessageEvent::ThinkingDelta {
            content_index: outer_index,
            delta: delta.to_string(),
            partial: message.clone(),
        });
    }

    pub fn end(&mut self, message: &mut AssistantMessage, inner_index: usize, content: &str, source: &AssistantMessage) {
        let Some(&outer_index) = self.outer_index_by_inner_index.get(&inner_index) else { return };
        let Some(ContentBlock::Thinking(current)) = message.content.get(outer_index) else { return };
        let source_block = source.content.get(inner_index);
        let updated = match source_block {
            Some(ContentBlock::Thinking(thinking)) if self.preserve_source_metadata => {
                clone_thinking(thinking, content.to_string())
            }
            _ => clone_thinking(current, content.to_string()),
        };
        message.content[outer_index] = ContentBlock::Thinking(updated);
        self.stream.push(AssistantMessageEvent::ThinkingEnd {
            content_index: outer_index,
            content: content.to_string(),
            partial: message.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StopReason, Usage};

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
    async fn start_delta_end_projects_thinking_and_pushes_events() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamThinkingProjection::new(stream.clone(), false);
        let mut message = message();
        let source = message.clone();

        let outer = projection.start(&mut message, 0, &source);
        assert_eq!(outer, 0);
        projection.delta(&mut message, 0, "ab", &source);
        projection.delta(&mut message, 0, "cd", &source);
        projection.end(&mut message, 0, "abcd", &source);

        let events = [
            stream.next().await.expect("start"),
            stream.next().await.expect("delta1"),
            stream.next().await.expect("delta2"),
            stream.next().await.expect("end"),
        ];
        assert!(matches!(events[0], Some(AssistantMessageEvent::ThinkingStart { content_index: 0, .. })));
        assert!(matches!(&events[1], Some(AssistantMessageEvent::ThinkingDelta { content_index: 0, delta, .. }) if delta == "ab"));
        assert!(matches!(&events[3], Some(AssistantMessageEvent::ThinkingEnd { content_index: 0, content, .. }) if content == "abcd"));
        match &message.content[0] {
            ContentBlock::Thinking(thinking) => assert_eq!(thinking.thinking, "abcd"),
            other => panic!("expected thinking block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preserve_source_metadata_clones_source_block_verbatim() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamThinkingProjection::new(stream.clone(), true);
        let mut message = message();
        let mut source = message.clone();
        source.content.push(ContentBlock::Thinking(ThinkingContent {
            thinking: "source-thinking".into(),
            thinking_signature: Some("sig".into()),
            ..ThinkingContent::default()
        }));

        projection.start(&mut message, 0, &source);
        match &message.content[0] {
            ContentBlock::Thinking(thinking) => {
                assert_eq!(thinking.thinking, "source-thinking");
                assert_eq!(thinking.thinking_signature.as_deref(), Some("sig"));
            }
            other => panic!("expected thinking block, got {other:?}"),
        }
        let _ = stream.next().await;
    }

    #[tokio::test]
    async fn delta_and_end_on_unknown_inner_index_are_noops() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamThinkingProjection::new(stream.clone(), false);
        let mut message = message();
        let source = message.clone();
        projection.delta(&mut message, 5, "x", &source);
        projection.end(&mut message, 5, "x", &source);
        assert!(message.content.is_empty());
    }
}
