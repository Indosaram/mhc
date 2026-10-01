//! Port of senpi packages/ai/test/google-shared-retry.test.ts.

use maho_ai::api::google_shared::{retry_google_request, GoogleRequestError};
use maho_ai::utils::provider_retry::{ProviderRetryError, ProviderRetryOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

fn google_api_error(status: u16) -> GoogleRequestError {
    GoogleRequestError::Sdk { message: format!("got status: {status}"), status: Some(status), headers: None }
}

fn options(max_retries: Option<u32>) -> ProviderRetryOptions {
    ProviderRetryOptions { max_retries, max_retry_delay_ms: None, signal: None }
}

fn request_error(error: ProviderRetryError<GoogleRequestError>) -> GoogleRequestError {
    match error {
        ProviderRetryError::Request(error) => error,
        other => panic!("expected a request error, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn retries_a_headers_less_sdk_error_with_a_retryable_status() {
    let calls = Arc::new(AtomicU32::new(0));
    let result = retry_google_request(
        || {
            let calls = Arc::clone(&calls);
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 { Err(google_api_error(429)) } else { Ok("ok") }
            }
        },
        &options(Some(1)),
    )
    .await;

    assert_eq!(result.expect("the retry succeeds"), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn does_not_retry_when_max_retries_is_unset() {
    let calls = Arc::new(AtomicU32::new(0));
    let result = retry_google_request(
        || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<&str, _>(google_api_error(429))
            }
        },
        &options(None),
    )
    .await;

    let error = request_error(result.expect_err("the request fails"));
    assert!(matches!(error, GoogleRequestError::Sdk { status: Some(429), .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_retry_a_non_retryable_status() {
    let calls = Arc::new(AtomicU32::new(0));
    let result = retry_google_request(
        || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<&str, _>(google_api_error(400))
            }
        },
        &options(Some(2)),
    )
    .await;

    let error = request_error(result.expect_err("the request fails"));
    assert!(matches!(error, GoogleRequestError::Sdk { status: Some(400), .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
