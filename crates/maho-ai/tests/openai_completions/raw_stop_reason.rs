use maho_ai::types::StopReason;
use serde_json::json;

use super::harness::*;

#[tokio::test]
async fn preserves_raw_finish_reasons_for_successful_stops() {
    let transport = ScriptedTransport::success([chunk(json!({}), Some("stop"))]);

    let message =
        finish(&run(&model(&[]), &context(vec![user_message("hello")], None), options("test"), transport)).await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(message.error_message, None);
}

#[tokio::test]
async fn preserves_raw_finish_reasons_for_provider_error_stops() {
    let transport = ScriptedTransport::success([chunk(json!({}), Some("content_filter"))]);

    let message =
        finish(&run(&model(&[]), &context(vec![user_message("hello")], None), options("test"), transport)).await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("content_filter"));
    assert_eq!(message.error_message.as_deref(), Some("Provider finish_reason: content_filter"));
}
