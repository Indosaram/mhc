//! Port of senpi packages/ai/src/utils/retry-profile/classifiers.ts.

use super::types::{RetryClassification, RetryFailure, RetryFailureKind};
use crate::utils::retry::{ErrorMessageClass, USAGE_LIMIT_EXHAUSTION, classify_error_message};

const KIMI_RETRYABLE_STATUS_CODES: &[u16] = &[408, 409, 429, 500, 502, 503, 504, 529];

fn classify_kimi_http_status(status_code: Option<u16>) -> RetryClassification {
    match status_code {
        Some(429) => RetryClassification::RateLimited,
        Some(code) if KIMI_RETRYABLE_STATUS_CODES.contains(&code) => RetryClassification::Transient,
        _ => RetryClassification::Terminal,
    }
}

pub fn classify_kimi_failure(failure: &RetryFailure) -> RetryClassification {
    use RetryFailureKind as K;
    match failure.kind {
        K::Abort | K::Refusal | K::Sensitive | K::QuotaExhausted | K::ImageFormat | K::Unknown => RetryClassification::Terminal,
        K::Connection | K::Timeout | K::Provider => RetryClassification::Transient,
        K::EmptyResponse if failure.finish_reason.as_deref() == Some("filtered") => RetryClassification::Terminal,
        K::EmptyResponse => RetryClassification::Transient,
        K::HttpStatus => classify_kimi_http_status(failure.status_code),
    }
}

const SENPI_STRUCTURED_RETRYABLE_STATUS_CODES: &[u16] = &[408, 409, 429, 500, 502, 503, 504, 522, 524, 529];

fn is_senpi_terminal_provider_code(code: &str) -> bool {
    matches!(code, "insufficient_quota" | "credits_required") || USAGE_LIMIT_EXHAUSTION.codes.contains(&code)
}

pub fn classify_senpi_assistant_failure(failure: &RetryFailure) -> RetryClassification {
    use RetryFailureKind as K;
    if matches!(failure.kind, K::Abort | K::Refusal | K::Sensitive | K::QuotaExhausted | K::ImageFormat) {
        return RetryClassification::Terminal;
    }
    let regex_verdict = classify_error_message(&failure.message);
    if regex_verdict == ErrorMessageClass::NonRetryable
        || failure.should_retry == Some(false)
        || failure.provider_codes.iter().flatten().any(|code| is_senpi_terminal_provider_code(code))
    {
        return RetryClassification::Terminal;
    }
    if regex_verdict == ErrorMessageClass::Retryable {
        return RetryClassification::Transient;
    }
    match failure.status_code {
        Some(429) => RetryClassification::RateLimited,
        Some(code) if SENPI_STRUCTURED_RETRYABLE_STATUS_CODES.contains(&code) => RetryClassification::Transient,
        _ => RetryClassification::Terminal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::retry::is_retryable_error_message;

    // Fixture strings mirrored verbatim from senpi's retry.test.ts /
    // retry-profile-classifiers.test.ts. Do not reword: the delegation pin
    // intentionally matches the exact bytes the regex classifier sees.
    const APITOPIA_TOOL_SCHEMA_REJECTION: &str = "500 data: {\"error\":{\"message\":\"500 server_error: Invalid request: tools.function.parameters.type is required and must be \\\"object\\\"\",\"type\":\"server_error\",\"code\":500,\"status\":500,\"statusCode\":500,\"isRetryable\":true}}\n\ndata:[DONE]\n\n";
    const MOONSHOT_TOOL_SCHEMA_REJECTION: &str = "500 server_error: Invalid request: tools.0.function.parameters: invalid tool schema";
    const ANTHROPIC_INVALID_MAX_TOKENS: &str = "400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"max_tokens: must be greater than or equal to 1\"}}";
    const ANTHROPIC_CREDITS_REQUIRED: &str = "429 event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Usage credits are required for this model.\",\"details\":{\"error_code\":\"credits_required\",\"model\":\"claude-fable-5\"}},\"request_id\":\"req_011CdW2nFxprAx6KQ9JhnAvq\"}";
    const GATEWAY_MODEL_REQUEST_REJECTED: &str = "Error: The model request was rejected. Check the request and try again.";
    const OPENAI_EXPLICIT_RETRY: &str = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.";
    const OPENAI_SERVER_ERROR: &str = "Error: Error Code server_error: An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID e4026cfc-c6b6-414a-8a21-c03a6adf0336 in your message.";
    const BEDROCK_EXPLICIT_RETRY: &str = "{\"message\":\"The system encountered an unexpected error during processing. Try your request again.\"}";
    const NVIDIA_NIM_RESOURCE_EXHAUSTED: &str = "ResourceExhausted: Worker local total request limit reached (288/48)";
    const BUN_FETCH_SOCKET_CLOSED: &str = "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()";
    const OPENAI_RESPONSES_EARLY_EOF: &str = "OpenAI Responses stream ended before a terminal response event";
    const WRAPPED_DNS_LOOKUP_ERROR: &str = "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)";
    const ANTHROPIC_ORPHAN_SERVER_TOOL: &str = "400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"messages.1: `web_search` tool use with id `srvtoolu_01Gchdhqw1UaCNUuVq2LhMH9` was found without a corresponding `web_search_tool_result` block\"},\"request_id\":\"req_011CdQL9JsEk5NWJxWQX4NiG\"}";

    pub(crate) fn failure(kind: RetryFailureKind, message: &str, status_code: Option<u16>) -> RetryFailure {
        RetryFailure {
            origin: "test".into(),
            kind,
            message: message.into(),
            status_code,
            provider_codes: None,
            finish_reason: None,
            retry_after_ms: None,
            should_retry: None,
        }
    }

    fn kimi_failure(kind: RetryFailureKind) -> RetryFailure {
        failure(kind, "kimi failure", None)
    }

    #[test]
    fn classifies_whitelisted_kimi_http_status_codes() {
        let cases: &[(u16, RetryClassification)] = &[
            (408, RetryClassification::Transient),
            (409, RetryClassification::Transient),
            (429, RetryClassification::RateLimited),
            (500, RetryClassification::Transient),
            (502, RetryClassification::Transient),
            (503, RetryClassification::Transient),
            (504, RetryClassification::Transient),
            (529, RetryClassification::Transient),
        ];
        for (status_code, expected) in cases {
            let failure = failure(RetryFailureKind::HttpStatus, "kimi failure", Some(*status_code));
            assert_eq!(classify_kimi_failure(&failure), *expected, "status {status_code}");
        }
    }

    #[test]
    fn classifies_non_whitelisted_kimi_http_status_as_terminal() {
        for status_code in [400u16, 401, 404, 422, 501] {
            let failure = failure(RetryFailureKind::HttpStatus, "kimi failure", Some(status_code));
            assert_eq!(classify_kimi_failure(&failure), RetryClassification::Terminal, "status {status_code}");
        }
    }

    #[test]
    fn classifies_kimi_http_status_without_a_status_code_as_terminal() {
        assert_eq!(classify_kimi_failure(&kimi_failure(RetryFailureKind::HttpStatus)), RetryClassification::Terminal);
    }

    #[test]
    fn classifies_kimi_failure_kinds() {
        let cases: &[(RetryFailureKind, RetryClassification)] = &[
            (RetryFailureKind::Abort, RetryClassification::Terminal),
            (RetryFailureKind::Refusal, RetryClassification::Terminal),
            (RetryFailureKind::Sensitive, RetryClassification::Terminal),
            (RetryFailureKind::Connection, RetryClassification::Transient),
            (RetryFailureKind::Timeout, RetryClassification::Transient),
            (RetryFailureKind::QuotaExhausted, RetryClassification::Terminal),
            (RetryFailureKind::ImageFormat, RetryClassification::Terminal),
            (RetryFailureKind::Provider, RetryClassification::Transient),
            (RetryFailureKind::Unknown, RetryClassification::Terminal),
        ];
        for (kind, expected) in cases {
            assert_eq!(classify_kimi_failure(&kimi_failure(*kind)), *expected, "{kind:?}");
        }
    }

    #[test]
    fn classifies_kimi_empty_response_as_transient_unless_finish_reason_is_filtered() {
        assert_eq!(classify_kimi_failure(&kimi_failure(RetryFailureKind::EmptyResponse)), RetryClassification::Transient);
        let stop = RetryFailure { finish_reason: Some("stop".into()), ..kimi_failure(RetryFailureKind::EmptyResponse) };
        assert_eq!(classify_kimi_failure(&stop), RetryClassification::Transient);
        let filtered = RetryFailure { finish_reason: Some("filtered".into()), ..kimi_failure(RetryFailureKind::EmptyResponse) };
        assert_eq!(classify_kimi_failure(&filtered), RetryClassification::Terminal);
    }

    #[test]
    fn classifies_kimi_quota_exhausted_429_as_terminal_kind_outranks_status() {
        let quota_429 = RetryFailure { status_code: Some(429), ..kimi_failure(RetryFailureKind::QuotaExhausted) };
        assert_eq!(classify_kimi_failure(&quota_429), RetryClassification::Terminal);
    }

    #[test]
    fn mirrors_is_retryable_error_message_for_senpi_assistant_failures() {
        let cases: &[(&str, bool)] = &[
            (APITOPIA_TOOL_SCHEMA_REJECTION, false),
            (MOONSHOT_TOOL_SCHEMA_REJECTION, false),
            (ANTHROPIC_INVALID_MAX_TOKENS, false),
            ("429 quota exceeded", false),
            (ANTHROPIC_CREDITS_REQUIRED, false),
            (GATEWAY_MODEL_REQUEST_REJECTED, true),
            (OPENAI_EXPLICIT_RETRY, true),
            (OPENAI_SERVER_ERROR, true),
            (BEDROCK_EXPLICIT_RETRY, true),
            (NVIDIA_NIM_RESOURCE_EXHAUSTED, true),
            (BUN_FETCH_SOCKET_CLOSED, true),
            (OPENAI_RESPONSES_EARLY_EOF, true),
            (WRAPPED_DNS_LOOKUP_ERROR, true),
            (ANTHROPIC_ORPHAN_SERVER_TOOL, true),
        ];
        for (message, retryable) in cases {
            let failure = failure(RetryFailureKind::Unknown, message, None);
            // Pin the fixture's expected boolean first so regex drift surfaces here too.
            assert_eq!(is_retryable_error_message(message), *retryable, "isRetryableErrorMessage({message:?})");
            let expected = if *retryable { RetryClassification::Transient } else { RetryClassification::Terminal };
            assert_eq!(classify_senpi_assistant_failure(&failure), expected, "{message:?}");
        }
    }

    #[test]
    fn keeps_non_retryable_precedence_over_retryable_wording() {
        for message in [
            format!("{GATEWAY_MODEL_REQUEST_REJECTED} quota exceeded"),
            format!("{GATEWAY_MODEL_REQUEST_REJECTED} Invalid request: tools.0.function.parameters.type is required"),
        ] {
            let failure = failure(RetryFailureKind::Unknown, &message, None);
            assert!(!is_retryable_error_message(&message));
            assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
        }
    }

    #[test]
    fn pins_the_accepted_divergence_between_the_two_classifiers_on_a_tool_schema_500() {
        let failure = failure(RetryFailureKind::HttpStatus, MOONSHOT_TOOL_SCHEMA_REJECTION, Some(500));
        assert_eq!(classify_kimi_failure(&failure), RetryClassification::Transient);
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    fn opaque_failure(status_code: Option<u16>) -> RetryFailure {
        failure(RetryFailureKind::HttpStatus, "The provider could not complete this request right now", status_code)
    }

    #[test]
    fn retries_an_opaque_status_on_its_structured_status_alone() {
        for status_code in [408u16, 409, 500, 502, 503, 504, 522, 524] {
            let failure = opaque_failure(Some(status_code));
            assert!(!is_retryable_error_message(&failure.message));
            assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Transient, "status {status_code}");
        }
    }

    #[test]
    fn classifies_an_opaque_429_as_rate_limited_on_its_structured_status_alone() {
        let failure = opaque_failure(Some(429));
        assert!(!is_retryable_error_message(&failure.message));
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::RateLimited);
    }

    #[test]
    fn keeps_529_retryable_for_an_opaque_message() {
        assert_eq!(classify_senpi_assistant_failure(&opaque_failure(Some(529))), RetryClassification::Transient);
    }

    #[test]
    fn treats_a_429_with_insufficient_quota_provider_code_as_terminal() {
        let failure = RetryFailure { provider_codes: Some(vec!["insufficient_quota".into()]), ..opaque_failure(Some(429)) };
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn treats_a_429_with_credits_required_provider_code_as_terminal() {
        let failure = RetryFailure { provider_codes: Some(vec!["credits_required".into()]), ..opaque_failure(Some(429)) };
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn honours_a_structured_should_retry_false_over_a_whitelisted_status() {
        let failure = RetryFailure { should_retry: Some(false), ..opaque_failure(Some(503)) };
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn keeps_a_500_carrying_tool_schema_text_terminal() {
        let failure = failure(RetryFailureKind::HttpStatus, MOONSHOT_TOOL_SCHEMA_REJECTION, Some(500));
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn keeps_a_400_carrying_server_tool_pairing_text_retryable() {
        let failure = failure(RetryFailureKind::HttpStatus, ANTHROPIC_ORPHAN_SERVER_TOOL, Some(400));
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Transient);
    }

    #[test]
    fn falls_back_to_the_exact_regex_verdict_when_no_diagnostic_facts_exist() {
        let failure = failure(RetryFailureKind::Unknown, "The provider could not complete this request right now", None);
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn keeps_deterministic_failure_kinds_terminal_before_any_text_or_status_inspection() {
        for kind in [
            RetryFailureKind::Abort,
            RetryFailureKind::Refusal,
            RetryFailureKind::Sensitive,
            RetryFailureKind::QuotaExhausted,
            RetryFailureKind::ImageFormat,
        ] {
            let failure = failure(kind, "overloaded", Some(503));
            assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal, "{kind:?}");
        }
    }

    const USAGE_LIMIT_EXHAUSTED_MESSAGE: &str =
        r#"OpenAI API error (429): {"type":"usage_limit_reached","message":"The usage limit has been reached"}"#;

    #[test]
    fn classifies_the_exhaustion_body_as_non_retryable() {
        assert!(!is_retryable_error_message(USAGE_LIMIT_EXHAUSTED_MESSAGE));
    }

    #[test]
    fn treats_a_message_only_exhaustion_failure_as_terminal() {
        let failure = failure(RetryFailureKind::Unknown, USAGE_LIMIT_EXHAUSTED_MESSAGE, None);
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn treats_an_exhaustion_failure_carrying_status_code_429_as_terminal_not_rate_limited() {
        let failure = failure(RetryFailureKind::HttpStatus, USAGE_LIMIT_EXHAUSTED_MESSAGE, Some(429));
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal);
    }

    #[test]
    fn treats_a_429_with_usage_limit_provider_codes_as_terminal() {
        for code in ["usage_limit_reached", "usage_not_included"] {
            let failure = RetryFailure {
                provider_codes: Some(vec![code.into()]),
                ..failure(RetryFailureKind::HttpStatus, "The provider could not complete this request right now", Some(429))
            };
            assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal, "{code}");
        }
    }

    #[test]
    fn keeps_an_approaching_the_limit_warning_retryable() {
        // "Approaching" the limit is a transient throttle warning, not
        // exhaustion: the family patterns must not swallow it.
        let message = "OpenAI API error (429): You are approaching your usage limit";
        assert!(is_retryable_error_message(message));
        let failure = failure(RetryFailureKind::HttpStatus, message, Some(429));
        assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Transient);
    }
}
