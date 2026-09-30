//! Setup-error streams shared by registries and the models collection (the error envelope
//! senpi's api/lazy.ts builds; the lazy loader itself is ported with the wire APIs).

use crate::types::{AssistantMessage, AssistantMessageEvent, ErrorReason, Model, StopReason, Usage};
use crate::utils::diagnostics::now_ms;
use crate::utils::event_stream::AssistantMessageEventStream;

pub fn setup_error_message(model: &Model, message: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        stop_details: None,
        deferred: None,
        error_message: Some(message.to_owned()),
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    }
}

/// A stream that immediately terminates with an error event for `model`.
pub fn error_stream(model: &Model, message: &str) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    let error = setup_error_message(model, message);
    stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: error.clone() });
    stream.end(Some(error));
    stream
}
