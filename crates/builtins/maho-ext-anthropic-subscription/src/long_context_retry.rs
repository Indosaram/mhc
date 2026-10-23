//! Standard-window retry for a refused legacy `[1m]` request (senpi `long-context-retry.ts`).
use crate::errors::{SdkErrorClassification, SdkErrorKind, classify_sdk_error};
use serde_json::Value;

/// The failure an SDK message carries, as the classifier input.
pub fn sdk_failure(message: &Value) -> Option<Value> {
    if let Some(refusal) = crate::refusal::refusal_error(message) {
        return Some(Value::String(refusal.to_string()));
    }
    match message["type"].as_str() {
        Some("assistant") => crate::errors::sdk_assistant_failure(message).map(Value::String),
        Some("result") => {
            crate::errors::sdk_result_failure(message).map(|failure| Value::String(failure.message))
        }
        _ => None,
    }
}

pub fn visible_sdk_message(message: &Value) -> bool {
    message["type"] == "stream_event"
        && matches!(
            message["event"]["type"].as_str(),
            Some("content_block_start" | "content_block_delta" | "content_block_stop")
        )
}

/// Only a request that asked for the legacy `[1m]` window can have that window refused. The same words on
/// any other request are an ordinary failure: never a recorded refusal, a 200k retry, or an account block (senpi#3801).
pub fn classify_for_request(
    error: &Value,
    asked_legacy_long_context: bool,
) -> SdkErrorClassification {
    let classification = classify_sdk_error(error);
    if classification.kind == SdkErrorKind::LongContextUnavailable && !asked_legacy_long_context {
        SdkErrorClassification {
            kind: SdkErrorKind::Other,
            retryable: false,
        }
    } else {
        classification
    }
}

/// One attempt's SDK message stream.
#[async_trait::async_trait]
pub trait AttemptStream: Send {
    async fn next(&mut self) -> Option<anyhow::Result<Value>>;
    async fn close(&mut self) -> anyhow::Result<()>;
}

/// The ambient lane has no account failover, so a refused 1M window is retried here: once, and only before
/// anything visible streamed. Every message passes to `deliver` as it arrives; the refused attempt is closed
/// before the retry starts, after the refusal is recorded so the rebuilt options ask for the standard window.
pub async fn retry_at_standard_window_once<S, Fut>(
    mut attempt: impl FnMut() -> Fut,
    asked_legacy_long_context: impl Fn() -> bool,
    mut deliver: impl FnMut(Value),
) -> anyhow::Result<()>
where
    S: AttemptStream,
    Fut: std::future::Future<Output = anyhow::Result<S>>,
{
    let mut first = attempt().await?;
    let mut visible = false;
    let mut refused = false;
    let outcome: anyhow::Result<()> = async {
        loop {
            let Some(next) = first.next().await else {
                return Ok(());
            };
            let message = next?;
            let failure = if visible { None } else { sdk_failure(&message) };
            if let Some(failure) = failure
                && classify_for_request(&failure, asked_legacy_long_context()).kind
                    == SdkErrorKind::LongContextUnavailable
            {
                refused = true;
                return Ok(());
            }
            visible |= visible_sdk_message(&message);
            deliver(message);
        }
    }
    .await;
    // Closing the first query must never replace the error (or the refusal) that ended it.
    let _ = first.close().await;
    outcome?;
    if !refused {
        return Ok(());
    }
    crate::long_context::mark_long_context_refused();
    let mut second = attempt().await?;
    let result: anyhow::Result<()> = async {
        while let Some(next) = second.next().await {
            deliver(next?);
        }
        Ok(())
    }
    .await;
    let _ = second.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixed {
        messages: std::collections::VecDeque<Value>,
        closed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl AttemptStream for Fixed {
        async fn next(&mut self) -> Option<anyhow::Result<Value>> {
            self.messages.pop_front().map(Ok)
        }
        async fn close(&mut self) -> anyhow::Result<()> {
            self.closed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }
    fn refusal() -> Value {
        json!({"type":"result","subtype":"success","is_error":true,"result":"Extra usage is required for long context"})
    }
    #[test]
    fn non_legacy_requests_read_the_refusal_as_ordinary() {
        let error = json!("Extra usage is required for long context");
        assert_eq!(
            classify_for_request(&error, true).kind,
            SdkErrorKind::LongContextUnavailable
        );
        assert_eq!(
            classify_for_request(&error, false),
            SdkErrorClassification {
                kind: SdkErrorKind::Other,
                retryable: false
            }
        );
    }
    #[test]
    fn only_content_stream_events_are_visible() {
        assert!(visible_sdk_message(
            &json!({"type":"stream_event","event":{"type":"content_block_delta"}})
        ));
        assert!(!visible_sdk_message(
            &json!({"type":"stream_event","event":{"type":"message_start"}})
        ));
        assert!(!visible_sdk_message(&json!({"type":"system"})));
    }
    #[tokio::test]
    async fn legacy_refusal_closes_the_first_stream_and_retries_once() {
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut attempts = vec![
            vec![json!({"type":"system"}), refusal()],
            vec![json!({"type":"result","subtype":"success","result":"ok"})],
        ]
        .into_iter();
        let mut delivered = Vec::new();
        let counter = closed.clone();
        retry_at_standard_window_once(
            || {
                let messages = attempts.next().unwrap_or_default();
                let closed = counter.clone();
                async move {
                    Ok(Fixed {
                        messages: messages.into(),
                        closed,
                    })
                }
            },
            || true,
            |message| delivered.push(message),
        )
        .await
        .expect("retry");
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[1]["result"], "ok");
        assert_eq!(closed.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn a_non_legacy_request_is_never_retried() {
        let closed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut calls = 0;
        let mut delivered = Vec::new();
        retry_at_standard_window_once(
            || {
                calls += 1;
                let closed = closed.clone();
                async move {
                    Ok(Fixed {
                        messages: vec![refusal()].into(),
                        closed,
                    })
                }
            },
            || false,
            |message| delivered.push(message),
        )
        .await
        .expect("passes through");
        assert_eq!(calls, 1);
        assert_eq!(delivered.len(), 1);
    }
}
