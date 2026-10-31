//! Port of senpi packages/ai/src/api/chatgpt-usage-limit.ts.
//!
//! The event and wording half (`describeChatGptUsageLimitEvent`, `describeChatGptUsageLimit`,
//! `CHATGPT_USAGE_LIMIT_PREFIX`) was ported next to its only caller in
//! `api/openai_codex_responses/usage_limit.rs`; this module is the upstream file's home and adds the
//! HTTP-response entry point, `describeChatGptUsageLimitResponse`.

use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::utils::diagnostics::now_ms;

pub use super::openai_codex_responses::usage_limit::{
    CHATGPT_USAGE_LIMIT_PREFIX, describe_chat_gpt_usage_limit, describe_chat_gpt_usage_limit_event,
};

fn has_code(error: &serde_json::Map<String, Value>, http: bool) -> bool {
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .or_else(|| error.get("type").and_then(Value::as_str));
    let Some(code) = code.map(str::to_lowercase) else {
        return false;
    };
    code.contains("usage_limit_reached")
        || code.contains("usage_not_included")
        || (http && code.contains("rate_limit_exceeded"))
}

/// HTTP refusal: a usage-limit code or any 429 whose JSON body carries an `error` object.
pub fn describe_chat_gpt_usage_limit_response(
    status: u16,
    body: &Value,
    headers: &HeaderMap,
) -> Option<String> {
    let error = body.as_object()?.get("error")?.as_object()?;
    if !(has_code(error, true) || status == 429) {
        return None;
    }
    let header = |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    Some(describe_chat_gpt_usage_limit(
        error.get("plan_type").and_then(Value::as_str),
        error.get("resets_at").and_then(Value::as_f64),
        &header,
        now_ms(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describes_usage_limit_codes_and_any_429_error_body() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-rate-limit-reached-type",
            "workspace_member_credits_depleted".parse().unwrap(),
        );
        let body = json!({"error": {"code": "usage_limit_reached", "plan_type": "team"}});
        let text = describe_chat_gpt_usage_limit_response(400, &body, &headers).expect("text");
        assert!(text.starts_with("You have hit your ChatGPT usage limit (team plan)."));
        assert!(text.contains("ask your workspace owner to add more"));
        let rate = json!({"error": {"type": "rate_limit_exceeded"}});
        assert!(describe_chat_gpt_usage_limit_response(400, &rate, &HeaderMap::new()).is_some());
        let other = json!({"error": {"code": "server_error"}});
        assert!(describe_chat_gpt_usage_limit_response(500, &other, &HeaderMap::new()).is_none());
        assert!(describe_chat_gpt_usage_limit_response(429, &other, &HeaderMap::new()).is_some());
        assert!(
            describe_chat_gpt_usage_limit_response(429, &json!({"detail": "x"}), &HeaderMap::new())
                .is_none()
        );
    }
}
