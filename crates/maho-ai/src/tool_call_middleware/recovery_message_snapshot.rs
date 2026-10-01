//! Port of senpi packages/ai/src/tool-call-middleware/recovery-message-snapshot.ts.
//! senpi's `partialJson` scratch field is a JS-only dynamic property bolted onto a
//! `ToolCall` object; maho-ai's `ToolCall` struct has no such field; ownership already
//! gives every event its own message graph, so there is nothing left to clone or strip.

use crate::types::{AssistantMessage, AssistantMessageEvent};

fn sanitize_message(_message: &mut AssistantMessage) {}

pub fn snapshot_recovery_event(event: AssistantMessageEvent) -> AssistantMessageEvent {
    let mut snapshot = event;
    match &mut snapshot {
        AssistantMessageEvent::Done { message, .. } => sanitize_message(message),
        AssistantMessageEvent::Error { error, .. } => sanitize_message(error),
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => sanitize_message(partial),
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DoneReason, StopReason, Usage};

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
            stop_reason: StopReason::Stop,
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
    fn snapshot_of_a_done_event_preserves_message_identity_fields() {
        let event = AssistantMessageEvent::Done { reason: DoneReason::Stop, message: message() };
        let snapshot = snapshot_recovery_event(event);
        let AssistantMessageEvent::Done { message, .. } = snapshot else { panic!("expected done event") };
        assert_eq!(message.model, "m");
    }

    #[test]
    fn snapshot_of_a_start_event_preserves_the_partial_message() {
        let event = AssistantMessageEvent::Start { partial: message() };
        let snapshot = snapshot_recovery_event(event);
        let AssistantMessageEvent::Start { partial } = snapshot else { panic!("expected start event") };
        assert_eq!(partial.provider, "openai");
    }
}
