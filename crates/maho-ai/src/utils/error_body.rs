//! Port of senpi packages/ai/src/utils/error-body.ts.

use serde_json::Value;

pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderError {
    pub status: Option<u16>,
    pub body: Option<String>,
    pub message: String,
    pub message_carries_body: bool,
}

/// `$response.body`: a readable stream is never read.
#[derive(Debug, Clone, PartialEq)]
pub enum SdkResponseBody {
    Value(Value),
    Stream,
}

/// The SDK error fields the TS normalizer duck-types (`Error & {...}`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SdkErrorShape {
    pub message: String,
    pub status_code: Option<Value>,
    pub status: Option<Value>,
    pub body: Option<Value>,
    pub error: Option<Value>,
    pub metadata_http_status_code: Option<Value>,
    pub response_status_code: Option<Value>,
    pub response_body: Option<SdkResponseBody>,
}

/// A thrown value: an `Error` (with SDK fields) or anything else.
#[derive(Debug, Clone, PartialEq)]
pub enum ThrownProviderError {
    Error(Box<SdkErrorShape>),
    Other(Value),
}

pub fn normalize_provider_error(error: &ThrownProviderError) -> NormalizedProviderError {
    let sdk = match error {
        ThrownProviderError::Other(value) => {
            return NormalizedProviderError { status: None, body: None, message: safe_json_stringify(value), message_carries_body: false };
        }
        ThrownProviderError::Error(sdk) => sdk.as_ref(),
    };
    let status = extract_status(sdk);
    let body = extract_body(sdk);
    let message_carries_body = body.as_ref().is_none_or(|body| sdk.message.contains(body.as_str()));
    NormalizedProviderError { status, body, message: sdk.message.clone(), message_carries_body }
}

fn number(value: Option<&Value>) -> Option<u16> {
    value?.as_f64().and_then(|n| u16::try_from(n as i64).ok())
}

fn extract_status(error: &SdkErrorShape) -> Option<u16> {
    number(error.status_code.as_ref())
        .or_else(|| number(error.status.as_ref()))
        .or_else(|| number(error.metadata_http_status_code.as_ref()))
        .or_else(|| number(error.response_status_code.as_ref()))
}

fn extract_body(error: &SdkErrorShape) -> Option<String> {
    let text = pick_body_text(error)?;
    let trimmed = crate::utils::js::trim(&text);
    (!trimmed.is_empty()).then(|| truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

fn is_plain_non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|map| !map.is_empty())
}

fn pick_body_text(error: &SdkErrorShape) -> Option<String> {
    if let Some(Value::String(body)) = &error.body {
        return Some(body.clone());
    }
    if let Some(inner) = error.error.as_ref().filter(|v| is_plain_non_empty_object(v)) {
        return Some(safe_json_stringify(inner));
    }
    match &error.response_body {
        Some(SdkResponseBody::Value(Value::String(body))) => Some(body.clone()),
        Some(SdkResponseBody::Value(body)) if is_plain_non_empty_object(body) => Some(safe_json_stringify(body)),
        _ => None,
    }
}

pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    match (norm.message_carries_body, norm.status, norm.body.as_deref()) {
        (false, Some(status), Some(body)) => match prefix {
            Some(prefix) => format!("{prefix} ({status}): {body}"),
            None => format!("{status}: {body}"),
        },
        (_, status, _) => match (prefix, status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        },
    }
}

/// Truncates to `max_chars` UTF-16 units, matching `String.prototype.slice`.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let length = crate::utils::js::utf16_len(text);
    if length <= max_chars {
        return text.to_owned();
    }
    let head: Vec<u16> = text.encode_utf16().take(max_chars).collect();
    format!("{}... [truncated {} chars]", String::from_utf16_lossy(&head), length - max_chars)
}

pub fn safe_json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_and_formats_sdk_errors() {
        let sdk = SdkErrorShape {
            message: "Bad request".into(),
            status: Some(json!(400)),
            error: Some(json!({"type": "invalid_request_error"})),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(sdk)));
        assert_eq!(norm.body.as_deref(), Some(r#"{"type":"invalid_request_error"}"#));
        assert!(!norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, Some("Anthropic")), r#"Anthropic (400): {"type":"invalid_request_error"}"#);
        assert_eq!(format_provider_error(&norm, None), r#"400: {"type":"invalid_request_error"}"#);
        let carried = SdkErrorShape { message: "503 upstream".into(), body: Some(json!(" upstream ")), status_code: Some(json!(503)), ..Default::default() };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(carried)));
        assert!(norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, Some("X")), "X (503): 503 upstream");
        let stream = SdkErrorShape { message: "m".into(), response_body: Some(SdkResponseBody::Stream), ..Default::default() };
        assert_eq!(normalize_provider_error(&ThrownProviderError::Error(Box::new(stream))).body, None);
        let other = normalize_provider_error(&ThrownProviderError::Other(json!("boom")));
        assert_eq!((other.message.as_str(), other.message_carries_body), ("\"boom\"", false));
    }

    #[test]
    fn truncates_by_utf16_units() {
        assert_eq!(truncate_error_text("abcdef", 4), "abcd... [truncated 2 chars]");
        assert_eq!(truncate_error_text("abc", 4), "abc");
    }
}
