//! Port of senpi packages/ai/src/utils/stop-details.ts.

use crate::types::{AssistantMessage, AssistantStopDetails, StopReason};
#[cfg(test)]
use crate::types::Usage;
use regex::Regex;
use std::sync::LazyLock;

static ANTHROPIC_POLICY_REFUSAL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)This request triggered restrictions on .+? and was blocked under Anthropic's Usage Policy\b")
        .unwrap_or_else(|e| panic!("{e}"))
});

/// Returns true when an assistant response represents a refusal or sensitive stop.
pub fn is_classifier_refusal(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error && message.stop_reason != StopReason::ToolUse {
        return false;
    }
    if matches!(message.stop_details, Some(AssistantStopDetails::Refusal { .. } | AssistantStopDetails::Sensitive)) {
        return true;
    }
    message.error_message.as_deref().is_some_and(|text| ANTHROPIC_POLICY_REFUSAL_PATTERN.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANTHROPIC_POLICY_REFUSAL: &str = "This request triggered restrictions on violative cyber content and was blocked under Anthropic's Usage Policy. To learn more, see https://platform.claude.com/docs/en/build-with-claude/refusals-and-fallback.";

    /// Port of `fauxAssistantMessage("", options)` (providers/faux.ts): a plain builder over the
    /// faux provider's zero-usage default, no wire/registration machinery involved.
    fn message(stop: StopReason, error: Option<&str>, details: Option<AssistantStopDetails>) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "faux".into(),
            provider: "faux".into(),
            model: "faux-1".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: stop,
            stop_details: details,
            deferred: None,
            error_message: error.map(str::to_owned),
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    // "maps Anthropic refusal and sensitive stops to typed error details" and
    // "leaves successful and length-limited Anthropic stops unclassified" (stop-details.test.ts)
    // drive `streamAnthropic(...).result()` over a faked SSE `Response` through
    // `api/anthropic-messages.ts` end to end - real wire/SSE parsing owned by todos 10-13's api/.
    // "passes classifier details through faux error stream events and excludes them from retry"
    // drives `registerFauxProvider()` + `stream()` through the provider registry/dispatch in
    // `providers/faux.ts` and `compat.ts` - owned by todos 10-13's providers/. All three are
    // excluded here; `is_classifier_refusal`'s own logic is fully covered by the four tests below.

    #[test]
    fn recognizes_typed_classifier_details_on_error_and_tool_use_stops() {
        let classifier_refusal = message(StopReason::Error, None, Some(AssistantStopDetails::Refusal { explanation: None }));
        let classifier_sensitive = message(StopReason::Error, None, Some(AssistantStopDetails::Sensitive));
        let tool_use_refusal = message(
            StopReason::ToolUse,
            Some(ANTHROPIC_POLICY_REFUSAL),
            Some(AssistantStopDetails::Refusal { explanation: Some(ANTHROPIC_POLICY_REFUSAL.into()) }),
        );
        let non_error = message(StopReason::Stop, None, Some(AssistantStopDetails::Refusal { explanation: None }));
        let absent = message(StopReason::Error, None, None);

        assert!(is_classifier_refusal(&classifier_refusal));
        assert!(is_classifier_refusal(&classifier_sensitive));
        assert!(is_classifier_refusal(&tool_use_refusal));
        assert!(!is_classifier_refusal(&non_error));
        assert!(!is_classifier_refusal(&absent));
    }

    #[test]
    fn recognizes_the_legacy_anthropic_policy_block_when_typed_details_are_absent() {
        let refusal = message(
            StopReason::Error,
            Some(&format!(
                "{ANTHROPIC_POLICY_REFUSAL} API integrators: you can reduce refusals for your users by configuring a fallback model."
            )),
            None,
        );
        assert!(is_classifier_refusal(&refusal));
    }

    #[test]
    fn does_not_infer_refusal_from_ordinary_anthropic_policy_documentation_errors() {
        let ordinary_policy_error = message(
            StopReason::Error,
            Some("Provider configuration failed. Review Anthropic's Usage Policy at https://platform.claude.com/docs/en/build-with-claude/refusals-and-fallback."),
            None,
        );
        assert!(!is_classifier_refusal(&ordinary_policy_error));
    }

    /// Covers the `isClassifierRefusal` assertions of "leaves successful and length-limited
    /// Anthropic stops unclassified"; the `stopReason`/`stopDetails` SSE-derived assertions in
    /// that TS case need `streamAnthropic` (api/, todos 10-13) and are excluded.
    #[test]
    fn leaves_successful_and_length_limited_stops_unclassified() {
        let completed = message(StopReason::Stop, None, None);
        let length_limited = message(StopReason::Length, None, None);
        assert!(!is_classifier_refusal(&completed));
        assert!(!is_classifier_refusal(&length_limited));
    }
}
