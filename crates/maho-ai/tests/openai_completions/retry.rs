use maho_ai::api::openai_completions::{OpenAiCompletionsError, OpenAiCompletionsOptions};
use maho_ai::types::StopReason;
use serde_json::json;

use super::harness::*;

fn http_error(status: u16, message: &str, headers: &[(&str, &str)]) -> OpenAiCompletionsError {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            reqwest::header::HeaderValue::from_str(value).expect("header value"),
        );
    }
    OpenAiCompletionsError::http(message, status, map)
}

fn retry_model() -> maho_ai::types::Model {
    model(&[
        ("id", json!("test-model")),
        ("provider", json!("opencode-go")),
        ("baseUrl", json!("https://opencode.ai/zen/go/v1")),
        ("contextWindow", json!(1000)),
        ("maxTokens", json!(100)),
    ])
}

fn retry_context() -> maho_ai::types::Context {
    context(vec![user_blocks(vec![text_block("hi")])], Some(Vec::new()))
}

fn with_retries(api_key: &str, max_retries: u32, max_retry_delay_ms: u64) -> OpenAiCompletionsOptions {
    let mut options = options(api_key);
    options.stream.request.max_retries = Some(max_retries);
    options.stream.request.max_retry_delay_ms = Some(max_retry_delay_ms);
    options
}

#[tokio::test]
async fn disables_sdk_retries_by_default() {
    let transport = ScriptedTransport::new([Err(http_error(503, "server error", &[]))]);

    let result = finish(&run(&retry_model(), &retry_context(), options("test"), transport.clone())).await;

    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(transport.request_count(), 1);
}

#[tokio::test]
async fn honors_provider_retries_while_keeping_sdk_retries_disabled() {
    let transport = ScriptedTransport::new([
        Err(http_error(429, "rate limited", &[("retry-after-ms", "1")])),
        Err(http_error(500, "server error", &[("retry-after-ms", "1")])),
        Ok(ScriptedResponse::chunks([chunk(json!({ "content": "ok" }), Some("stop"))])),
    ]);

    let result = finish(&run(&retry_model(), &retry_context(), with_retries("test", 2, 1000), transport.clone())).await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(transport.request_count(), 3);
}

#[tokio::test]
async fn fails_immediately_when_a_provider_requested_retry_delay_exceeds_the_limit() {
    let transport = ScriptedTransport::new([Err(http_error(429, "rate limited", &[("retry-after", "277403")]))]);

    let result = finish(&run(&retry_model(), &retry_context(), with_retries("test", 2, 1000), transport.clone())).await;

    assert_eq!(result.stop_reason, StopReason::Error);
    let message = result.error_message.unwrap_or_default();
    assert!(message.contains("Server requested 277403s retry delay (max: 1s)"), "{message}");
    assert!(message.contains("rate limited"), "{message}");
    assert_eq!(transport.request_count(), 1);
}

#[tokio::test]
async fn retries_the_observed_digital_ocean_failure_before_the_first_stream_chunk() {
    let transport = ScriptedTransport::new([
        Ok(ScriptedResponse::failing(OpenAiCompletionsError::transport(
            "Upstream error from DigitalOcean: stream failed",
        ))),
        Ok(ScriptedResponse::chunks([chunk(json!({ "content": "recovered" }), Some("stop"))])),
    ]);

    let message = finish(&run(&retry_model(), &retry_context(), with_retries("test", 1, 1000), transport.clone())).await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.content.len(), 1);
    assert_eq!(transport.request_count(), 2);
}

#[tokio::test]
async fn bounds_retries_for_repeated_pre_chunk_stream_failures() {
    let failure = || OpenAiCompletionsError::transport("Upstream error from DigitalOcean: stream failed");
    let transport =
        ScriptedTransport::new([Ok(ScriptedResponse::failing(failure())), Ok(ScriptedResponse::failing(failure()))]);

    let message = finish(&run(&retry_model(), &retry_context(), with_retries("test", 1, 1000), transport.clone())).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message.error_message.unwrap_or_default().contains("Upstream error from DigitalOcean: stream failed"),
        "digitalocean message"
    );
    assert_eq!(transport.request_count(), 2);
}

#[tokio::test]
async fn does_not_retry_non_retryable_pre_chunk_stream_failures() {
    let transport = ScriptedTransport::new([Err(http_error(400, "invalid request", &[]))]);

    let message = finish(&run(&retry_model(), &retry_context(), with_retries("test", 2, 100), transport.clone())).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message.error_message.unwrap_or_default().contains("invalid request"), "invalid request message");
    assert_eq!(transport.request_count(), 1);
}

#[tokio::test]
async fn does_not_retry_after_the_first_stream_chunk() {
    let transport = ScriptedTransport::new([Ok(ScriptedResponse::chunks_then_error(
        [chunk(json!({ "content": "ok" }), None)],
        OpenAiCompletionsError::transport("Upstream error from DigitalOcean: stream failed"),
    ))]);

    let message = finish(&run(&retry_model(), &retry_context(), with_retries("test", 2, 100), transport.clone())).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.content.len(), 1);
    assert_eq!(transport.request_count(), 1);
}
