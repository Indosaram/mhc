//! Port of senpi packages/ai/src/api/openai-codex-responses/provider-diagnostic.ts.
//!
//! The providerDiagnostic for a ChatGPT-subscription (Codex backend) failure. The backend reports
//! a refusal three ways: an HTTP error body, an in-stream `error` event (the WebSocket form carries
//! the HTTP status inline) and a `response.failed` event whose error sits under `response.error`.
//! All three name the error by `error.code` or `error.type` and may name the reset by `resets_at`
//! (epoch seconds) or `resets_in_seconds`.

use serde_json::{Map, Value};

use crate::types::{
    ProviderDiagnostic, ProviderDiagnosticCategory, ProviderDiagnosticEvidence,
    sanitize_provider_diagnostic,
};
use crate::utils::provider_diagnostic_vocabulary::{
    ProviderFailureFacts, category_for_provider_code, classify_provider_failure,
    is_opaque_provider_code,
};

fn as_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value?.as_object()
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value?.as_str().filter(|text| !text.is_empty())
}

fn error_code(error: Option<&Map<String, Value>>) -> Option<&str> {
    non_empty_string(error.and_then(|error| error.get("code")))
        .or_else(|| non_empty_string(error.and_then(|error| error.get("type"))))
}

fn is_valid_resets_at(ms: i64) -> bool {
    ms > 946_684_800_000 && ms < 4_102_444_800_000
}

fn resets_at_ms(error: Option<&Map<String, Value>>, now_ms: i64) -> Option<i64> {
    let error = error?;
    if let Some(absolute) = error.get("resets_at").and_then(Value::as_f64)
        && absolute.is_finite()
        && absolute > 0.0
    {
        let ms = (if absolute < 1e12 {
            absolute * 1000.0
        } else {
            absolute
        })
        .round() as i64;
        return is_valid_resets_at(ms).then_some(ms);
    }
    if let Some(relative) = error.get("resets_in_seconds").and_then(Value::as_f64)
        && relative.is_finite()
        && relative >= 0.0
    {
        let ms = (now_ms as f64 + relative * 1000.0).round() as i64;
        return is_valid_resets_at(ms).then_some(ms);
    }
    None
}

struct CodexFailureFacts<'a> {
    status: Option<u16>,
    code: Option<&'a str>,
    /// The event type, used as the code when the error names none (`response.failed`).
    fallback_code: Option<&'a str>,
    resets_at: Option<i64>,
}

fn classify_codex_failure(facts: CodexFailureFacts<'_>) -> Option<ProviderDiagnostic> {
    let status = facts.status.filter(|status| (400..=599).contains(status));
    let status_value = status.map(Value::from);
    if let Some(code) = facts.code
        && category_for_provider_code(code).is_some()
    {
        let classified = classify_provider_failure(ProviderFailureFacts {
            status: status_value.as_ref(),
            code: Some(code),
        })
        .or_else(|| {
            classify_provider_failure(ProviderFailureFacts {
                status: None,
                code: Some(code),
            })
        })?;
        return sanitize_provider_diagnostic(&ProviderDiagnostic {
            resets_at: facts.resets_at.or(classified.resets_at),
            ..classified
        });
    }
    let by_status = status_value.as_ref().and_then(|status| {
        classify_provider_failure(ProviderFailureFacts {
            status: Some(status),
            code: None,
        })
    });
    if let Some(by_status) = &by_status
        && by_status.category != ProviderDiagnosticCategory::Unknown
    {
        return Some(by_status.clone());
    }
    let opaque = [facts.code, facts.fallback_code]
        .into_iter()
        .flatten()
        .find(|code| is_opaque_provider_code(code));
    if let Some(code) = opaque {
        return sanitize_provider_diagnostic(&ProviderDiagnostic {
            category: ProviderDiagnosticCategory::Unknown,
            http_status: None,
            code: Some(code.to_owned()),
            evidence: ProviderDiagnosticEvidence::StructuredCode,
            resets_at: None,
        });
    }
    by_status
}

pub fn codex_http_provider_diagnostic(
    status: u16,
    body_text: &str,
    now_ms: i64,
) -> Option<ProviderDiagnostic> {
    let body = serde_json::from_str::<Value>(body_text).ok();
    let error = as_record(body.as_ref().and_then(|body| body.get("error")));
    classify_codex_failure(CodexFailureFacts {
        status: Some(status),
        code: error_code(error),
        fallback_code: None,
        resets_at: resets_at_ms(error, now_ms),
    })
}

pub fn codex_event_provider_diagnostic(
    event: &Map<String, Value>,
    now_ms: i64,
) -> Option<ProviderDiagnostic> {
    let response = as_record(event.get("response"));
    let error = as_record(event.get("error"))
        .or_else(|| as_record(response.and_then(|response| response.get("error"))));
    let status = event
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    classify_codex_failure(CodexFailureFacts {
        status,
        code: error_code(error).or_else(|| non_empty_string(event.get("code"))),
        fallback_code: non_empty_string(event.get("type")),
        resets_at: resets_at_ms(error, now_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn http_usage_limit_reads_seconds_reset() {
        let body = json!({"error": {"code": "usage_limit_reached", "resets_at": 1_800_000_600}});
        let diagnostic =
            codex_http_provider_diagnostic(429, &body.to_string(), NOW).expect("diagnostic");
        assert_eq!(diagnostic.category, ProviderDiagnosticCategory::Quota);
        assert_eq!(diagnostic.resets_at, Some(1_800_000_600_000));
    }

    #[test]
    fn relative_reset_is_added_to_now() {
        let body = json!({"error": {"type": "rate_limit_exceeded", "resets_in_seconds": 60}});
        let diagnostic =
            codex_http_provider_diagnostic(429, &body.to_string(), NOW).expect("diagnostic");
        assert_eq!(diagnostic.category, ProviderDiagnosticCategory::RateLimit);
        assert_eq!(diagnostic.resets_at, Some(NOW + 60_000));
    }

    #[test]
    fn response_failed_reads_nested_error_and_opaque_event_type_falls_back() {
        let event = json!({"type": "response.failed", "response": {"error": {"code": "usage_not_included"}}});
        let diagnostic = codex_event_provider_diagnostic(event.as_object().expect("object"), NOW)
            .expect("diagnostic");
        assert_eq!(diagnostic.category, ProviderDiagnosticCategory::Quota);

        let opaque = json!({"type": "response.failed"});
        let diagnostic = codex_event_provider_diagnostic(opaque.as_object().expect("object"), NOW)
            .expect("opaque diagnostic");
        assert_eq!(diagnostic.category, ProviderDiagnosticCategory::Unknown);
        assert_eq!(diagnostic.code.as_deref(), Some("response.failed"));
    }

    #[test]
    fn bare_status_without_code_classifies_by_status() {
        let diagnostic = codex_http_provider_diagnostic(401, "not json", NOW).expect("diagnostic");
        assert_eq!(diagnostic.category, ProviderDiagnosticCategory::Auth);
    }
}
