//! Port of senpi packages/ai/src/tool-call-middleware/recovery-diagnostics.ts.

use serde_json::{Map, Value};

use super::stream_wrapper_shared::StreamMessageProjection;
use super::types::ToolCallFormat;
use crate::types::{AssistantMessageDiagnostic, ToolCall};

pub fn append_recovery_diagnostic(projection: &mut StreamMessageProjection, tool_call: &ToolCall, protocol: ToolCallFormat) {
    let mut details = Map::new();
    details.insert("protocol".to_string(), Value::String(protocol.as_str().to_string()));
    details.insert("toolCallId".to_string(), Value::String(tool_call.id.clone()));
    details.insert("toolCallName".to_string(), Value::String(tool_call.name.clone()));
    if tool_call.incomplete.unwrap_or(false) {
        details.insert("incomplete".to_string(), Value::Bool(true));
    }
    projection.append_diagnostic(AssistantMessageDiagnostic {
        kind: "text_tool_call_recovered".to_string(),
        timestamp: crate::utils::now_millis(),
        error: None,
        details: Some(details),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::stream_wrapper_shared::StreamMessageProjectionOptions;
    use crate::types::{AssistantMessage, StopReason, Usage};
    use crate::utils::event_stream::AssistantMessageEventStream;

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

    #[test]
    fn records_protocol_and_tool_call_identity() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream, message(), StreamMessageProjectionOptions::default());
        let tool_call = ToolCall { id: "id-1".into(), name: "get_weather".into(), arguments: Map::new(), incomplete: None, error_message: None, thought_signature: None, namespace: None };
        append_recovery_diagnostic(&mut projection, &tool_call, ToolCallFormat::Antml);
        let diagnostic = projection.message.diagnostics.as_ref().and_then(|d| d.last()).expect("diagnostic recorded");
        assert_eq!(diagnostic.kind, "text_tool_call_recovered");
        let details = diagnostic.details.as_ref().expect("details present");
        assert_eq!(details.get("protocol"), Some(&Value::String("antml".into())));
        assert_eq!(details.get("toolCallId"), Some(&Value::String("id-1".into())));
    }

    #[test]
    fn flags_incomplete_tool_calls() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = StreamMessageProjection::new(stream, message(), StreamMessageProjectionOptions::default());
        let tool_call = ToolCall { id: "id-2".into(), name: "get_weather".into(), arguments: Map::new(), incomplete: Some(true), error_message: None, thought_signature: None, namespace: None };
        append_recovery_diagnostic(&mut projection, &tool_call, ToolCallFormat::Antml);
        let diagnostic = projection.message.diagnostics.as_ref().and_then(|d| d.last()).expect("diagnostic recorded");
        assert_eq!(diagnostic.details.as_ref().unwrap().get("incomplete"), Some(&Value::Bool(true)));
    }
}
