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
        // classifiers.test.ts:42 it.each "classifies whitelisted http-status %i as %s"
        let cases: &[(&str, u16, RetryClassification)] = &[
            ("classifies whitelisted http-status 408 as transient", 408, RetryClassification::Transient),
            ("classifies whitelisted http-status 409 as transient", 409, RetryClassification::Transient),
            ("classifies whitelisted http-status 429 as rate-limited", 429, RetryClassification::RateLimited),
            ("classifies whitelisted http-status 500 as transient", 500, RetryClassification::Transient),
            ("classifies whitelisted http-status 502 as transient", 502, RetryClassification::Transient),
            ("classifies whitelisted http-status 503 as transient", 503, RetryClassification::Transient),
            ("classifies whitelisted http-status 504 as transient", 504, RetryClassification::Transient),
            ("classifies whitelisted http-status 529 as transient", 529, RetryClassification::Transient),
        ];
        for (title, status_code, expected) in cases {
            let failure = failure(RetryFailureKind::HttpStatus, "kimi failure", Some(*status_code));
            assert_eq!(classify_kimi_failure(&failure), *expected, "case: {title}");
        }
    }

    #[test]
    fn classifies_non_whitelisted_kimi_http_status_as_terminal() {
        // classifiers.test.ts:53 it.each "classifies non-whitelisted http-status %i as terminal"
        let cases: &[(&str, u16)] = &[
            ("classifies non-whitelisted http-status 400 as terminal", 400),
            ("classifies non-whitelisted http-status 401 as terminal", 401),
            ("classifies non-whitelisted http-status 404 as terminal", 404),
            ("classifies non-whitelisted http-status 422 as terminal", 422),
            ("classifies non-whitelisted http-status 501 as terminal", 501),
        ];
        for (title, status_code) in cases {
            let failure = failure(RetryFailureKind::HttpStatus, "kimi failure", Some(*status_code));
            assert_eq!(classify_kimi_failure(&failure), RetryClassification::Terminal, "case: {title}");
        }
    }

    #[test]
    fn classifies_kimi_http_status_without_a_status_code_as_terminal() {
        // classifiers.test.ts:61 it "classifies an http-status failure without a status code as terminal"
        assert_eq!(classify_kimi_failure(&kimi_failure(RetryFailureKind::HttpStatus)), RetryClassification::Terminal);
    }

    #[test]
    fn classifies_kimi_failure_kinds() {
        // classifiers.test.ts:65 it.each "classifies %s failures as %s"
        let cases: &[(&str, RetryFailureKind, RetryClassification)] = &[
            ("classifies abort failures as terminal", RetryFailureKind::Abort, RetryClassification::Terminal),
            ("classifies refusal failures as terminal", RetryFailureKind::Refusal, RetryClassification::Terminal),
            ("classifies sensitive failures as terminal", RetryFailureKind::Sensitive, RetryClassification::Terminal),
            ("classifies connection failures as transient", RetryFailureKind::Connection, RetryClassification::Transient),
            ("classifies timeout failures as transient", RetryFailureKind::Timeout, RetryClassification::Transient),
            ("classifies quota-exhausted failures as terminal", RetryFailureKind::QuotaExhausted, RetryClassification::Terminal),
            ("classifies image-format failures as terminal", RetryFailureKind::ImageFormat, RetryClassification::Terminal),
            ("classifies provider failures as transient", RetryFailureKind::Provider, RetryClassification::Transient),
            ("classifies unknown failures as terminal", RetryFailureKind::Unknown, RetryClassification::Terminal),
        ];
        for (title, kind, expected) in cases {
            assert_eq!(classify_kimi_failure(&kimi_failure(*kind)), *expected, "case: {title}");
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
        // classifiers.test.ts:100 it.each "mirrors isRetryableErrorMessage: %s"
        let cases: &[(&str, &str, bool)] = &[
            ("mirrors isRetryableErrorMessage: apitopia tool-schema rejection in a 500 envelope", APITOPIA_TOOL_SCHEMA_REJECTION, false),
            ("mirrors isRetryableErrorMessage: moonshot tool-schema rejection in a 500 envelope", MOONSHOT_TOOL_SCHEMA_REJECTION, false),
            ("mirrors isRetryableErrorMessage: anthropic invalid max_tokens 400", ANTHROPIC_INVALID_MAX_TOKENS, false),
            ("mirrors isRetryableErrorMessage: 429 quota exceeded", "429 quota exceeded", false),
            ("mirrors isRetryableErrorMessage: anthropic credits_required 429", ANTHROPIC_CREDITS_REQUIRED, false),
            ("mirrors isRetryableErrorMessage: canonical gateway model-request rejection", GATEWAY_MODEL_REQUEST_REJECTED, true),
            ("mirrors isRetryableErrorMessage: openai explicit retry guidance", OPENAI_EXPLICIT_RETRY, true),
            ("mirrors isRetryableErrorMessage: openai server_error with retry guidance", OPENAI_SERVER_ERROR, true),
            ("mirrors isRetryableErrorMessage: bedrock explicit retry guidance", BEDROCK_EXPLICIT_RETRY, true),
            ("mirrors isRetryableErrorMessage: nvidia nim ResourceExhausted", NVIDIA_NIM_RESOURCE_EXHAUSTED, true),
            ("mirrors isRetryableErrorMessage: bun fetch socket drop", BUN_FETCH_SOCKET_CLOSED, true),
            ("mirrors isRetryableErrorMessage: openai responses early EOF", OPENAI_RESPONSES_EARLY_EOF, true),
            ("mirrors isRetryableErrorMessage: wrapped DNS lookup failure", WRAPPED_DNS_LOOKUP_ERROR, true),
            ("mirrors isRetryableErrorMessage: anthropic orphan server-tool 400", ANTHROPIC_ORPHAN_SERVER_TOOL, true),
        ];
        for (title, message, retryable) in cases {
            let failure = failure(RetryFailureKind::Unknown, message, None);
            // Pin the fixture's expected boolean first so regex drift surfaces here too.
            assert_eq!(is_retryable_error_message(message), *retryable, "case: {title}");
            let expected = if *retryable { RetryClassification::Transient } else { RetryClassification::Terminal };
            assert_eq!(classify_senpi_assistant_failure(&failure), expected, "case: {title}");
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
        // classifiers.test.ts:207 it.each "retries an opaque %i on its structured status alone"
        let cases: &[(&str, u16)] = &[
            ("retries an opaque 408 on its structured status alone", 408),
            ("retries an opaque 409 on its structured status alone", 409),
            ("retries an opaque 500 on its structured status alone", 500),
            ("retries an opaque 502 on its structured status alone", 502),
            ("retries an opaque 503 on its structured status alone", 503),
            ("retries an opaque 504 on its structured status alone", 504),
            ("retries an opaque 522 on its structured status alone", 522),
            ("retries an opaque 524 on its structured status alone", 524),
        ];
        for (title, status_code) in cases {
            let failure = opaque_failure(Some(*status_code));
            assert!(!is_retryable_error_message(&failure.message), "case: {title}");
            assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Transient, "case: {title}");
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
        // classifiers.test.ts:274 it.each "treats a 429 with the %s provider code as terminal"
        let cases: &[(&str, &str)] = &[
            ("treats a 429 with the usage_limit_reached provider code as terminal", "usage_limit_reached"),
            ("treats a 429 with the usage_not_included provider code as terminal", "usage_not_included"),
        ];
        for (title, code) in cases {
            let failure = RetryFailure {
                provider_codes: Some(vec![(*code).into()]),
                ..failure(RetryFailureKind::HttpStatus, "The provider could not complete this request right now", Some(429))
            };
            assert_eq!(classify_senpi_assistant_failure(&failure), RetryClassification::Terminal, "case: {title}");
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
