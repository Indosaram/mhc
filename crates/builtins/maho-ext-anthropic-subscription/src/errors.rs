use std::fmt;
use regex::Regex;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdkErrorKind { RateLimit, Overloaded, AuthError, Billing, OrgNotAllowed, Entitlement, Other }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SdkErrorClassification { pub kind: SdkErrorKind, pub retryable: bool }
#[derive(Clone, Debug, PartialEq)]
pub struct SdkResultFailure { pub message: String, pub usage: Option<Value> }
impl fmt::Display for SdkResultFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for SdkResultFailure {}

pub fn sdk_result_failure(message: &Value) -> Option<SdkResultFailure> {
    if message["subtype"] == "success" && message["is_error"] != true { return None; }
    let first_error = message["errors"].as_array().and_then(|errors| errors.iter().filter_map(Value::as_str).find(|text| !text.is_empty()));
    let result_text = message["result"].as_str().unwrap_or("").trim();
    let fallback = format!("Claude Code {}", message["subtype"].as_str().unwrap_or("undefined"));
    let detail = first_error.or_else(|| (message["is_error"] == true && !result_text.is_empty()).then_some(result_text)).unwrap_or(&fallback);
    let status = message.get("api_error_status").filter(|status| !status.is_null()).map(|status| format!("HTTP {}", status.as_str().map(String::from).unwrap_or_else(|| status.to_string())));
    let reason = message["terminal_reason"].as_str().filter(|reason| !reason.is_empty());
    let suffix = status.iter().map(String::as_str).chain(reason).collect::<Vec<_>>().join(", ");
    Some(SdkResultFailure {
        message: if suffix.is_empty() { detail.into() } else { format!("{detail} ({suffix})") },
        usage: message.get("usage").cloned(),
    })
}

pub fn sdk_result_failure_usage(error: &SdkResultFailure) -> Option<&Value> { error.usage.as_ref() }

pub fn sdk_assistant_failure(message: &Value) -> Option<String> {
    let error = message["error"].as_str().filter(|error| !error.is_empty())?;
    let content = &message["message"]["content"];
    let text = content.as_str().map(String::from).unwrap_or_else(|| {
        content.as_array().into_iter().flatten()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join(" ")
    });
    let text = text.trim();
    Some(if text.is_empty() { error.into() } else if error == "unknown" { text.into() } else { format!("{text} ({error})") })
}

fn matches(pattern: &str, text: &str) -> bool { Regex::new(pattern).is_ok_and(|regex| regex.is_match(text)) }

pub fn classify_sdk_error(error: &Value) -> SdkErrorClassification {
    use SdkErrorKind::*;
    let text = error.as_str().or_else(|| error["message"].as_str()).or_else(|| error["error"].as_str())
        .map(String::from).unwrap_or_else(|| error.to_string()).to_lowercase();
    let result = |kind, retryable| SdkErrorClassification { kind, retryable };
    if matches(r"(?-u)\b(enotfound|eai_again|econnreset|econnrefused|etimedout|enetunreach|ehostunreach|und_err_connect_timeout|und_err_socket)\b|fetch failed|socket hang up|connection reset by peer", &text) { return result(Other, true); }
    for (code, kind, retryable) in [
        ("authentication_failed", AuthError, true), ("oauth_org_not_allowed", OrgNotAllowed, true),
        ("billing_error", Billing, true), ("rate_limit", RateLimit, true), ("overloaded", Overloaded, true),
        ("invalid_request", Other, false), ("server_error", Other, true), ("account_on_hold", Billing, true),
        ("model_not_found", Other, false), ("max_output_tokens", Other, false),
    ] {
        if matches(&format!(r"(?-u)\b{code}\b"), &text) { return result(kind, retryable); }
    }
    if matches(r"(?-u)\b(?:http\s*)?429\b|too many requests|rate[ _-]?limit|\bblocking_limit\b|\brapid_refill_breaker\b|\b(?:hit|reached|exceeded)\b(?u:[^.]*)\blimit\b|\b(?:weekly|daily|hourly|usage)\s+limit\b", &text) { return result(RateLimit, true); }
    if matches(r"(?-u)\b(?:http\s*)?529\b|overloaded", &text) { return result(Overloaded, true); }
    if matches(r"(?-u)\binvalid_grant\b|\binvalid_token\b|\b(?:http\s*)?401\b|\bunauthorized\b", &text) { return result(AuthError, true); }
    if matches(r"(?-u)\brequires usage credits\b|/usage-credits\b|\bcredits_required\b", &text) { return result(Entitlement, false); }
    result(Other, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn extracts_first_upstream_result_failure_scenario() {
        let result = json!({"type":"result", "subtype":"success", "is_error":true, "api_error_status":400, "terminal_reason":"api_error", "result":"API Error: 400 Claude Code 2.1.241 does not support this model; version 2.1.251 or newer is required.", "usage":{"input_tokens":37}});
        let failure = sdk_result_failure(&result).unwrap();
        assert!(failure.message.contains("does not support this model"));
        assert!(failure.message.ends_with("(HTTP 400, api_error)"));
        assert_eq!(sdk_result_failure_usage(&failure), Some(&json!({"input_tokens":37})));
        assert!(sdk_result_failure(&json!({"subtype":"success", "is_error":false, "result":"ok"})).is_none());
        assert_eq!(sdk_result_failure(&json!({"subtype":"error_during_execution", "is_error":true, "errors":["You've hit your session limit"]})).unwrap().message, "You've hit your session limit");
    }
    #[test]
    fn assistant_text_and_codes() {
        assert_eq!(sdk_assistant_failure(&json!({"error":"unknown", "message":{"content":[{"type":"text", "text":"model version floor"}]}})), Some("model version floor".into()));
        let text = sdk_assistant_failure(&json!({"error":"rate_limit", "message":{"content":[{"type":"text", "text":"You've hit your session limit"}]}})).unwrap();
        assert!(text.ends_with("(rate_limit)"));
        assert_eq!(classify_sdk_error(&json!(text)), SdkErrorClassification { kind: SdkErrorKind::RateLimit, retryable: true });
    }
    #[test]
    fn transport_failures_precede_auth_classification() {
        assert_eq!(classify_sdk_error(&json!("authentication_failed: getaddrinfo ENOTFOUND platform.claude.com")), SdkErrorClassification { kind: SdkErrorKind::Other, retryable: true });
        assert_eq!(classify_sdk_error(&json!("invalid_grant")), SdkErrorClassification { kind: SdkErrorKind::AuthError, retryable: true });
    }
    #[test]
    fn usage_credits_are_entitlement() {
        for text in ["Fable 5 requires usage credits. Run /usage-credits to continue", "credits_required"] {
            assert_eq!(classify_sdk_error(&json!(text)), SdkErrorClassification { kind: SdkErrorKind::Entitlement, retryable: false });
        }
    }
    #[test]
    fn exhaustion_and_unknown_cases() {
        for text in ["blocking_limit", "rapid_refill_breaker", "You've hit your weekly limit", "daily limit exceeded"] {
            assert_eq!(classify_sdk_error(&json!(text)).kind, SdkErrorKind::RateLimit);
        }
        assert_eq!(classify_sdk_error(&json!("HTTP 529")).kind, SdkErrorKind::Overloaded);
        assert!(!classify_sdk_error(&json!("model_not_found")).retryable);
        assert!(!classify_sdk_error(&json!("unknown error")).retryable);
    }
}
