use model_core::{ErrorInfo,should_retry_error,RuntimeFallbackErrorType,classify_runtime_fallback_error,is_runtime_fallback_retryable_error,RuntimeFallbackRetryOptions};
use serde_json::json;

#[test]
fn fair_use_rate_restriction_advances_both_classifiers(){
    let message="Request blocked under Fair Use Policy. Your request rate has been restricted.";
    let error=ErrorInfo{name:None,message:Some(message.into()),status_code:Some(429)};
    let value=json!({"message":message,"statusCode":429});
    assert!(should_retry_error(&error));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn permanent_terminal_quota_stops_both_classifiers(){
    for message in ["Terminal quota exhausted for this account. Upgrade the plan to continue.","Hard billing limit reached for this organization."]{
        let error=ErrorInfo{name:None,message:Some(message.into()),status_code:None};
        let value=json!({"message":message});
        assert!(!should_retry_error(&error));
        assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::Abort));
        assert!(!is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
    }
}

#[test]
fn context_limit_and_rejected_credential_are_not_usage_exhaustion(){
    for (message,status_code) in [("prompt is too long: context limit reached for this model",None),("Unauthorized",Some(401))]{
        let error=ErrorInfo{name:None,message:Some(message.into()),status_code};
        let value=json!({"message":message,"statusCode":status_code});
        assert!(!should_retry_error(&error));
        assert_eq!(classify_runtime_fallback_error(Some(&value)),None);
    }
}

#[test]
fn temporary_opencode_go_monthly_limit_8296(){
    let value=json!({"statusCode":429,"message":"Monthly usage limit reached. It will reset in 2 days 8 hours"});
    let error=ErrorInfo{name:None,message:Some("Monthly usage limit reached. It will reset in 2 days 8 hours".into()),status_code:Some(429)};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_z_ai_weekly_monthly_exhaustion_7161(){
    let value=json!({"name":"AI_APICallError","message":"Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-09-30 00:00:00"});
    let error=ErrorInfo{name:Some("AI_APICallError".into()),message:Some("Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-09-30 00:00:00".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_anthropic_subscription_session_limit(){
    let value=json!({"message":"You've hit your session limit · resets 3pm (Asia/Seoul)"});
    let error=ErrorInfo{name:None,message:Some("You've hit your session limit · resets 3pm (Asia/Seoul)".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_anthropic_subscription_weekly_limit(){
    let value=json!({"message":"You've hit your weekly limit · resets 5am (Asia/Seoul)"});
    let error=ErrorInfo{name:None,message:Some("You've hit your weekly limit · resets 5am (Asia/Seoul)".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_anthropic_subscription_5_hour_limit(){
    let value=json!({"message":"5-hour limit reached ∙ resets 3pm"});
    let error=ErrorInfo{name:None,message:Some("5-hour limit reached ∙ resets 3pm".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_anthropic_subscription_model_weekly_limit(){
    let value=json!({"message":"You've hit your Fable weekly limit · resets Oct 2, 9am"});
    let error=ErrorInfo{name:None,message:Some("You've hit your Fable weekly limit · resets Oct 2, 9am".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_claude_usage_limit_marker(){
    let value=json!({"message":"Claude AI usage limit reached|1759075200"});
    let error=ErrorInfo{name:None,message:Some("Claude AI usage limit reached|1759075200".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_codex_usage_limit_reached_body(){
    let value=json!({"name":"AI_APICallError","message":"{\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\"}}"});
    let error=ErrorInfo{name:Some("AI_APICallError".into()),message:Some("{\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\"}}".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_copilot_quota_429(){
    let value=json!({"statusCode":429,"message":"GitHub Copilot quota exceeded (HTTP 429): the plan's included usage or its additional-usage limit is used up, so premium models are refused until it resets or the limit is raised."});
    let error=ErrorInfo{name:None,message:Some("GitHub Copilot quota exceeded (HTTP 429): the plan's included usage or its additional-usage limit is used up, so premium models are refused until it resets or the limit is raised.".into()),status_code:Some(429)};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_glm_daily_call_limit(){
    let value=json!({"statusCode":429,"message":"Daily call limit for this API key has been reached. Limit will reset at midnight UTC."});
    let error=ErrorInfo{name:None,message:Some("Daily call limit for this API key has been reached. Limit will reset at midnight UTC.".into()),status_code:Some(429)};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_glm_account_in_arrears(){
    let value=json!({"statusCode":429,"message":"Your account is in arrears, please recharge and try again."});
    let error=ErrorInfo{name:None,message:Some("Your account is in arrears, please recharge and try again.".into()),status_code:Some(429)};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_quota_reset_window(){
    let value=json!({"message":"quota will reset after 1 hour"});
    let error=ErrorInfo{name:None,message:Some("quota will reset after 1 hour".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_billing_period_quota(){
    let value=json!({"message":"quota exceeded for this billing period"});
    let error=ErrorInfo{name:None,message:Some("quota exceeded for this billing period".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_insufficient_credits(){
    let value=json!({"message":"insufficient credits to complete this request"});
    let error=ErrorInfo{name:None,message:Some("insufficient credits to complete this request".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_subscription_quota(){
    let value=json!({"message":"Subscription quota exceeded. You can continue using free models."});
    let error=ErrorInfo{name:None,message:Some("Subscription quota exceeded. You can continue using free models.".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_quotaexceedederror_name(){
    let value=json!({"name":"QuotaExceededError"});
    let error=ErrorInfo{name:Some("QuotaExceededError".into()),message:None,status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_insufficientcreditserror_name(){
    let value=json!({"name":"InsufficientCreditsError"});
    let error=ErrorInfo{name:Some("InsufficientCreditsError".into()),message:None,status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_freeusagelimiterror_name(){
    let value=json!({"name":"FreeUsageLimitError"});
    let error=ErrorInfo{name:Some("FreeUsageLimitError".into()),message:None,status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_zhipu_5_hour_cap(){
    let value=json!({"message":"已达到 5 小时的使用上限"});
    let error=ErrorInfo{name:None,message:Some("已达到 5 小时的使用上限".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_localized_insufficient_quota(){
    let value=json!({"message":"额度不足"});
    let error=ErrorInfo{name:None,message:Some("额度不足".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_localized_insufficient_balance(){
    let value=json!({"message":"账户余额不足"});
    let error=ErrorInfo{name:None,message:Some("账户余额不足".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}

#[test]
fn temporary_localized_free_quota_exhausted(){
    let value=json!({"message":"免费额度已耗尽"});
    let error=ErrorInfo{name:None,message:Some("免费额度已耗尽".into()),status_code:None};
    assert!(should_retry_error(&error));
    assert_eq!(classify_runtime_fallback_error(Some(&value)),Some(RuntimeFallbackErrorType::QuotaExceeded));
    assert!(is_runtime_fallback_retryable_error(Some(&value),&[429,500,502,503,504],&RuntimeFallbackRetryOptions::default()));
}
