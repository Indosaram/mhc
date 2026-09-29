use std::cell::RefCell;

use model_core::RuntimeFallbackAutoRetrySignal;
use model_core::RuntimeFallbackErrorType;
use model_core::RuntimeFallbackRetryOptions;
use model_core::UnsafeRetryableSignalRejected;
use model_core::classify_runtime_fallback_error;
use model_core::extract_runtime_fallback_auto_retry_signal;
use model_core::get_runtime_fallback_error_message;
use model_core::get_runtime_fallback_status_code;
use model_core::is_runtime_fallback_retryable_error;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const DEFAULT_RETRY_CODES: [i64; 5] = [429, 500, 502, 503, 504];

fn retryable(error: Option<&Value>, codes: &[i64]) -> bool {
    is_runtime_fallback_retryable_error(error, codes, &RuntimeFallbackRetryOptions::default())
}

#[test]
fn classifies_representative_anthropic_provider_payloads_without_adapter_state() {
    let cases = [
        (
            "anthropic 429 rate limit",
            json!({
                "name": "AI_APICallError",
                "statusCode": 429,
                "message": "Too Many Requests: rate limit reached for anthropic/claude-sonnet-4-6",
            }),
            None,
            true,
            Some(429),
        ),
        (
            "anthropic 503 service unavailable",
            json!({ "error": { "name": "AI_APICallError", "statusCode": 503, "message": "Service Unavailable" } }),
            None,
            true,
            Some(503),
        ),
        (
            "anthropic quota exhaustion",
            json!({
                "data": {
                    "error": {
                        "name": "QuotaExceededError",
                        "message": "Subscription quota exceeded. You can continue using free models.",
                    },
                },
            }),
            Some(RuntimeFallbackErrorType::QuotaExceeded),
            true,
            None,
        ),
        (
            "anthropic abort",
            json!({ "name": "MessageAbortedError", "message": "The user aborted this request." }),
            Some(RuntimeFallbackErrorType::Abort),
            false,
            None,
        ),
        (
            "anthropic unrelated validation error",
            json!({ "name": "ValidationError", "statusCode": 400, "message": "Invalid request payload" }),
            None,
            false,
            Some(400),
        ),
    ];

    for (label, error, expected_type, expected_retryable, expected_status_code) in cases {
        assert_eq!(
            classify_runtime_fallback_error(Some(&error)),
            expected_type,
            "{label}"
        );
        assert_eq!(
            retryable(Some(&error), &DEFAULT_RETRY_CODES),
            expected_retryable,
            "{label}"
        );
        assert_eq!(
            get_runtime_fallback_status_code(Some(&error), Some(&DEFAULT_RETRY_CODES)),
            expected_status_code,
            "{label}"
        );
    }
}

#[test]
fn preserves_malformed_provider_payload_classification_behavior() {
    // `null` and `undefined` both map to an absent/null value in Rust.
    let malformed_payloads = [
        Some(Value::Null),
        None,
        Some(json!({ "statusCode": "429", "message": 429 })),
        Some(json!({ "data": { "error": { "name": 7, "message": false } } })),
        Some(json!({ "data": { "error": null }, "error": "broken" })),
    ];

    let results: Vec<_> = malformed_payloads
        .iter()
        .map(|error| {
            let error = error.as_ref();
            (
                get_runtime_fallback_error_message(error),
                get_runtime_fallback_status_code(error, Some(&DEFAULT_RETRY_CODES)),
                classify_runtime_fallback_error(error),
                retryable(error, &DEFAULT_RETRY_CODES),
            )
        })
        .collect();

    assert_eq!(
        results,
        vec![
            (String::new(), None, None, false),
            (String::new(), None, None, false),
            (
                r#"{"statuscode":"429","message":429}"#.to_string(),
                Some(429),
                None,
                true
            ),
            (
                r#"{"data":{"error":{"name":7,"message":false}}}"#.to_string(),
                None,
                None,
                false
            ),
            (
                r#"{"data":{"error":null},"error":"broken"}"#.to_string(),
                None,
                None,
                false
            ),
        ]
    );
}

#[test]
fn honors_retryable_ai_sdk_signals_only_for_safe_status_codes() {
    let cases = [
        (
            json!({ "error": { "statusCode": 524, "isRetryable": true, "message": "Cloudflare timeout" } }),
            true,
        ),
        (
            json!({ "error": { "statusCode": 401, "isRetryable": true, "message": "Unauthorized" } }),
            false,
        ),
        (
            json!({ "error": { "isRetryable": true, "message": "connection reset before response body arrived" } }),
            true,
        ),
    ];

    let rejected: RefCell<Vec<i64>> = RefCell::new(Vec::new());
    let on_rejected = |details: UnsafeRetryableSignalRejected<'_>| {
        rejected.borrow_mut().push(details.status_code)
    };
    let options = RuntimeFallbackRetryOptions {
        on_unsafe_retryable_signal_rejected: Some(&on_rejected),
    };
    let results: Vec<bool> = cases
        .iter()
        .map(|(error, _)| {
            is_runtime_fallback_retryable_error(Some(error), &DEFAULT_RETRY_CODES, &options)
        })
        .collect();

    assert_eq!(
        results,
        cases
            .iter()
            .map(|(_, expected)| *expected)
            .collect::<Vec<_>>()
    );
    assert_eq!(*rejected.borrow(), vec![401]);
}

#[test]
fn treats_free_usage_exceeded_messages_as_retryable_runtime_fallback_errors() {
    let error = json!({ "message": "Free usage exceeded, subscribe to Go" });

    assert!(retryable(Some(&error), &DEFAULT_RETRY_CODES));
    assert_eq!(classify_runtime_fallback_error(Some(&error)), None);
}

#[test]
fn leaves_opencode_context_overflow_to_native_compaction() {
    let error = json!({
        "name": "ContextOverflowError",
        "data": {
            "message": "Your input exceeds the context window of this model. Please adjust your input and try again.",
            "responseBody": r#"{"error":{"message":"Your input exceeds the context window of this model. Please adjust your input and try again.","type":"invalid_request_error","code":"context_too_large"}}"#,
        },
    });
    let codes = [400, 429, 500, 502, 503, 504];

    assert_eq!(
        classify_runtime_fallback_error(Some(&error)),
        Some(RuntimeFallbackErrorType::ContextOverflow)
    );
    assert!(!retryable(Some(&error), &codes));
}

#[test]
fn extracts_provider_auto_retry_signals_from_status_summary_or_details() {
    let summary = "All credentials for model claude-opus-4-7 are cooling down [retrying in 7m 56s attempt #1]";
    let retry_info = json!({ "summary": summary });

    assert_eq!(
        extract_runtime_fallback_auto_retry_signal(retry_info.as_object()),
        Some(RuntimeFallbackAutoRetrySignal {
            signal: summary.to_string()
        })
    );
}
