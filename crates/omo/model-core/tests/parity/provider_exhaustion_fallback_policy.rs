use model_core::ErrorInfo;
use model_core::RuntimeFallbackErrorType;
use model_core::classify_provider_exhaustion_fallback_signal;
use model_core::is_provider_exhaustion_fallback_eligible;
use model_core::should_retry_error;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn error_info(error: &Value) -> ErrorInfo {
    ErrorInfo {
        name: error
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        status_code: None,
    }
}

#[test]
fn quota_subscription_and_billing_exhaustion_are_eligible_without_weakening_legacy_stop_semantics()
{
    let errors = [
        json!({ "name": "QuotaExceededError", "message": "Quota exceeded for this billing period." }),
        json!({ "message": "Subscription limit exceeded. You can continue using free models." }),
        json!({ "name": "BillingError", "message": "Billing hard limit reached for this account." }),
        json!({ "message": "Payment required: out of credits." }),
        json!({ "message": "Credit balance too low for this request." }),
    ];

    let provider_exhaustion_results: Vec<_> = errors
        .iter()
        .map(|error| {
            (
                classify_provider_exhaustion_fallback_signal(Some(error)),
                is_provider_exhaustion_fallback_eligible(Some(error)),
            )
        })
        .collect();
    let legacy_retry_results: Vec<bool> = errors
        .iter()
        .map(|error| should_retry_error(&error_info(error)))
        .collect();

    assert_eq!(
        provider_exhaustion_results,
        vec![(Some(RuntimeFallbackErrorType::QuotaExceeded), true); errors.len()]
    );
    assert_eq!(legacy_retry_results, vec![false; errors.len()]);
}

#[test]
fn hard_stop_runtime_errors_stay_ineligible() {
    let hard_stop_errors = [
        json!({ "name": "MessageAbortedError", "message": "The user aborted this request." }),
        json!({
            "name": "AI_LoadAPIKeyError",
            "message": "API key is missing from the OPENAI_API_KEY environment variable.",
        }),
        json!({ "message": "API key must be a string." }),
        json!({ "name": "ValidationError", "message": "Invalid request payload." }),
    ];

    let results: Vec<_> = hard_stop_errors
        .iter()
        .map(|error| {
            (
                classify_provider_exhaustion_fallback_signal(Some(error)),
                is_provider_exhaustion_fallback_eligible(Some(error)),
            )
        })
        .collect();

    assert_eq!(results, vec![(None, false); hard_stop_errors.len()]);
}
