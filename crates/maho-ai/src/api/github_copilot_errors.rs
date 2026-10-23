//! Port of senpi packages/ai/src/api/github-copilot-errors.ts.

use reqwest::header::HeaderMap;
use serde_json::Value;

/// Read only the provider-owned error envelope, never presentation text or `type` buckets.
pub fn diagnostic_from_responses_event(event: &Value) -> Option<crate::types::ProviderDiagnostic> {
    if !matches!(event.get("type").and_then(Value::as_str), Some("error" | "response.failed")) {
        return None;
    }
    let error = event.pointer("/response/error").or_else(|| event.get("error")).unwrap_or(event);
    let status = error.get("status").or_else(|| event.get("status"));
    let code = error.get("code");
    if code.is_some_and(|code| !code.is_null() && !code.is_string()) {
        return None;
    }
    crate::utils::provider_diagnostic_vocabulary::classify_provider_failure(
        crate::utils::provider_diagnostic_vocabulary::ProviderFailureFacts {
            status,
            code: code.and_then(Value::as_str),
        },
    )
}

/// The structured ChatGPT producer is chatgpt-usage-limit.ts, not a prose classifier.
pub fn usage_limit_cause(
    provider: &str,
    status: Option<u16>,
    payload: &Value,
) -> Option<crate::types::AssistantErrorCause> {
    if provider != "chatgpt-subscription" {
        return None;
    }
    let error = payload.pointer("/response/error").or_else(|| payload.get("error"))?.as_object()?;
    let code = error.get("code").and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .or_else(|| error.get("type").and_then(Value::as_str));
    let limited = matches!(code, Some("usage_limit_reached" | "usage_not_included"))
        || status == Some(429)
        || (status.is_some() && code == Some("rate_limit_exceeded"));
    limited.then_some(crate::types::AssistantErrorCause::UsageLimit {
        scope: crate::types::UsageLimitScope::Account,
    })
}

pub fn diagnostic_from_openai_error(
    status: Option<u16>,
    body: Option<&str>,
) -> Option<crate::types::ProviderDiagnostic> {
    use crate::types::{
        ProviderDiagnostic, ProviderDiagnosticCategory as Category, ProviderDiagnosticEvidence,
    };
    if status.is_some_and(|status| !(400..=599).contains(&status)) {
        return None;
    }
    let parsed = body.and_then(|body| serde_json::from_str::<Value>(body).ok());
    let code = parsed.as_ref().and_then(|value| {
        value
            .get("error")
            .filter(|error| error.is_object())
            .unwrap_or(value)
            .get("code")
    });
    if code.is_some_and(|code| !code.is_null() && !code.is_string()) {
        return None;
    }
    let code = code.and_then(Value::as_str);
    let category = match code {
        Some("authentication_error" | "invalid_api_key") => Some(Category::Auth),
        Some("rate_limit_error" | "rate_limit_exceeded") => Some(Category::RateLimit),
        Some("insufficient_quota") => Some(Category::Quota),
        Some("context_length_exceeded") => Some(Category::ContextLimit),
        Some("invalid_request_error") => Some(Category::InvalidRequest),
        Some("overloaded_error" | "api_error" | "timeout_error") => {
            Some(Category::ProviderUnavailable)
        }
        Some("billing_error" | "permission_error" | "not_found_error") => Some(Category::Unknown),
        _ => None,
    };
    if let (Some(category), Some(code)) = (category, code) {
        return crate::types::sanitize_provider_diagnostic(&ProviderDiagnostic {
            category,
            http_status: status,
            code: Some(code.into()),
            evidence: ProviderDiagnosticEvidence::StructuredCode,
            resets_at: None,
        });
    }
    crate::utils::event_stream::StreamError::diagnostic_from_status(status)
}

pub fn describe_github_copilot_failure(
    status: Option<u16>,
    body: Option<&str>,
    headers: Option<&HeaderMap>,
) -> Option<String> {
    let status = status?;
    let header = |name: &str| {
        headers
            .and_then(|headers| headers.get(name))
            .and_then(|value| value.to_str().ok())
    };
    let parsed = body.and_then(|body| serde_json::from_str::<Value>(body).ok());
    let code = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("error")
                .filter(|error| error.is_object())
                .unwrap_or(value)
                .get("code")
        })
        .and_then(Value::as_str);
    let quota = matches!(
        code,
        Some(
            "quota_exceeded"
                | "free_quota_exceeded"
                | "overage_limit_reached"
                | "billing_not_configured"
                | "additional_spend_limit_reached"
        )
    ) || status == 402
        || (status == 429
            && (header("x-ratelimit-exceeded").is_some_and(|value| value.contains("quota"))
                || body.is_some_and(|body| body.to_ascii_lowercase().contains("quota exceeded"))));
    let reference = header("x-github-request-id")
        .map(|id| {
            format!(
                " {}.",
                crate::utils::retry::format_provider_request_id("GitHub", id)
            )
        })
        .unwrap_or_default();
    if quota {
        return Some(format!(
            "GitHub Copilot quota exceeded (HTTP {status}): the plan's included usage or its additional-usage limit is used up, so premium models are refused until it resets or the limit is raised. Usage is shown at https://github.com/settings/copilot.{reference}"
        ));
    }
    match status {
        421 => Some(format!(
            "GitHub Copilot sent this account to a different API host (HTTP 421 Misdirected Request): Business and Enterprise accounts are served from their own host, which senpi reads from the Copilot token. Run /login github-copilot so senpi stores the account's endpoint, and report it with the GitHub request id if it persists.{reference}"
        )),
        403 => {
            let reason = if body.is_none() {
                " with an empty body"
            } else {
                ""
            };
            Some(format!(
                "GitHub Copilot refused the request (HTTP 403{reason}). Copilot checks access per model and per client; if a fresh /login github-copilot does not help, report it with the GitHub request id.{reference}"
            ))
        }
        _ => None,
    }
}

pub fn with_github_copilot_failure_note(
    message: String,
    provider: &str,
    status: Option<u16>,
    body: Option<&str>,
    headers: Option<&HeaderMap>,
) -> String {
    if provider == "github-copilot"
        && let Some(note) = describe_github_copilot_failure(status, body, headers)
    {
        return format!("{message}\n{note}");
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_usage_limits_do_not_classify_prose_or_other_providers() {
        // Given typed HTTP and event envelopes, plus an unrelated prose message.
        let event = serde_json::json!({"error":{"code":"usage_limit_reached"}});
        let prose = serde_json::json!({"error":{"message":"usage_limit_reached"}});
        // When projecting the producer-owned usage scope.
        let scope = usage_limit_cause("chatgpt-subscription", None, &event);
        // Then only the producer's structured evidence yields a cause.
        assert_eq!(scope, Some(crate::types::AssistantErrorCause::UsageLimit { scope: crate::types::UsageLimitScope::Account }));
        assert!(usage_limit_cause("openai", None, &event).is_none());
        assert!(usage_limit_cause("chatgpt-subscription", None, &prose).is_none());
        assert!(usage_limit_cause("chatgpt-subscription", Some(429), &prose).is_some());
        assert!(usage_limit_cause("chatgpt-subscription", Some(429), &serde_json::json!({})).is_none());
    }

    #[test]
    fn structured_code_classifies_quota_without_trusting_prose() {
        let diagnostic = diagnostic_from_openai_error(
            Some(429),
            Some(r#"{"error":{"code":"insufficient_quota"}}"#),
        )
        .expect("quota diagnostic");
        assert_eq!(
            diagnostic.category,
            crate::types::ProviderDiagnosticCategory::Quota
        );
        assert_eq!(diagnostic.code.as_deref(), Some("insufficient_quota"));
        assert!(
            diagnostic_from_openai_error(
                Some(400),
                Some(r#"{"error":{"code":"insufficient_quota"}}"#),
            )
            .is_none()
        );
        assert!(diagnostic_from_openai_error(None, Some("quota exceeded")).is_none());
    }

    #[test]
    fn quota_codes_are_recognized_independently_of_status() {
        // Given a structured quota error on an otherwise unclassified status.
        let body = r#"{"error":{"code":"overage_limit_reached"}}"#;
        // When classifying the response.
        let note = describe_github_copilot_failure(Some(400), Some(body), None);
        // Then it is classified, unlike a normal rate limit.
        assert!(note.is_some());
        assert!(describe_github_copilot_failure(Some(429), None, None).is_none());
    }

    #[test]
    fn other_providers_do_not_receive_copilot_notes() {
        // Given an unrelated provider with the same status.
        let message = "denied".to_owned();
        // When formatting it.
        let actual =
            with_github_copilot_failure_note(message.clone(), "other", Some(402), None, None);
        // Then its message is unchanged.
        assert_eq!(actual, message);
    }
}
