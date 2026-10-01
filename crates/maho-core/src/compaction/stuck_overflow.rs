//! Port of senpi `packages/coding-agent/src/core/compaction/stuck-overflow.ts`.

use maho_ai::types::{AssistantMessage, StopReason};
use maho_ai::utils::overflow::is_context_overflow;

/// `isTurnStuckOnContextOverflow`.
pub fn is_turn_stuck_on_context_overflow(message: &AssistantMessage, context_window: u64) -> bool {
    if !is_context_overflow(message, Some(context_window)) {
        return false;
    }
    if message.stop_reason == StopReason::Error {
        return true;
    }
    message.stop_reason == StopReason::Length && message.usage.output == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::{Usage, UsageCost};

    fn message(stop_reason: StopReason, error_message: Option<&str>, output: u64) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "anthropic-messages".to_string(),
            provider: "anthropic".to_string(),
            model: "claude".to_string(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage { input: 200_000, output, cache_read: 0, cache_write: 0, cache_write_1h: None, reasoning: None, total_tokens: 200_000 + output, cost: UsageCost::default() },
            stop_reason,
            stop_details: None,
            deferred: None,
            error_message: error_message.map(str::to_string),
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    #[test]
    fn an_error_that_reads_as_an_overflow_is_stuck() {
        assert!(is_turn_stuck_on_context_overflow(&message(StopReason::Error, Some("prompt is too long: 200000 tokens > 200000 maximum"), 0), 200_000));
    }

    #[test]
    fn a_zero_output_length_stop_is_stuck() {
        assert!(is_turn_stuck_on_context_overflow(&message(StopReason::Length, None, 0), 200_000));
    }

    #[test]
    fn a_length_stop_that_produced_output_is_not_stuck() {
        assert!(!is_turn_stuck_on_context_overflow(&message(StopReason::Length, None, 120), 200_000));
    }

    #[test]
    fn an_ordinary_stop_is_never_stuck() {
        assert!(!is_turn_stuck_on_context_overflow(&message(StopReason::Stop, None, 0), 200_000));
        assert!(!is_turn_stuck_on_context_overflow(&message(StopReason::Error, Some("connection reset"), 0), 200_000));
    }
}
