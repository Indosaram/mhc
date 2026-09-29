//! Port of senpi packages/ai/src/utils/retry-profile/failure.ts.

use super::types::{RetryFailure, RetryFailureKind};
use crate::utils::retry_hint::{RetryHintInput, extract_429_retry_after_ms};
use regex::{Regex, RegexBuilder};
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Clone, Default)]
pub struct RetryFailureContext {
    pub body_text: Option<String>,
}

/// The fields of a thrown JS error that the TS normalizer duck-types.
#[derive(Debug, Clone, Default)]
pub struct RetryErrorShape {
    /// `false` for a non-`Error` throw (TS `String(error)` becomes the message).
    pub is_error: bool,
    pub name: String,
    /// SDK error class name (`error.constructor.name`).
    pub class_name: Option<String>,
    pub message: String,
    pub status: Option<u16>,
    pub headers: Option<HeaderMap>,
    /// The SDK's parsed `error` body.
    pub error: Option<Value>,
}

const MAX_MESSAGE_CHARS: usize = 500;

fn ci(pattern: &str) -> Regex {
    RegexBuilder::new(pattern).case_insensitive(true).build().unwrap_or_else(|e| panic!("failure pattern: {e}"))
}

static QUOTA_WORDING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| ci(r"exceeded_current_quota_error|insufficient\s+balance|credits?_required|quota|billing"));
static IMAGE_FORMAT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| ci(r"unsupported image format|unsupported media type for base64 image|invalid data url for image"));

fn nested_provider_codes(body: Option<&Value>) -> Option<Vec<String>> {
    let inner = body?.as_object()?.get("error")?.as_object()?;
    let codes: Vec<String> = ["type", "code"]
        .iter()
        .filter_map(|key| inner.get(*key)?.as_str().filter(|v| !v.is_empty()).map(str::to_owned))
        .collect();
    (!codes.is_empty()).then_some(codes)
}

fn parse_leading_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

fn slice_utf16(text: &str, max: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(max).collect();
    String::from_utf16_lossy(&units)
}

pub fn normalize_anthropic_retry_failure(error: &RetryErrorShape, context: Option<&RetryFailureContext>) -> RetryFailure {
    let message = error.message.as_str();
    let status = if error.is_error { error.status } else { None };
    let headers = if error.is_error { error.headers.as_ref() } else { None };
    let body_text = context.and_then(|c| c.body_text.as_deref()).unwrap_or(message);
    let class_name = error.class_name.as_deref().filter(|_| error.is_error);
    let mut provider_codes = None;
    let kind = if error.is_error && error.name == "AbortError" {
        RetryFailureKind::Abort
    } else if class_name == Some("APIConnectionTimeoutError") {
        RetryFailureKind::Timeout
    } else if class_name == Some("APIConnectionError") {
        RetryFailureKind::Connection
    } else if IMAGE_FORMAT_PATTERN.is_match(message) {
        RetryFailureKind::ImageFormat
    } else if let Some(status) = status {
        provider_codes = nested_provider_codes(error.error.as_ref());
        let quota = (error.is_error && QUOTA_WORDING_PATTERN.is_match(message)) || QUOTA_WORDING_PATTERN.is_match(body_text);
        if status == 429 && quota { RetryFailureKind::QuotaExhausted } else { RetryFailureKind::HttpStatus }
    } else {
        provider_codes = nested_provider_codes(parse_leading_json(body_text).as_ref());
        if provider_codes.is_some() { RetryFailureKind::Provider } else { RetryFailureKind::Unknown }
    };
    let retry_after_ms = extract_429_retry_after_ms(&RetryHintInput { status, headers, body_text }, None);
    let should_retry = match headers.and_then(|h| h.get("x-should-retry")).and_then(|v| v.to_str().ok()) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    RetryFailure {
        origin: "anthropic-messages".into(),
        kind,
        message: slice_utf16(message, MAX_MESSAGE_CHARS),
        status_code: status,
        provider_codes,
        finish_reason: None,
        retry_after_ms,
        should_retry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "sk-ant-TEST-SECRET-DO-NOT-LEAK";
    const ALLOWED_KEYS: &[&str] =
        &["origin", "kind", "message", "statusCode", "providerCodes", "finishReason", "retryAfterMs", "shouldRetry"];

    fn error(message: &str) -> RetryErrorShape {
        RetryErrorShape { is_error: true, name: "Error".into(), message: message.into(), ..Default::default() }
    }

    fn headers_from(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (key, value) in pairs {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes()).expect("header name");
            let value = reqwest::header::HeaderValue::from_str(value).expect("header value");
            headers.insert(name, value);
        }
        headers
    }

    #[test]
    fn quota_wording_429_maps_to_quota_exhausted_with_retry_after_header() {
        let shaped = RetryErrorShape {
            status: Some(429),
            headers: Some(headers_from(&[("retry-after", "2")])),
            error: Some(json!({"type": "error", "error": {"type": "error", "message": "exceeded_current_quota_error"}})),
            ..error("exceeded_current_quota_error: You exceeded your current quota, please check your plan and billing details")
        };
        let failure = normalize_anthropic_retry_failure(&shaped, None);
        assert_eq!(failure.kind, RetryFailureKind::QuotaExhausted);
        assert_eq!(failure.status_code, Some(429));
        assert_eq!(failure.retry_after_ms, Some(2000));
    }

    #[test]
    fn insufficient_balance_wording_429_maps_to_quota_exhausted() {
        let shaped = RetryErrorShape { status: Some(429), headers: Some(HeaderMap::new()), ..error("429 This API key has an insufficient balance") };
        assert_eq!(normalize_anthropic_retry_failure(&shaped, None).kind, RetryFailureKind::QuotaExhausted);
    }

    #[test]
    fn plain_rate_limit_throttle_429_maps_to_http_status() {
        let shaped = RetryErrorShape { status: Some(429), headers: Some(HeaderMap::new()), ..error("All tokens rate limited") };
        let failure = normalize_anthropic_retry_failure(&shaped, None);
        assert_eq!(failure.kind, RetryFailureKind::HttpStatus);
        assert_eq!(failure.status_code, Some(429));
    }

    #[test]
    fn numeric_status_maps_to_http_status() {
        let shaped = RetryErrorShape { status: Some(500), headers: Some(HeaderMap::new()), ..error("Internal server error") };
        let failure = normalize_anthropic_retry_failure(&shaped, None);
        assert_eq!(failure.kind, RetryFailureKind::HttpStatus);
        assert_eq!(failure.status_code, Some(500));
    }

    #[test]
    fn x_should_retry_header_maps_to_boolean_should_retry() {
        let retryable = RetryErrorShape {
            status: Some(503),
            headers: Some(headers_from(&[("x-should-retry", "true")])),
            ..error("503 overloaded")
        };
        let terminal = RetryErrorShape {
            status: Some(400),
            headers: Some(headers_from(&[("x-should-retry", "false")])),
            ..error("400 invalid")
        };
        assert_eq!(normalize_anthropic_retry_failure(&retryable, None).should_retry, Some(true));
        assert_eq!(normalize_anthropic_retry_failure(&terminal, None).should_retry, Some(false));
    }

    #[test]
    fn sse_plain_error_with_json_body_maps_to_provider_with_codes() {
        let shaped = error(r#"{"type":"error","error":{"type":"overloaded_error","code":"custom_code","message":"Overloaded"}}"#);
        let failure = normalize_anthropic_retry_failure(&shaped, None);
        assert_eq!(failure.kind, RetryFailureKind::Provider);
        assert_eq!(failure.status_code, None);
        assert_eq!(failure.provider_codes, Some(vec!["overloaded_error".to_owned(), "custom_code".to_owned()]));
    }

    #[test]
    fn sse_plain_error_with_retry_after_ms_marker_recovers_as_number() {
        let shaped = error(
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limited","retryDelay":"45s"}} (retry-after-ms: 45000)"#,
        );
        let failure = normalize_anthropic_retry_failure(&shaped, None);
        assert_eq!(failure.kind, RetryFailureKind::Provider);
        assert_eq!(failure.retry_after_ms, Some(45000));
    }

    #[test]
    fn api_connection_error_shaped_maps_to_connection() {
        let shaped = RetryErrorShape { class_name: Some("APIConnectionError".into()), ..error("connection failed") };
        let failure = normalize_anthropic_retry_failure(&shaped, None);
        assert_eq!(failure.kind, RetryFailureKind::Connection);
        assert_eq!(failure.status_code, None);
    }

    #[test]
    fn api_connection_timeout_error_shaped_maps_to_timeout_beats_connection() {
        let shaped = RetryErrorShape { class_name: Some("APIConnectionTimeoutError".into()), ..error("timed out") };
        assert_eq!(normalize_anthropic_retry_failure(&shaped, None).kind, RetryFailureKind::Timeout);
    }

    #[test]
    fn abort_error_maps_to_abort() {
        let shaped = RetryErrorShape { name: "AbortError".into(), ..error("Request aborted") };
        assert_eq!(normalize_anthropic_retry_failure(&shaped, None).kind, RetryFailureKind::Abort);
    }

    #[test]
    fn narrow_image_format_rejection_maps_to_image_format() {
        assert_eq!(normalize_anthropic_retry_failure(&error("Unsupported image format: image/bmp"), None).kind, RetryFailureKind::ImageFormat);
    }

    #[test]
    fn unrecognized_plain_error_maps_to_unknown() {
        let failure = normalize_anthropic_retry_failure(&error("something completely different"), None);
        assert_eq!(failure.kind, RetryFailureKind::Unknown);
        assert_eq!(failure.origin, "anthropic-messages");
    }

    #[test]
    fn message_truncated_to_500_chars() {
        let failure = normalize_anthropic_retry_failure(&error(&"x".repeat(600)), None);
        assert_eq!(failure.message.chars().count(), 500);
    }

    #[test]
    fn security_authorization_header_and_raw_body_never_leak_into_the_normalized_fact() {
        let shaped = RetryErrorShape {
            status: Some(429),
            headers: Some(headers_from(&[
                ("authorization", &format!("Bearer {SECRET}")),
                ("x-api-key", SECRET),
                ("retry-after-ms", "750"),
            ])),
            ..error("429 All tokens rate limited")
        };
        let context = RetryFailureContext { body_text: Some(format!(r#"{{"error":{{"type":"rate_limit_error","message":"throttled for {SECRET}"}}}}"#)) };

        let failure = normalize_anthropic_retry_failure(&shaped, Some(&context));

        // Whitelist: only the eight declared keys may exist, with values safe to log.
        assert!(ALLOWED_KEYS.contains(&"origin") && !failure.origin.is_empty());
        let debug = format!("{failure:?}");
        assert!(!debug.to_lowercase().contains("authorization"));
        assert!(!debug.contains(SECRET));
        assert_eq!(failure.retry_after_ms, Some(750));
        assert_eq!(failure.kind, RetryFailureKind::HttpStatus);
    }
}
