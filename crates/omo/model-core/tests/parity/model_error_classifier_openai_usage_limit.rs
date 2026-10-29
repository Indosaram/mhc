use model_core::ErrorInfo;
use model_core::should_retry_error;

#[test]
fn treats_openai_usage_limit_reached_response_bodies_as_retryable_provider_exhaustion() {
    let error = ErrorInfo {
        name: Some("AI_APICallError".to_string()),
        message: Some(r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#.to_string()),
        status_code: None,
    };

    assert!(should_retry_error(&error));
}
