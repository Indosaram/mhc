//! Port of senpi packages/agent/src/response-timing.ts.

use std::time::Instant;

use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageTiming, ContentBlock,
};

use crate::assistant_terminal_state::now_ms;

/// The deltas that count as streamed output for time to first token and the generation window.
pub const STREAMED_DELTA_TYPES: [&str; 3] = ["text_delta", "thinking_delta", "toolcall_delta"];

/// The slice of a message that decides where the generation window starts.
pub struct GenerationWindowMessage<'a> {
    pub reasoning: Option<u64>,
    pub content: &'a [ContentBlock],
}

impl<'a> From<&'a AssistantMessage> for GenerationWindowMessage<'a> {
    fn from(message: &'a AssistantMessage) -> Self {
        Self {
            reasoning: message.usage.reasoning,
            content: &message.content,
        }
    }
}

/// Whether a response generated output before its first streamed delta, so its generation window
/// starts at the response start instead (senpi#3393). Shared by the persisted timing (senpi#3879)
/// and the TPS notice so both measure the same window.
pub fn reasoned_before_first_delta(
    message: &GenerationWindowMessage<'_>,
    thinking_streamed: bool,
) -> bool {
    if message.reasoning.unwrap_or(0) > 0 {
        return true;
    }
    if thinking_streamed {
        return false;
    }
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Thinking(_)))
}

fn delta_type(event: &AssistantMessageEvent) -> Option<&'static str> {
    match event {
        AssistantMessageEvent::TextDelta { .. } => Some("text_delta"),
        AssistantMessageEvent::ThinkingDelta { .. } => Some("thinking_delta"),
        AssistantMessageEvent::ToolcallDelta { .. } => Some("toolcall_delta"),
        _ => None,
    }
}

/// Records one response's timing as the agent loop streams it, then stamps it on the final
/// message (senpi#3879). Wall-clock fields use `now_ms()` so they line up with `timestamp`; the
/// generation window uses the monotonic clock so a clock jump cannot make it negative.
#[derive(Debug, Default)]
pub struct ResponseTimingRecorder {
    requested_at: Option<i64>,
    started: Option<Instant>,
    first_delta: Option<Instant>,
    last_delta: Option<Instant>,
    first_token_at: Option<i64>,
    last_token_at: Option<i64>,
    thinking_streamed: bool,
}

pub fn create_response_timing_recorder() -> ResponseTimingRecorder {
    ResponseTimingRecorder::default()
}

impl ResponseTimingRecorder {
    pub fn requested(&mut self) {
        self.requested_at = Some(now_ms());
    }

    pub fn started(&mut self) {
        self.started.get_or_insert_with(Instant::now);
    }

    pub fn observe(&mut self, event: &AssistantMessageEvent) {
        let Some(kind) = delta_type(event) else {
            return;
        };
        if !STREAMED_DELTA_TYPES.contains(&kind) {
            return;
        }
        let now = Instant::now();
        let at = now_ms();
        if kind == "thinking_delta" {
            self.thinking_streamed = true;
        }
        self.first_delta.get_or_insert(now);
        self.first_token_at.get_or_insert(at);
        self.last_delta = Some(now);
        self.last_token_at = Some(at);
    }

    /// Stamps the final message. A response whose request was never issued gets no timing.
    pub fn stamp(&self, message: &mut AssistantMessage) {
        let Some(requested_at) = self.requested_at else {
            return;
        };
        let window = GenerationWindowMessage::from(&*message);
        let window_start = if reasoned_before_first_delta(&window, self.thinking_streamed) {
            self.started
        } else {
            self.first_delta
        };
        let generation_ms = match (window_start, self.last_delta) {
            (Some(start), Some(last)) if last > start => {
                Some((last.duration_since(start).as_secs_f64() * 1000.0).round() as i64)
            }
            _ => None,
        };
        message.timing = Some(AssistantMessageTiming {
            requested_at,
            first_token_at: self.first_token_at,
            last_token_at: self.last_token_at,
            ended_at: now_ms().max(self.last_token_at.unwrap_or(requested_at)),
            generation_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_usage_starts_window_at_response_start() {
        let window = GenerationWindowMessage {
            reasoning: Some(4),
            content: &[],
        };
        assert!(reasoned_before_first_delta(&window, true));
    }

    #[test]
    fn streamed_thinking_without_usage_keeps_first_delta_window() {
        let content = [ContentBlock::text("x")];
        let window = GenerationWindowMessage {
            reasoning: None,
            content: &content,
        };
        assert!(!reasoned_before_first_delta(&window, true));
        assert!(!reasoned_before_first_delta(&window, false));
    }

    #[test]
    fn stamp_without_request_sets_nothing() {
        let recorder = create_response_timing_recorder();
        let mut message =
            crate::assistant_terminal_state::create_terminal_failure_assistant_message(
                &crate::agent::default_model(),
                maho_ai::types::StopReason::Error,
                &crate::assistant_terminal_state::AgentStreamError::Other {
                    message: "x".into(),
                },
                None,
            );
        recorder.stamp(&mut message);
        assert!(message.timing.is_none());
    }

    #[test]
    fn stamp_after_request_without_deltas_has_no_generation_ms() {
        let mut recorder = create_response_timing_recorder();
        recorder.requested();
        recorder.started();
        let mut message =
            crate::assistant_terminal_state::create_terminal_failure_assistant_message(
                &crate::agent::default_model(),
                maho_ai::types::StopReason::Error,
                &crate::assistant_terminal_state::AgentStreamError::Other {
                    message: "x".into(),
                },
                None,
            );
        recorder.stamp(&mut message);
        let timing = message.timing.expect("timing stamped");
        assert!(timing.generation_ms.is_none());
        assert!(timing.first_token_at.is_none());
        assert!(timing.ended_at >= timing.requested_at);
    }
}
