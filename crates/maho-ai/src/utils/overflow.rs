//! Port of senpi packages/ai/src/utils/overflow.ts.

use crate::types::{AssistantMessage, StopReason, Usage};
use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

fn build(pattern: &str, case_insensitive: bool) -> Regex {
    RegexBuilder::new(pattern).case_insensitive(case_insensitive).build().unwrap_or_else(|e| panic!("overflow pattern: {e}"))
}

/// (pattern, case-insensitive) pairs in TS order.
const OVERFLOW_SOURCES: &[(&str, bool)] = &[
    (r"^Context window exhausted: ", false),
    (r"prompt is too long", true),
    (r"request_too_large", true),
    (r"input is too long for requested model", true),
    (r"exceeds (?:(?:the|this) )?(?:model'?s )?context window", true),
    (r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", true),
    (r"input token count.*exceeds the maximum", true),
    (r"maximum prompt length is \d+", true),
    (r"reduce the length of the messages", true),
    (r"maximum context length is \d+ tokens", true),
    (r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", true),
    (r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", true),
    (r"exceeds the limit of \d+", true),
    (r"exceeds the available context size", true),
    (r"greater than the context length", true),
    (r"context window exceeds limit", true),
    (r"exceeded model token limit", true),
    (r"too large for model with \d+ maximum context length", true),
    (r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?", true),
    (r"model_context_window_exceeded", true),
    (r"prompt too long; exceeded (?:max )?context length", true),
    (r"range of input length should be", true),
    (r"context[_ ]length[_ ]exceeded", true),
    (r"too many tokens", true),
    (r"token limit exceeded", true),
    (r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)", true),
    (r"(?:request[ _])?(?:body|entity|payload)[_ ]too[_ ]large", true),
    (r"Request payload is \d+ (?:bytes, over the \d+ byte|tokens, over the \d+ token) limit Kiro accepts\.", false),
    (r"Model context limit reached\. Conversation size exceeds model capacity\.", false),
];

const NON_OVERFLOW_SOURCES: &[(&str, bool)] = &[
    (r"^(Throttling error|Service unavailable):", true),
    (r"rate limit", true),
    (r"too many requests", true),
    (r"tokens per (?:min|minute|hour|day)", true),
    (r"\bTPM\b", true),
    (r"\bRPM\b", true),
    (r"quota exceeded", true),
    (r"retry (?:after|in) \d", true),
    (r"^429\b", false),
    (r"status code 429", true),
    (r"overloaded", true),
];

static OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| OVERFLOW_SOURCES.iter().map(|(p, i)| build(p, *i)).collect());
static NON_OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| NON_OVERFLOW_SOURCES.iter().map(|(p, i)| build(p, *i)).collect());
static RESOURCE_EXHAUSTED_PATTERN: LazyLock<Regex> = LazyLock::new(|| build(r"resource.?exhausted", true));

fn token_count(usage: &Usage) -> u64 {
    if usage.total_tokens != 0 { usage.total_tokens } else { usage.input + usage.output + usage.cache_read + usage.cache_write }
}

pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error
        && let Some(error) = message.error_message.as_deref().filter(|e| !e.is_empty())
    {
        let non_overflow = NON_OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error));
        if !non_overflow && OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error)) {
            return true;
        }
        let tokens = token_count(&message.usage);
        if !non_overflow
            && tokens > 0
            && RESOURCE_EXHAUSTED_PATTERN.is_match(error)
            && context_window.is_none_or(|window| window == 0 || tokens as f64 >= window as f64 * 0.5)
        {
            return true;
        }
    }
    let Some(window) = context_window.filter(|w| *w > 0) else { return false };
    let input_tokens = message.usage.input + message.usage.cache_read;
    match message.stop_reason {
        StopReason::Stop => input_tokens > window,
        StopReason::Length => message.usage.output == 0 && input_tokens as f64 >= window as f64 * 0.99,
        _ => false,
    }
}

pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length && desired_max_output > 0 && message.usage.output < desired_max_output
}

pub fn get_overflow_patterns() -> Vec<Regex> {
    OVERFLOW_PATTERNS.clone()
}

/// The loosely typed message shape the Cursor helpers accept.
#[derive(Debug, Clone, Default)]
pub struct CursorExhaustionProbe<'a> {
    pub stop_reason: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub usage: Option<Usage>,
}

impl<'a> CursorExhaustionProbe<'a> {
    pub fn from_message(message: &'a AssistantMessage) -> Self {
        let stop_reason = match message.stop_reason {
            StopReason::Stop => "stop",
            StopReason::Length => "length",
            StopReason::ToolUse => "toolUse",
            StopReason::Error => "error",
            StopReason::Aborted => "aborted",
            StopReason::Pending => "pending",
            StopReason::Deferred => "deferred",
        };
        Self { stop_reason: Some(stop_reason), error_message: message.error_message.as_deref(), usage: Some(message.usage) }
    }

    fn tokens(&self) -> u64 {
        self.usage.as_ref().map_or(0, token_count)
    }

    fn is_resource_exhausted_error(&self) -> bool {
        self.stop_reason == Some("error") && RESOURCE_EXHAUSTED_PATTERN.is_match(self.error_message.unwrap_or_default())
    }
}

pub fn is_cursor_payload_resource_exhausted(message: &CursorExhaustionProbe<'_>, _estimate_tokens: u64) -> bool {
    message.is_resource_exhausted_error() && message.tokens() == 0
}

pub fn is_cursor_quota_resource_exhausted(message: &CursorExhaustionProbe<'_>, context_window: u64) -> bool {
    let tokens = message.tokens();
    message.is_resource_exhausted_error() && context_window > 0 && tokens > 0 && (tokens as f64) < context_window as f64 * 0.5
}

pub fn is_cursor_zero_token_resource_exhausted(message: &CursorExhaustionProbe<'_>) -> bool {
    message.is_resource_exhausted_error() && message.tokens() == 0
}

pub fn should_skip_provider_fallback_for_cursor_zero_re(same_model_remint: Option<bool>) -> bool {
    same_model_remint == Some(true)
}

static NOTHING_TO_COMPACT: LazyLock<Regex> = LazyLock::new(|| build("Nothing to compact", true));

pub fn should_retry_overflow_without_compact(compacted: bool, error_message: &str) -> bool {
    !compacted && NOTHING_TO_COMPACT.is_match(error_message)
}

/// Compaction settings fields the Cursor overflow override touches.
pub trait CursorCompactionSettings: Clone {
    fn with_cursor_overflow_overrides(self) -> Self;
}

pub fn cursor_overflow_compaction_settings<T: CursorCompactionSettings>(settings: T, provider: Option<&str>, reason: Option<&str>) -> T {
    if reason == Some("overflow") && matches!(provider, Some("cursor" | "cursor-cli-oauth")) {
        settings.with_cursor_overflow_overrides()
    } else {
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(stop: StopReason, error: Option<&str>, input: u64, output: u64, total: u64) -> AssistantMessage {
        let model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8").expect("model");
        let mut message = crate::utils::lazy::setup_error_message(model, error.unwrap_or_default());
        message.stop_reason = stop;
        message.error_message = error.map(str::to_owned);
        message.usage.input = input;
        message.usage.output = output;
        message.usage.total_tokens = total;
        message
    }

    #[test]
    fn detects_error_and_usage_overflow() {
        assert!(is_context_overflow(&message(StopReason::Error, Some("prompt is too long: 250000 tokens"), 0, 0, 0), None));
        assert!(!is_context_overflow(&message(StopReason::Error, Some("429 too many tokens"), 0, 0, 0), None));
        assert!(is_context_overflow(&message(StopReason::Error, Some("Request payload is 5 bytes, over the 4 byte limit Kiro accepts."), 0, 0, 0), None));
        assert!(!is_context_overflow(&message(StopReason::Error, Some("REQUEST PAYLOAD IS 5 BYTES, OVER THE 4 BYTE LIMIT KIRO ACCEPTS."), 0, 0, 0), None));
        assert!(is_context_overflow(&message(StopReason::Error, Some("ResourceExhausted"), 0, 0, 600), Some(1000)));
        assert!(!is_context_overflow(&message(StopReason::Error, Some("ResourceExhausted"), 0, 0, 100), Some(1000)));
        assert!(is_context_overflow(&message(StopReason::Stop, None, 1001, 1, 0), Some(1000)));
        assert!(is_context_overflow(&message(StopReason::Length, None, 990, 0, 0), Some(1000)));
        assert!(!is_context_overflow(&message(StopReason::Length, None, 990, 1, 0), Some(1000)));
        assert!(is_recoverable_length(&message(StopReason::Length, None, 0, 5, 0), 10));
        assert_eq!(get_overflow_patterns().len(), 29);
    }

    #[test]
    fn cursor_resource_exhaustion_helpers() {
        let zero = message(StopReason::Error, Some("resource_exhausted"), 0, 0, 0);
        let probe = CursorExhaustionProbe::from_message(&zero);
        assert!(is_cursor_zero_token_resource_exhausted(&probe) && is_cursor_payload_resource_exhausted(&probe, 5));
        let quota = message(StopReason::Error, Some("resource exhausted"), 0, 0, 100);
        assert!(is_cursor_quota_resource_exhausted(&CursorExhaustionProbe::from_message(&quota), 1000));
        assert!(!is_cursor_quota_resource_exhausted(&CursorExhaustionProbe::from_message(&quota), 100));
        assert!(should_skip_provider_fallback_for_cursor_zero_re(Some(true)) && !should_skip_provider_fallback_for_cursor_zero_re(None));
        assert!(should_retry_overflow_without_compact(false, "nothing to compact") && !should_retry_overflow_without_compact(true, "Nothing to compact"));
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Settings {
        keep_recent_tokens: Option<u64>,
        restoration_enabled: Option<bool>,
    }

    impl CursorCompactionSettings for Settings {
        fn with_cursor_overflow_overrides(self) -> Self {
            Settings { keep_recent_tokens: Some(0), restoration_enabled: Some(false) }
        }
    }

    #[test]
    fn cursor_overflow_compaction_override() {
        let base = Settings { keep_recent_tokens: Some(9), restoration_enabled: None };
        assert_eq!(cursor_overflow_compaction_settings(base.clone(), Some("cursor"), Some("overflow")).keep_recent_tokens, Some(0));
        assert_eq!(cursor_overflow_compaction_settings(base.clone(), Some("anthropic"), Some("overflow")), base);
        assert_eq!(cursor_overflow_compaction_settings(base.clone(), Some("cursor"), Some("manual")), base);
    }
}
