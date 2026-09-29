use model_core::ErrorInfo;
use model_core::is_retryable_model_error;
use model_core::select_fallback_provider;
use model_core::select_fallback_provider_with_cache;
use model_core::should_retry_error;

use crate::support::ConnectedCache;
use crate::support::strings;

fn message(message: &str) -> ErrorInfo {
    ErrorInfo {
        message: Some(message.to_string()),
        ..Default::default()
    }
}

fn named(name: &str) -> ErrorInfo {
    ErrorInfo {
        name: Some(name.to_string()),
        ..Default::default()
    }
}

fn status(status_code: i64, message: Option<&str>) -> ErrorInfo {
    ErrorInfo {
        name: None,
        message: message.map(str::to_string),
        status_code: Some(status_code),
    }
}

#[test]
fn treats_overloaded_retry_messages_as_retryable() {
    assert!(should_retry_error(&message("Provider is overloaded")));
}

#[test]
fn treats_cooling_down_auto_retry_messages_as_retryable() {
    assert!(should_retry_error(&message(
        "All credentials for model claude-opus-4-7-thinking are cooling down [retrying in ~5 days attempt #1]"
    )));
}

#[test]
fn select_fallback_provider_prefers_first_connected_provider_in_preference_order() {
    let cache = ConnectedCache::with(&["anthropic", "nvidia"]);
    assert_eq!(
        select_fallback_provider_with_cache(
            &strings(&["anthropic", "nvidia"]),
            &cache,
            Some("nvidia")
        ),
        "anthropic"
    );
}

#[test]
fn select_fallback_provider_falls_back_to_next_connected_provider_when_first_is_disconnected() {
    let cache = ConnectedCache::with(&["nvidia"]);
    assert_eq!(
        select_fallback_provider_with_cache(&strings(&["anthropic", "nvidia"]), &cache, None),
        "nvidia"
    );
}

#[test]
fn select_fallback_provider_uses_provider_preference_order_when_cache_is_missing() {
    assert_eq!(
        select_fallback_provider(&strings(&["anthropic", "nvidia"]), Some("nvidia")),
        "anthropic"
    );
}

#[test]
fn select_fallback_provider_uses_connected_preferred_provider_when_fallback_providers_are_unavailable()
 {
    let cache = ConnectedCache::with(&["provider-x"]);
    assert_eq!(
        select_fallback_provider_with_cache(&strings(&["provider-y"]), &cache, Some("provider-x")),
        "provider-x"
    );
}

#[test]
fn treats_quota_exceeded_error_pascal_case_name_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&named("QuotaExceededError")));
}

#[test]
fn treats_quota_exceeded_error_lowercase_name_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&named("quotaexceedederror")));
}

#[test]
fn treats_insufficient_credits_error_pascal_case_name_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&named("InsufficientCreditsError")));
}

#[test]
fn treats_insufficient_credits_error_lowercase_name_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&named("insufficientcreditserror")));
}

#[test]
fn treats_free_usage_limit_error_pascal_case_name_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&named("FreeUsageLimitError")));
}

#[test]
fn treats_free_usage_limit_error_lowercase_name_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&named("freeusagelimiterror")));
}

#[test]
fn treats_quota_reset_message_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&message(
        "quota will reset after 1 hour"
    )));
}

#[test]
fn treats_quota_exceeded_message_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&message(
        "quota exceeded for this billing period"
    )));
}

#[test]
fn treats_provider_usage_limit_reached_message_as_retryable_fallback_signal() {
    assert!(should_retry_error(&message(
        "usage limit has been reached for your account"
    )));
}

#[test]
fn treats_insufficient_credits_message_as_non_retryable_stop_error() {
    assert!(!should_retry_error(&message(
        "insufficient credits to complete this request"
    )));
}

#[test]
fn treats_bad_request_message_as_retryable() {
    assert!(should_retry_error(&message("400 Bad Request")));
}

#[test]
fn treats_bad_request_lowercase_as_retryable() {
    assert!(should_retry_error(&message(
        "bad request: model temporarily unavailable"
    )));
}

#[test]
fn treats_localized_transient_provider_messages_as_retryable() {
    let results: Vec<bool> = ["请求过于频繁，请稍后重试", "服务暂时不可用", "触发频率限制"]
        .iter()
        .map(|text| should_retry_error(&message(text)))
        .collect();
    assert_eq!(results, vec![true, true, true]);
}

#[test]
fn treats_subscription_quota_message_as_non_retryable() {
    assert!(!should_retry_error(&message(
        "Subscription quota exceeded. You can continue using free models."
    )));
}

#[test]
fn treats_localized_quota_exhaustion_messages_as_non_retryable_stop_errors() {
    let results: Vec<bool> = [
        "已达到 5 小时的使用上限",
        "额度不足",
        "账户余额不足",
        "免费额度已耗尽",
    ]
    .iter()
    .map(|text| should_retry_error(&message(text)))
    .collect();
    assert_eq!(results, vec![false, false, false, false]);
}

#[test]
fn treats_http_429_rate_limit_message_as_retryable() {
    assert!(should_retry_error(&message(
        "429 Too Many Requests: rate limit reached"
    )));
}

#[test]
fn treats_forbidden_provider_message_as_retryable() {
    assert!(should_retry_error(&message(
        "Forbidden: Selected provider is forbidden"
    )));
}

#[test]
fn does_not_treat_unrelated_forbidden_messages_as_retryable() {
    assert!(!should_retry_error(&message(
        "EACCES: forbidden write to /etc/hosts"
    )));
}

#[test]
fn does_not_treat_unrelated_403_messages_as_retryable() {
    assert!(!should_retry_error(&message(
        "Tool returned HTTP 403 for the requested URL"
    )));
}

#[test]
fn glm_429_with_status_code_and_chinese_message_triggers_fallback() {
    assert!(is_retryable_model_error(&status(429, Some("请求频率过高"))));
}

#[test]
fn glm_429_with_status_code_and_no_message_triggers_fallback() {
    assert!(is_retryable_model_error(&status(429, None)));
}

#[test]
fn glm_503_service_unavailable_with_status_code_triggers_fallback() {
    assert!(is_retryable_model_error(&status(
        503,
        Some("Service Unavailable")
    )));
}

#[test]
fn glm_529_overloaded_with_status_code_triggers_fallback() {
    assert!(is_retryable_model_error(&status(529, None)));
}

#[test]
fn http_400_with_status_code_does_not_trigger_fallback_via_status_code_alone() {
    assert!(!is_retryable_model_error(&status(
        400,
        Some("Invalid parameter: model_name")
    )));
}

#[test]
fn http_401_with_status_code_does_not_trigger_fallback() {
    assert!(!is_retryable_model_error(&status(
        401,
        Some("Unauthorized")
    )));
}

#[test]
fn glm_daily_quota_429_does_not_trigger_fallback() {
    assert!(!is_retryable_model_error(&status(
        429,
        Some(
            "Daily call limit for this API key has been reached. Limit will reset at midnight UTC."
        )
    )));
}

#[test]
fn glm_account_in_arrears_429_does_not_trigger_fallback() {
    assert!(!is_retryable_model_error(&status(
        429,
        Some("Your account is in arrears, please recharge and try again.")
    )));
}

#[test]
fn glm_fair_use_policy_violation_429_does_not_trigger_fallback() {
    assert!(!is_retryable_model_error(&status(
        429,
        Some("Request blocked under Fair Use Policy. Your request rate has been restricted.")
    )));
}

#[test]
fn stop_message_pattern_takes_precedence_over_429_status_code() {
    assert!(!is_retryable_model_error(&status(
        429,
        Some("quota exceeded for this account, usage limit has been reached")
    )));
}

#[test]
fn rate_limit_message_without_status_code_still_works() {
    assert!(is_retryable_model_error(&message(
        "rate limit reached for requests"
    )));
}

#[test]
fn treats_openai_streaming_server_error_envelopes_as_retryable() {
    assert!(should_retry_error(&message(
        r#"{"error":{"type":"server_error","message":"server_error"}}"#
    )));
}

#[test]
fn treats_openai_prose_an_error_occurred_while_processing_message_as_retryable() {
    assert!(should_retry_error(&message(
        "An error occurred while processing your request. Please try again later."
    )));
}

#[test]
fn treats_upstream_request_failed_provider_error_as_retryable() {
    assert!(should_retry_error(&message(
        "Error from provider (Console Go): Upstream request failed"
    )));
}
