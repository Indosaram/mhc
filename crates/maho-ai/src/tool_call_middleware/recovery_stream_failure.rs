//! Port of senpi packages/ai/src/tool-call-middleware/recovery-stream-failure.ts.

use serde_json::{Map, Value};

use super::stream_wrapper_shared::StreamMessageProjection;
use super::types::ToolCallFormat;
use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, ErrorReason, StopReason};
use crate::utils::event_stream::AssistantMessageEventStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStreamFailure {
    Collision,
    InvalidContentEventOrder,
    InvalidNativeEventOrder,
}

struct FailureDetails {
    diagnostic_type: &'static str,
    error_message: &'static str,
    status: &'static str,
}

fn failure_details(failure: RecoveryStreamFailure) -> FailureDetails {
    match failure {
        RecoveryStreamFailure::Collision => FailureDetails {
            diagnostic_type: "text_tool_call_recovery_collision",
            error_message: "Tool call ID collision in provider stream",
            status: "collision",
        },
        RecoveryStreamFailure::InvalidContentEventOrder => FailureDetails {
            diagnostic_type: "text_tool_call_recovery_invalid_content_event",
            error_message: "Invalid assistant content event order",
            status: "invalid_content_event_order",
        },
        RecoveryStreamFailure::InvalidNativeEventOrder => FailureDetails {
            diagnostic_type: "text_tool_call_recovery_invalid_native_event",
            error_message: "Invalid native tool call event order",
            status: "invalid_native_event_order",
        },
    }
}

pub struct TerminateRecoveryStreamOptions<'a> {
    pub source: &'a AssistantMessage,
    pub failure: RecoveryStreamFailure,
    pub protocol: ToolCallFormat,
}

pub fn terminate_recovery_stream_for_failure(stream: &AssistantMessageEventStream, projection: &mut StreamMessageProjection, options: TerminateRecoveryStreamOptions<'_>) {
    let details = failure_details(options.failure);
    projection.sync(options.source.clone());
    let mut message = projection.finalize(options.source, false);
    message.content.retain(|block| !matches!(block, ContentBlock::ToolCall(_)));
    message.stop_reason = StopReason::Error;
    message.error_message = Some(details.error_message.to_string());

    let mut diagnostics = options.source.diagnostics.clone().unwrap_or_default();
    let mut detail_map = Map::new();
    detail_map.insert("protocol".to_string(), Value::String(options.protocol.as_str().to_string()));
    detail_map.insert("status".to_string(), Value::String(details.status.to_string()));
    diagnostics.push(crate::types::AssistantMessageDiagnostic { kind: details.diagnostic_type.to_string(), timestamp: crate::utils::diagnostics::now_ms(), error: None, details: Some(detail_map) });
    message.diagnostics = Some(diagnostics);

    stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
    stream.end(Some(message));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::stream_wrapper_shared::StreamMessageProjectionOptions;
    use crate::types::{ToolCall, Usage};

    fn message() -> AssistantMessage {
        AssistantMessage {
            content: vec![ContentBlock::ToolCall(ToolCall { id: "id-1".into(), name: "get_weather".into(), arguments: Map::new(), incomplete: None, error_message: None, thought_signature: None, namespace: None })],
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
    async fn strips_tool_calls_and_emits_an_error_with_diagnostics() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream.clone(), message(), StreamMessageProjectionOptions::default());
        let source = message();
        terminate_recovery_stream_for_failure(&stream, &mut projection, TerminateRecoveryStreamOptions { source: &source, failure: RecoveryStreamFailure::Collision, protocol: ToolCallFormat::Antml });

        let event = stream.next().await.expect("event").expect("some");
        let AssistantMessageEvent::Error { error, .. } = event else { panic!("expected error event") };
        assert!(!error.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
        assert_eq!(error.stop_reason, StopReason::Error);
        let diagnostic = error.diagnostics.as_ref().and_then(|d| d.last()).expect("diagnostic present");
        assert_eq!(diagnostic.kind, "text_tool_call_recovery_collision");
    }
}
