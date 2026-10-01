//! Port of senpi packages/ai/src/tool-call-middleware/recovery-stream-terminal.ts.

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, DoneReason, ErrorReason, StopReason};
use crate::utils::event_stream::{AssistantMessageEventStream, StreamError};
use crate::utils::server_fallback_receipt::{parse_server_fallback_receipt, SERVER_FALLBACK_ABORTED_DIAGNOSTIC};

use super::stream_wrapper_shared::StreamMessageProjection;

fn discard_superseded_tools(message: &mut AssistantMessage) -> bool {
    let boundary = message.content.iter().enumerate().rev().find_map(|(index, block)| match block {
        ContentBlock::ProviderNative(native) if parse_server_fallback_receipt(&native.raw).is_some() => Some(index),
        _ => None,
    });
    let Some(boundary) = boundary else { return false };
    let mut kept = Vec::with_capacity(message.content.len());
    for (index, block) in message.content.drain(..).enumerate() {
        if index > boundary || !matches!(block, ContentBlock::ToolCall(_)) {
            kept.push(block);
        }
    }
    message.content = kept;
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoneOrLengthOrToolUse {
    Stop,
    Length,
    ToolUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortedOrError {
    Aborted,
    Error,
}

pub struct RecoveryStreamTerminal {
    emitted: bool,
    stream: AssistantMessageEventStream,
}

impl RecoveryStreamTerminal {
    pub fn new(stream: AssistantMessageEventStream) -> Self {
        Self { emitted: false, stream }
    }

    pub fn done(&mut self, projection: &mut StreamMessageProjection, source: &AssistantMessage, saw_tool_call: bool, reason: DoneOrLengthOrToolUse) {
        if self.emitted {
            return;
        }
        projection.finalize_dangling_tool_calls();
        let mut message = projection.finalize(source, saw_tool_call);
        let superseded = discard_superseded_tools(&mut message);
        let recovered = (!superseded && saw_tool_call) || message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_)));
        let final_reason = if recovered && matches!(reason, DoneOrLengthOrToolUse::Stop | DoneOrLengthOrToolUse::Length) {
            DoneReason::ToolUse
        } else {
            match reason {
                DoneOrLengthOrToolUse::Stop => DoneReason::Stop,
                DoneOrLengthOrToolUse::Length => DoneReason::Length,
                DoneOrLengthOrToolUse::ToolUse => DoneReason::ToolUse,
            }
        };
        message.stop_reason = match final_reason {
            DoneReason::Stop => StopReason::Stop,
            DoneReason::Length => StopReason::Length,
            DoneReason::ToolUse => StopReason::ToolUse,
            DoneReason::Deferred => StopReason::Deferred,
        };
        self.emit_done(final_reason, message);
    }

    pub fn source_error(&mut self, projection: &mut StreamMessageProjection, source: &AssistantMessage, saw_tool_call: bool, reason: AbortedOrError) {
        if self.emitted {
            return;
        }
        if source.diagnostics.as_ref().is_some_and(|diagnostics| diagnostics.iter().any(|entry| entry.kind == SERVER_FALLBACK_ABORTED_DIAGNOSTIC)) {
            let error_reason = match reason {
                AbortedOrError::Aborted => ErrorReason::Aborted,
                AbortedOrError::Error => ErrorReason::Error,
            };
            self.emit_error(error_reason, source.clone());
            return;
        }
        discard_superseded_tools(&mut projection.message);
        projection.finalize_dangling_tool_calls();
        let mut message = projection.finalize(source, saw_tool_call);
        if reason == AbortedOrError::Aborted {
            message.content.retain(|block| !matches!(block, ContentBlock::ToolCall(_)));
            message.stop_reason = StopReason::Aborted;
            message.error_message.get_or_insert_with(|| "Request was aborted".to_string());
            self.emit_error(ErrorReason::Aborted, message);
            return;
        }
        if projection.has_finalized_tool_call_content() {
            message.stop_reason = StopReason::ToolUse;
            self.emit_done(DoneReason::ToolUse, message);
            return;
        }
        message.stop_reason = StopReason::Error;
        self.emit_error(ErrorReason::Error, message);
    }

    pub fn iterator_failure(&mut self, projection: Option<&mut StreamMessageProjection>, error: StreamError) {
        if self.emitted {
            return;
        }
        let Some(projection) = projection else {
            self.emitted = true;
            self.stream.fail(error);
            return;
        };
        projection.finalize_dangling_tool_calls();
        let message_snapshot = projection.message.clone();
        let mut message = projection.finalize(&message_snapshot, false);
        message.stop_reason = StopReason::Error;
        let text = error.message.clone();
        message.error_message = Some(if text.is_empty() { "Assistant message stream failed".to_string() } else { text });
        self.emit_error(ErrorReason::Error, message);
    }

    pub fn exhausted(&mut self, projection: Option<&mut StreamMessageProjection>) {
        if self.emitted {
            return;
        }
        let Some(projection) = projection else {
            self.iterator_failure(None, StreamError::new("Assistant message stream ended without a terminal event"));
            return;
        };
        projection.finalize_dangling_tool_calls();
        let message_snapshot = projection.message.clone();
        let mut message = projection.finalize(&message_snapshot, false);
        message.stop_reason = StopReason::Error;
        message.error_message = Some("Assistant message stream ended without a terminal event".to_string());
        self.emit_error(ErrorReason::Error, message);
    }

    pub fn cancelled(&mut self, projection: Option<&mut StreamMessageProjection>) {
        if self.emitted {
            return;
        }
        let Some(projection) = projection else {
            self.iterator_failure(None, StreamError::new("Assistant message stream consumption was cancelled"));
            return;
        };
        projection.finalize_dangling_tool_calls();
        let message_snapshot = projection.message.clone();
        let mut message = projection.finalize(&message_snapshot, false);
        message.content.retain(|block| !matches!(block, ContentBlock::ToolCall(_)));
        message.stop_reason = StopReason::Error;
        message.error_message = Some("Assistant message stream consumption was cancelled".to_string());
        self.emit_error(ErrorReason::Error, message);
    }

    fn emit_done(&mut self, reason: DoneReason, message: AssistantMessage) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        self.stream.push(AssistantMessageEvent::Done { reason, message: message.clone() });
        self.stream.end(Some(message));
    }

    fn emit_error(&mut self, reason: ErrorReason, error: AssistantMessage) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        self.stream.push(AssistantMessageEvent::Error { reason, error: error.clone() });
        self.stream.end(Some(error));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::stream_wrapper_shared::StreamMessageProjectionOptions;
    use crate::types::{ToolCall, Usage};
    use serde_json::Map;

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
    async fn done_promotes_stop_to_tool_use_when_a_tool_call_was_recovered() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        let mut terminal = RecoveryStreamTerminal::new(stream.clone());
        terminal.done(&mut projection, &message(), true, DoneOrLengthOrToolUse::Stop);
        let event = stream.next().await.expect("event").expect("some");
        let AssistantMessageEvent::Done { reason, .. } = event else { panic!("expected done event") };
        assert_eq!(reason, DoneReason::ToolUse);
    }

    #[tokio::test]
    async fn done_keeps_stop_when_no_tool_call_was_recovered() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        let mut terminal = RecoveryStreamTerminal::new(stream.clone());
        terminal.done(&mut projection, &message(), false, DoneOrLengthOrToolUse::Stop);
        let event = stream.next().await.expect("event").expect("some");
        let AssistantMessageEvent::Done { reason, .. } = event else { panic!("expected done event") };
        assert_eq!(reason, DoneReason::Stop);
    }

    #[tokio::test]
    async fn source_error_aborted_strips_tool_calls_and_sets_default_message() {
        let mut source = message();
        source.content.push(ContentBlock::ToolCall(ToolCall { id: "id-1".into(), name: "t".into(), arguments: Map::new(), incomplete: None, error_message: None, thought_signature: None, namespace: None }));
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), source.clone(), StreamMessageProjectionOptions::default());
        let mut terminal = RecoveryStreamTerminal::new(stream.clone());
        terminal.source_error(&mut projection, &source, false, AbortedOrError::Aborted);
        let event = stream.next().await.expect("event").expect("some");
        let AssistantMessageEvent::Error { error, .. } = event else { panic!("expected error event") };
        assert_eq!(error.stop_reason, StopReason::Aborted);
        assert_eq!(error.error_message.as_deref(), Some("Request was aborted"));
        assert!(!error.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
    }

    #[tokio::test]
    async fn emit_is_exactly_once() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        let mut terminal = RecoveryStreamTerminal::new(stream.clone());
        terminal.done(&mut projection, &message(), false, DoneOrLengthOrToolUse::Stop);
        terminal.exhausted(Some(&mut projection));
        let _ = stream.next().await;
        assert_eq!(stream.next().await, Ok(None));
    }
}
