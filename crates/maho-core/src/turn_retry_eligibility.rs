//! Port of senpi packages/coding-agent/src/core/turn-retry-eligibility.ts.

use maho_ai::types::{AssistantMessage, StopReason};
use maho_ai::utils::overflow::is_context_overflow;
use maho_ai::utils::retry::{is_provider_timeout_error, is_retryable_assistant_error};
use maho_ai::utils::stop_details::is_classifier_refusal;

use crate::retry_fallback::model_config_error::is_model_config_failure;

/// Providers mark post-delta failures with this prefix so a turn retry never replays visible text or tool calls.
pub const TURN_RETRY_SUPPRESSION_PREFIX: &str = "senpi:no-turn-retry:";

pub fn is_excluded_from_turn_retry(message: &AssistantMessage, context_window: usize) -> bool {
    if message
        .error_message
        .as_deref()
        .is_some_and(|msg| msg.starts_with(TURN_RETRY_SUPPRESSION_PREFIX))
    {
        return true;
    }
    if message.failure_source == Some(maho_ai::types::AssistantFailureSource::Local) {
        return true;
    }
    if is_model_config_failure(message) {
        return true;
    }
    is_context_overflow(message, Some(context_window as u64))
}

pub fn is_turn_retryable_failure(message: &AssistantMessage, context_window: usize) -> bool {
    if is_excluded_from_turn_retry(message, context_window) {
        return false;
    }
    if is_classifier_refusal(message) {
        return true;
    }
    let Some(err) = message.error_message.as_deref() else {
        return false;
    };
    if err.is_empty() {
        return false;
    }
    if message.stop_reason == StopReason::Aborted {
        return is_provider_timeout_error(message);
    }
    is_retryable_assistant_error(message)
}
