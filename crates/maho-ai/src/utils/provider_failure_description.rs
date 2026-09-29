//! Port of senpi packages/ai/src/utils/provider-failure-description.ts.

use crate::utils::retry::{ProviderStallDescriptionOptions, StallRecovery, describe_provider_stall_for_user, format_stall_duration, retried_sentence};
use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

pub const TURN_RETRY_SUPPRESSION_PREFIX: &str = "senpi:no-turn-retry:";

pub fn strip_turn_retry_suppression_prefix(message: &str) -> String {
    message.replace(TURN_RETRY_SUPPRESSION_PREFIX, "")
}

fn ci(pattern: &str) -> Regex {
    RegexBuilder::new(pattern).case_insensitive(true).build().unwrap_or_else(|e| panic!("failure pattern: {e}"))
}

static WEBSOCKET_CLOSED_PATTERN: LazyLock<Regex> = LazyLock::new(|| ci(r"^WebSocket closed\b"));
static WEBSOCKET_ERROR_PATTERN: LazyLock<Regex> = LazyLock::new(|| ci(r"^WebSocket error$"));
static WEBSOCKET_CONNECT_TIMEOUT_PATTERN: LazyLock<Regex> = LazyLock::new(|| ci(r"^WebSocket connect timeout after (\d+)ms"));

fn recovery_sentence(recovery: StallRecovery) -> &'static str {
    match recovery {
        StallRecovery::NoFallbackConfigured => {
            "No fallback model is configured for it, so nothing could take the turn over: run /fallback to add one, or send the message again to continue from the partial reply."
        }
        StallRecovery::ChainExhausted => {
            "Every model in its fallback chain was tried as well: run /fallback to review the chain, or send the message again to continue from the partial reply."
        }
    }
}

fn transport_symptom(error_message: &str, subject: &str) -> Option<String> {
    if WEBSOCKET_CLOSED_PATTERN.is_match(error_message) {
        return Some(format!("The connection to {subject} dropped before the reply finished ({error_message})."));
    }
    if WEBSOCKET_ERROR_PATTERN.is_match(error_message) {
        return Some(format!("The connection to {subject} reported an error before the reply finished ({error_message})."));
    }
    let captures = WEBSOCKET_CONNECT_TIMEOUT_PATTERN.captures(error_message)?;
    let provider = match subject.strip_prefix("the provider") {
        Some(rest) => format!("The provider{rest}"),
        None => subject.to_owned(),
    };
    let duration = format_stall_duration(captures[1].parse().unwrap_or(u64::MAX));
    Some(format!("{provider} did not accept the connection within {duration}."))
}

pub fn describe_provider_failure_for_user(error_message: Option<&str>, options: &ProviderStallDescriptionOptions) -> Option<String> {
    let stripped = strip_turn_retry_suppression_prefix(error_message.filter(|m| !m.is_empty())?);
    let message = crate::utils::js::trim(&stripped);
    if message.is_empty() {
        return None;
    }
    if let Some(stall) = describe_provider_stall_for_user(Some(message), options) {
        return Some(stall);
    }
    let subject = options.model.as_ref().map_or_else(|| "the provider".to_owned(), |m| format!("the provider for {m}"));
    let mut sentences = vec![transport_symptom(message, &subject)?];
    let attempts = options.attempts.unwrap_or(0);
    if attempts > 0 {
        sentences.push(retried_sentence(attempts));
    }
    if let Some(recovery) = options.recovery {
        sentences.push(recovery_sentence(recovery).to_owned());
    }
    Some(sentences.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_transport_failures() {
        let options = ProviderStallDescriptionOptions { attempts: Some(1), model: None, recovery: Some(StallRecovery::ChainExhausted) };
        assert_eq!(
            describe_provider_failure_for_user(Some("senpi:no-turn-retry:WebSocket closed 1006"), &options).as_deref(),
            Some("The connection to the provider dropped before the reply finished (WebSocket closed 1006). Retried 1 time on the same model with the same result. Every model in its fallback chain was tried as well: run /fallback to review the chain, or send the message again to continue from the partial reply.")
        );
        let model = ProviderStallDescriptionOptions { model: Some("m".into()), ..Default::default() };
        assert_eq!(
            describe_provider_failure_for_user(Some("WebSocket connect timeout after 15000ms"), &model).as_deref(),
            Some("The provider for m did not accept the connection within 15s.")
        );
        assert_eq!(describe_provider_failure_for_user(Some("boom"), &model), None);
        assert_eq!(describe_provider_failure_for_user(Some(" senpi:no-turn-retry: "), &model), None);
        assert!(describe_provider_failure_for_user(Some("Idle timeout waiting for provider stream after 500ms"), &model).is_some());
    }
}
