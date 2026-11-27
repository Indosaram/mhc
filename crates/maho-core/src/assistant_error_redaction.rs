//! Port of senpi packages/coding-agent/src/core/assistant-error-redaction.ts.

use maho_agent::types::{AgentEvent, AgentMessage};
use maho_ai::types::{AssistantMessage, AssistantMessageEvent, Message};

use crate::sensitive_output::redact_sensitive_token_values;

fn redact_assistant_error(message: &mut AssistantMessage) {
    if let Some(ref mut err) = message.error_message {
        *err = redact_sensitive_token_values(err, "[REDACTED]");
    }
}

fn redact_agent_message_error(message: &mut AgentMessage) {
    if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
        redact_assistant_error(std::sync::Arc::make_mut(assistant));
    }
}

/// Redacts sensitive token values from assistant error messages in place on streamed and terminal
/// message events before listeners, transcripts, or state observe them (`omo#12965`).
pub fn redact_assistant_error_text(event: &mut AgentEvent) {
    match event {
        AgentEvent::MessageStart { message, .. } | AgentEvent::MessageEnd { message } => {
            redact_agent_message_error(message);
        }
        AgentEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            redact_agent_message_error(message);
            match assistant_message_event {
                AssistantMessageEvent::Start { partial }
                | AssistantMessageEvent::TextStart { partial, .. }
                | AssistantMessageEvent::TextDelta { partial, .. }
                | AssistantMessageEvent::TextEnd { partial, .. }
                | AssistantMessageEvent::ThinkingStart { partial, .. }
                | AssistantMessageEvent::ThinkingDelta { partial, .. }
                | AssistantMessageEvent::ThinkingEnd { partial, .. }
                | AssistantMessageEvent::ToolcallStart { partial, .. }
                | AssistantMessageEvent::ToolcallDelta { partial, .. }
                | AssistantMessageEvent::ToolcallEnd { partial, .. } => {
                    redact_assistant_error(partial);
                }
                AssistantMessageEvent::Done { message, .. } => {
                    redact_assistant_error(message);
                }
                AssistantMessageEvent::Error { error, .. } => {
                    redact_assistant_error(error);
                }
            }
        }
        _ => {}
    }
}
