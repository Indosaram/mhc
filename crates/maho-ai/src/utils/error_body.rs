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
    /// A non-stream SDK wrapper/class instance (e.g. an AWS SDK v3 HTTP response wrapper). The
    /// TS prototype check (`Object.getPrototypeOf(value) !== Object.prototype`) rejects these the
    /// same way it rejects a readable stream: no body is surfaced and `error.message` wins.
    ClassInstance,
    Stream,
}

/// A JSON-shaped value duck-typed off the TS `unknown` field: either a genuine plain object /
/// primitive (serializes and normalizes like TS's own plain-object check), or a class instance
/// whose fields TS's `Object.getPrototypeOf(value) !== Object.prototype` check rejects as "not a
/// parsed body" regardless of how many enumerable fields it carries.
#[derive(Debug, Clone, PartialEq)]
pub enum SdkFieldValue {
    Plain(Value),
    ClassInstance,
}

/// The SDK error fields the TS normalizer duck-types (`Error & {...}`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SdkErrorShape {
    pub message: String,
    pub status_code: Option<Value>,
    pub status: Option<Value>,
    pub body: Option<Value>,
    pub error: Option<SdkFieldValue>,
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
    if let Some(SdkFieldValue::Plain(inner)) = error.error.as_ref().filter(|v| matches!(v, SdkFieldValue::Plain(value) if is_plain_non_empty_object(value))) {
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
    fn extracts_status_and_body_from_a_mistral_shaped_error() {
        let error = SdkErrorShape {
            message: "Mistral request failed".into(),
            status_code: Some(json!(403)),
            body: Some(json!("{\"error\":\"blocked by gateway WAF\"}")),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.status, Some(403));
        assert_eq!(norm.body.as_deref(), Some("{\"error\":\"blocked by gateway WAF\"}"));
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn reads_the_parsed_body_off_an_openai_apierror_when_the_message_is_opaque() {
        let error = SdkErrorShape {
            message: "403 status code (no body)".into(),
            status: Some(json!(403)),
            error: Some(SdkFieldValue::Plain(json!({"error": "blocked by gateway WAF"}))),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.status, Some(403));
        assert_eq!(norm.body.as_deref(), Some("{\"error\":\"blocked by gateway WAF\"}"));
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn preserves_the_message_when_google_genai_already_folds_the_body_into_it() {
        let body = json!({"error": {"code": 403, "message": "Permission denied"}});
        let message = serde_json::to_string(&body).unwrap();
        let error = SdkErrorShape { message: message.clone(), status: Some(json!(403)), ..Default::default() };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.status, Some(403));
        assert!(norm.message_carries_body);
        assert_eq!(norm.message, message);
    }

    #[test]
    fn extracts_status_and_body_from_a_bedrock_shaped_service_exception() {
        let error = SdkErrorShape {
            message: "UnknownError".into(),
            metadata_http_status_code: Some(json!(403)),
            response_status_code: Some(json!(403)),
            response_body: Some(SdkResponseBody::Value(json!("{\"message\":\"blocked by gateway WAF\"}"))),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.status, Some(403));
        assert_eq!(norm.body.as_deref(), Some("{\"message\":\"blocked by gateway WAF\"}"));
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn ignores_a_bedrock_response_stream_instead_of_serializing_its_internals() {
        let error = SdkErrorShape {
            message: "Invocation of model ID anthropic.claude-opus-5 with on-demand throughput isn't supported.".into(),
            metadata_http_status_code: Some(json!(400)),
            response_status_code: Some(json!(400)),
            response_body: Some(SdkResponseBody::Stream),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.status, Some(400));
        assert_eq!(norm.body, None);
        assert!(norm.message.contains("on-demand throughput isn't supported"));
        assert!(norm.message_carries_body);
    }

    #[test]
    fn ignores_a_class_instance_response_body_without_a_pipe_method_instead_of_serializing_it() {
        // TS constructs `new SdkHttpResponseBody()` (a class instance with `locked`/`state`
        // fields, no `pipe`). `SdkResponseBody::ClassInstance` models any non-stream SDK wrapper
        // object the TS prototype check (`Object.getPrototypeOf(value) !== Object.prototype`)
        // rejects the same way it rejects a Node stream: no body surfaces, the real message wins.
        let error = SdkErrorShape {
            message: "Input is too long for requested model.".into(),
            metadata_http_status_code: Some(json!(400)),
            response_status_code: Some(json!(400)),
            response_body: Some(SdkResponseBody::ClassInstance),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.status, Some(400));
        assert_eq!(norm.body, None);
        assert!(norm.message.contains("Input is too long"));
        assert!(norm.message_carries_body);
    }

    #[test]
    fn ignores_a_class_instance_error_field_instead_of_serializing_it() {
        // TS: `new SdkInnerError()` (`code`/`internalState` fields). `SdkFieldValue::ClassInstance`
        // is the same "reject a non-plain instance" signal `pick_body_text` treats as no body.
        let error = SdkErrorShape {
            message: "TLS handshake failed".into(),
            status: Some(json!(502)),
            error: Some(SdkFieldValue::ClassInstance),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.body, None);
        assert_eq!(norm.message, "TLS handshake failed");
        assert!(norm.message_carries_body);
    }

    #[test]
    fn still_surfaces_a_plain_parsed_json_body_object() {
        let error = SdkErrorShape {
            message: "400 status code (no body)".into(),
            status: Some(json!(400)),
            error: Some(SdkFieldValue::Plain(json!({"message": "schema validation failed", "field": "tools[0]"}))),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.body.as_deref(), Some("{\"message\":\"schema validation failed\",\"field\":\"tools[0]\"}"));
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn json_stringifies_a_non_error_thrown_value() {
        let norm = normalize_provider_error(&ThrownProviderError::Other(json!({"reason": "boom"})));
        assert_eq!(norm.status, None);
        assert_eq!(norm.body, None);
        assert_eq!(norm.message, "{\"reason\":\"boom\"}");
        assert!(!norm.message_carries_body);
    }

    #[test]
    fn treats_an_empty_parsed_body_object_as_no_body() {
        let error = SdkErrorShape {
            message: "403 status code (no body)".into(),
            status: Some(json!(403)),
            error: Some(SdkFieldValue::Plain(json!({}))),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(norm.body, None);
        assert!(norm.message_carries_body);
    }

    #[test]
    fn truncates_the_body_at_the_cap() {
        let long_body = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
        let error = SdkErrorShape { message: "failed".into(), status_code: Some(json!(500)), body: Some(json!(long_body.clone())), ..Default::default() };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        let body = norm.body.expect("body");
        assert!(body.contains("... [truncated 50 chars]"));
        assert!(body.len() < long_body.len());
    }

    #[test]
    fn sets_message_carries_body_when_the_message_already_contains_the_extracted_body() {
        let error = SdkErrorShape {
            message: "500: upstream exploded".into(),
            status_code: Some(json!(500)),
            body: Some(json!("upstream exploded")),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert!(norm.message_carries_body);
    }

    #[test]
    fn surfaces_status_and_body_without_a_prefix() {
        let error = SdkErrorShape {
            message: "403 status code (no body)".into(),
            status: Some(json!(403)),
            error: Some(SdkFieldValue::Plain(json!({"error": "blocked by gateway WAF"}))),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        let formatted = format_provider_error(&norm, None);
        assert!(formatted.contains("403"));
        assert!(formatted.contains("blocked by gateway WAF"));
        assert_ne!(formatted, "403 status code (no body)");
    }

    #[test]
    fn applies_a_provider_prefix_with_status_and_body() {
        let error = SdkErrorShape {
            message: "403 status code (no body)".into(),
            status: Some(json!(403)),
            error: Some(SdkFieldValue::Plain(json!({"error": "blocked by gateway WAF"}))),
            ..Default::default()
        };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(
            format_provider_error(&norm, Some("OpenAI API error")),
            "OpenAI API error (403): {\"error\":\"blocked by gateway WAF\"}"
        );
    }

    #[test]
    fn preserves_the_message_with_prefix_plus_status_when_it_already_carries_the_body() {
        let body = serde_json::to_string(&json!({"error": {"message": "Permission denied"}})).unwrap();
        let error = SdkErrorShape { message: body.clone(), status: Some(json!(403)), ..Default::default() };
        let norm = normalize_provider_error(&ThrownProviderError::Error(Box::new(error)));
        assert_eq!(format_provider_error(&norm, Some("OpenAI API error")), format!("OpenAI API error (403): {body}"));
    }

    #[test]
    fn returns_the_bare_message_for_a_non_error_value() {
        let norm = normalize_provider_error(&ThrownProviderError::Other(json!({"reason": "boom"})));
        assert_eq!(format_provider_error(&norm, None), "{\"reason\":\"boom\"}");
    }

    #[test]
    fn truncates_by_utf16_units() {
        assert_eq!(truncate_error_text("abcdef", 4), "abcd... [truncated 2 chars]");
        assert_eq!(truncate_error_text("abc", 4), "abc");
    }
}
