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

    /// Port of `createErrorMessage` (overflow.test.ts): an `error`-stopped faux Ollama message
    /// with zero usage and the given error text.
    fn error_message(error_message: &str) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "openai-completions".into(),
            provider: "ollama".into(),
            model: "qwen3.5:35b".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Error,
            stop_details: None,
            deferred: None,
            error_message: Some(error_message.to_owned()),
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    struct LengthStopOptions {
        input: u64,
        cache_read: u64,
        output: u64,
        cache_write: u64,
    }

    /// Port of `createLengthStopMessage` (overflow.test.ts).
    fn length_stop_message(options: LengthStopOptions) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "openai-completions".into(),
            provider: "test-provider".into(),
            model: "test-model".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage {
                input: options.input,
                output: options.output,
                cache_read: options.cache_read,
                cache_write: options.cache_write,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: options.input + options.cache_read + options.cache_write + options.output,
                cost: Default::default(),
            },
            stop_reason: StopReason::Length,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    /// One row per `it()` in overflow.test.ts's `describe("isContextOverflow")`, title carried
    /// verbatim. A TS case that makes several assertions contributes one row per assertion, each
    /// row carrying that case's verbatim title; the failing row is identified by the input text
    /// in the assertion message.
    #[test]
    fn is_context_overflow_matches_senpi_cases() {
        let cases: &[(&str, AssistantMessage, Option<u64>, bool)] = &[
            (
                "detects the local context exhaustion guard before any provider call",
                error_message("Context window exhausted: the conversation is estimated at 995154 of 1000000 tokens, leaving fewer than 1024 tokens for a response. Compact the conversation, enable auto-compaction, or start a new session before retrying."),
                Some(1_000_000),
                true,
            ),
            (
                "detects explicit Ollama prompt-too-long errors",
                error_message("400 `prompt too long; exceeded max context length by 100918 tokens`"),
                Some(32768),
                true,
            ),
            (
                "detects Together AI context length errors",
                error_message("400 The input (516368 tokens) is longer than the model's context length (262144 tokens)."),
                Some(262144),
                true,
            ),
            (
                "detects LiteLLM-wrapped OpenAI maximum context length errors",
                error_message("Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens."),
                Some(131072),
                true,
            ),
            (
                "detects OpenAI-compatible parenthesized maximum context length errors",
                error_message("Error: 400 Input length (265330) exceeds model's maximum context length (262144)."),
                Some(262144),
                true,
            ),
            (
                "detects OpenAI exceeds the model's context window wording",
                error_message("Your input exceeds the model's context window"),
                None,
                true,
            ),
            (
                "detects OpenAI exceeds this model's context window wording",
                error_message("Your input exceeds this model's context window"),
                None,
                true,
            ),
            (
                "detects OpenAI exceeds the context window wording",
                error_message("Your input exceeds the context window of this model"),
                None,
                true,
            ),
            (
                "detects OpenRouter Poolside maximum allowed input length errors",
                error_message("Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens."),
                Some(131072),
                true,
            ),
            (
                "detects DS4 configured context size errors",
                error_message("400 Prompt has 256468 tokens, but the configured context size is 256000 tokens"),
                Some(256000),
                true,
            ),
            (
                "detects DS4 configured context size errors",
                error_message("Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens"),
                Some(256000),
                true,
            ),
            (
                "detects gateway 413 body-size rejections as byte-size overflow (OpenAI-style body_too_large)",
                error_message(r#"413: {"message":"Request body too large","type":"invalid_request_error","code":"body_too_large"}"#),
                Some(200000),
                true,
            ),
            (
                "detects gateway 413 body-size rejections as byte-size overflow",
                error_message(r#"413: {"message":"Request Entity Too Large","type":"AI_APICallError","param":{"error":"Request Entity Too Large","statusCode":413,"name":"AI_APICallError","message":"Request Entity Too Large","isRetryable":false,"type":"AI_APICallError"}}"#),
                Some(200000),
                true,
            ),
            (
                "detects gateway 413 body-size rejections as byte-size overflow",
                error_message("413 Payload Too Large"),
                Some(200000),
                true,
            ),
            (
                "detects kiro-lb local payload guards across both route wrappers and units",
                error_message(r#"400 {"type":"error","error":{"type":"invalid_request_error","message":"Request payload is 1095225 bytes, over the 1085435 byte limit Kiro accepts. Shorten the conversation or send fewer tools."}}"#),
                Some(666667),
                true,
            ),
            (
                "detects kiro-lb local payload guards across both route wrappers and units (OpenAI tokens)",
                error_message(r#"400 {"detail":"Request payload is 800001 tokens, over the 800000 token limit Kiro accepts."}"#),
                Some(666667),
                true,
            ),
            (
                "detects Kiro upstream context overflow enhanced by kiro-lb",
                error_message(r#"400 {"error":{"type":"kiro_api_error","message":"Model context limit reached. Conversation size exceeds model capacity."}}"#),
                Some(666667),
                true,
            ),
            (
                "rejects malformed or unrelated Kiro-like payload-size prose",
                error_message("Request payload is 1,,225 bytes, over the 1,085 byte limit Kiro accepts."),
                None,
                false,
            ),
            (
                "rejects malformed or unrelated Kiro-like payload-size prose",
                error_message("Request payload is 1,225 bytes, over the 1,085 byte limit Kiro accepts."),
                None,
                false,
            ),
            (
                "rejects malformed or unrelated Kiro-like payload-size prose",
                error_message("Payload 1095225 bytes exceeds the 1085435 byte limit; AUTO_TRIM_PAYLOAD is disabled"),
                None,
                false,
            ),
            (
                "rejects malformed or unrelated Kiro-like payload-size prose",
                error_message("Another provider reports payload size exceeded."),
                None,
                false,
            ),
            (
                "does not treat generic non-overflow Ollama errors as overflow",
                error_message("500 `model runner crashed unexpectedly`"),
                Some(32768),
                false,
            ),
            (
                "does not treat Bedrock throttling 'Too many tokens' as overflow",
                error_message("Throttling error: Too many tokens, please wait before trying again."),
                Some(200000),
                false,
            ),
            (
                "does not treat Bedrock service unavailable as overflow",
                error_message("Service unavailable: The service is temporarily unavailable."),
                Some(200000),
                false,
            ),
            (
                "does not treat generic rate limit errors as overflow",
                error_message("Rate limit exceeded, please retry after 30 seconds."),
                Some(200000),
                false,
            ),
            (
                "does not treat HTTP 429 style errors as overflow",
                error_message("Too many requests. Please slow down."),
                Some(200000),
                false,
            ),
            (
                "does not treat tokens-per-minute rate limits as overflow",
                error_message("Too many tokens per minute for this model. Retry in 20s"),
                Some(200000),
                false,
            ),
            (
                "does not treat exceeds-the-limit tokens-per-minute messages as overflow",
                error_message("This request exceeds the limit of 30000 tokens per minute"),
                Some(200000),
                false,
            ),
            (
                "does not treat TPM quota wording as overflow even with overflow-looking phrasing",
                error_message("TPM limit exceeded: too many tokens"),
                Some(200000),
                false,
            ),
            (
                "does not treat RPM quota wording as overflow even with overflow-looking phrasing",
                error_message("RPM limit exceeded: too many tokens"),
                Some(200000),
                false,
            ),
            (
                "does not treat quota exceeded as overflow even with overflow-looking phrasing",
                error_message("Quota exceeded: too many tokens in the last minute"),
                Some(200000),
                false,
            ),
            (
                "does not treat retry-after token quota wording as overflow even with overflow-looking phrasing",
                error_message("Too many tokens. Retry after 10 seconds"),
                Some(200000),
                false,
            ),
            (
                "does not treat HTTP 429 prefixes as overflow even with overflow-looking phrasing",
                error_message("429 Too many tokens"),
                Some(200000),
                false,
            ),
            (
                "does not treat status code 429 as overflow even with overflow-looking phrasing",
                error_message("Request failed with status code 429: too many tokens"),
                Some(200000),
                false,
            ),
            (
                "does not treat overloaded errors as overflow even with overflow-looking phrasing",
                error_message("The model is overloaded. Too many tokens."),
                Some(200000),
                false,
            ),
        ];
        for (title, message, context_window, expected) in cases {
            assert_eq!(
                is_context_overflow(message, *context_window),
                *expected,
                "case: {title} :: {}",
                message.error_message.as_deref().unwrap_or_default()
            );
        }
    }

    #[test]
    fn does_not_treat_tiny_token_bearing_resource_exhausted_usage_as_context_overflow() {
        let mut message = error_message("Connect error resource_exhausted");
        message.usage.output = 12;
        message.usage.total_tokens = 12;
        assert!(!is_context_overflow(&message, Some(200_000)));
    }

    #[test]
    fn treats_token_bearing_resource_exhausted_usage_near_the_context_window_as_overflow() {
        let mut message = error_message("gRPC error 8: resource_exhausted");
        message.usage.total_tokens = 600_000;
        assert!(is_context_overflow(&message, Some(1_048_576)));
    }

    #[test]
    fn preserves_legacy_token_bearing_resource_exhausted_overflow_detection_without_a_context_window() {
        let mut message = error_message("Connect error resource_exhausted");
        message.usage.total_tokens = 12;
        assert!(is_context_overflow(&message, None));
    }

    #[test]
    fn identifies_cursor_usage_pool_exhaustion_below_half_the_context_window() {
        let mut message = error_message("Connect error resource_exhausted: Error");
        message.usage.total_tokens = 178_626;
        assert!(!is_context_overflow(&message, Some(1_048_576)));
        assert!(is_cursor_quota_resource_exhausted(&CursorExhaustionProbe::from_message(&message), 1_048_576));
    }

    #[test]
    fn does_not_identify_cursor_context_overflow_as_usage_pool_exhaustion() {
        let mut message = error_message("Connect error resource_exhausted: Error");
        message.usage.total_tokens = 600_000;
        assert!(!is_cursor_quota_resource_exhausted(&CursorExhaustionProbe::from_message(&message), 1_048_576));
    }

    #[test]
    fn does_not_identify_zero_token_or_non_resource_exhausted_errors_as_usage_pool_exhaustion() {
        let zero_token = error_message("Connect error resource_exhausted: Error");
        let non_resource_exhausted = error_message("Connect error unavailable");
        assert!(!is_cursor_quota_resource_exhausted(&CursorExhaustionProbe::from_message(&zero_token), 1_048_576));
        assert!(!is_cursor_quota_resource_exhausted(&CursorExhaustionProbe::from_message(&non_resource_exhausted), 1_048_576));
    }

    #[test]
    fn keeps_zero_token_resource_exhausted_errors_out_of_overflow_detection() {
        let message = error_message("Connect error resource_exhausted: quota exceeded");
        assert!(!is_context_overflow(&message, Some(200_000)));
    }

    #[test]
    fn detects_xiaomi_style_overflow_length_stop_with_zero_output_and_filled_context() {
        let message = length_stop_message(LengthStopOptions { input: 58, cache_read: 1_048_512, output: 0, cache_write: 0 });
        assert!(is_context_overflow(&message, Some(1_048_576)));
    }

    #[test]
    fn treats_a_length_stop_below_the_desired_output_limit_as_recoverable() {
        let message = length_stop_message(LengthStopOptions { input: 3, cache_read: 253_584, cache_write: 25_554, output: 16 });
        assert!(is_recoverable_length(&message, 128_000));
    }

    #[test]
    fn does_not_recover_a_length_stop_that_reached_the_desired_output_limit() {
        let message = length_stop_message(LengthStopOptions { input: 4062, cache_read: 0, cache_write: 0, output: 1024 });
        assert!(!is_recoverable_length(&message, 1024));
    }

    #[test]
    fn treats_zero_output_length_stops_as_recoverable_without_context_metadata() {
        let message = length_stop_message(LengthStopOptions { input: 100, cache_read: 0, cache_write: 0, output: 0 });
        assert!(is_recoverable_length(&message, 128_000));
    }

    #[test]
    fn does_not_treat_normal_length_stops_with_output_as_context_overflow() {
        let message = length_stop_message(LengthStopOptions { input: 1000, cache_read: 0, cache_write: 0, output: 4096 });
        assert!(!is_context_overflow(&message, Some(200_000)));
    }

    #[test]
    fn does_not_treat_zero_output_length_stops_far_below_context_as_context_overflow() {
        let message = length_stop_message(LengthStopOptions { input: 100, cache_read: 0, cache_write: 0, output: 0 });
        assert!(!is_context_overflow(&message, Some(200_000)));
    }

    #[test]
    fn get_overflow_patterns_count() {
        assert_eq!(get_overflow_patterns().len(), 29);
    }

    #[test]
    fn cursor_resource_exhaustion_helpers() {
        let zero = error_message("resource_exhausted");
        let probe = CursorExhaustionProbe::from_message(&zero);
        assert!(is_cursor_zero_token_resource_exhausted(&probe) && is_cursor_payload_resource_exhausted(&probe, 5));
        let quota = error_message("resource exhausted");
        let mut quota = quota;
        quota.usage.total_tokens = 100;
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
