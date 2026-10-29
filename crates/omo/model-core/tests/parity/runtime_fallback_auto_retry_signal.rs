use model_core::RuntimeFallbackAutoRetrySignal;
use model_core::extract_runtime_fallback_auto_retry_signal;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn free_usage_exhaustion_message_returns_the_provider_message() {
    let message = "Free usage exceeded, subscribe to Go";
    let info = json!({ "message": message });

    let signal = extract_runtime_fallback_auto_retry_signal(info.as_object());

    assert_eq!(
        signal,
        Some(RuntimeFallbackAutoRetrySignal {
            signal: message.to_string()
        })
    );
}
