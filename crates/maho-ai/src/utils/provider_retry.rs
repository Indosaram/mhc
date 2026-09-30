//! Port of senpi packages/ai/src/utils/provider-retry.ts.

use crate::utils::abort::AbortSignal;
use crate::utils::retry::math_random;
use crate::utils::retry_hint::{RetryHintInput, append_retry_after_ms_marker, extract_429_retry_after_ms};
use futures::stream::{BoxStream, Stream, StreamExt};
use reqwest::header::HeaderMap;
use std::future::Future;
use std::time::Duration;

const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
pub const DIGITALOCEAN_STREAM_FAILURE_MESSAGE: &str = "Upstream error from DigitalOcean: stream failed";

/// The TS `isProviderError` duck type: an error carrying an HTTP status and headers.
pub trait ProviderRequestError: std::fmt::Display {
    /// `None` when the error is not a provider HTTP error at all.
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>>;
}

pub struct ProviderErrorStatus<'a> {
    pub status: Option<u16>,
    pub headers: Option<&'a HeaderMap>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRetryError<E> {
    #[error("{0}")]
    Request(E),
    /// Server asked for a delay beyond the configured ceiling.
    #[error("{message}")]
    RetryDelay { message: String, retry_after_ms: u64 },
    #[error("Request aborted")]
    Aborted,
}

#[derive(Clone, Default)]
pub struct ProviderRetryOptions {
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
}

fn classify<E: ProviderRequestError>(error: &E) -> Option<(Option<u16>, Option<&HeaderMap>)> {
    if error.to_string() == DIGITALOCEAN_STREAM_FAILURE_MESSAGE {
        return Some(error.provider_status().map_or((None, None), |s| (s.status, s.headers)));
    }
    error.provider_status().map(|s| (s.status, s.headers))
}

fn is_retryable(status: Option<u16>, headers: Option<&HeaderMap>) -> bool {
    match headers.and_then(|h| h.get("x-should-retry")).and_then(|v| v.to_str().ok()) {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    match status {
        None => true,
        Some(status) => matches!(status, 408 | 409 | 429) || status >= 500,
    }
}

fn retry_delay_ms<E>(
    headers: Option<&HeaderMap>,
    retry_index: u32,
    max_retry_delay_ms: Option<u64>,
    provider_message: &str,
) -> Result<u64, ProviderRetryError<E>> {
    let hint = extract_429_retry_after_ms(&RetryHintInput { status: Some(429), headers, body_text: "" }, None);
    if let Some(delay_ms) = hint {
        let max_delay = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
        if max_delay > 0 && delay_ms > max_delay {
            let message = format!(
                "Server requested {}s retry delay (max: {}s). {provider_message}",
                delay_ms.div_ceil(1000),
                max_delay.div_ceil(1000)
            );
            return Err(ProviderRetryError::RetryDelay { message: append_retry_after_ms_marker(&message, delay_ms), retry_after_ms: delay_ms });
        }
        return Ok(delay_ms);
    }
    let exponential = (0.5 * 2f64.powi(retry_index as i32)).min(8.0) * 1000.0;
    Ok((exponential * (1.0 - math_random() * 0.25)) as u64)
}

async fn abortable_sleep<E>(ms: u64, signal: Option<&AbortSignal>) -> Result<(), ProviderRetryError<E>> {
    let Some(signal) = signal else {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        return Ok(());
    };
    if signal.aborted() {
        return Err(ProviderRetryError::Aborted);
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(ProviderRetryError::Aborted),
        () = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

/// Retries transient provider HTTP failures (408/409/429/5xx, `x-should-retry`) with server hints.
pub async fn retry_provider_request<T, E, F, Fut>(mut request: F, options: &ProviderRetryOptions) -> Result<T, ProviderRetryError<E>>
where
    E: ProviderRequestError,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut remaining = max_retries;
    loop {
        let error = match request().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(ProviderRetryError::Aborted);
        }
        let Some((_, headers)) = classify(&error).filter(|(s, h)| remaining > 0 && is_retryable(*s, *h)) else {
            return Err(ProviderRetryError::Request(error));
        };
        let retry_index = max_retries - remaining;
        remaining -= 1;
        let delay = retry_delay_ms(headers, retry_index, options.max_retry_delay_ms, &error.to_string())?;
        abortable_sleep(delay, options.signal.as_ref()).await?;
    }
}

pub struct ProviderStreamAttempt<TChunk, TMetadata> {
    pub stream: BoxStream<'static, Result<TChunk, TMetadata>>,
    pub metadata: TMetadata,
}

/// Retries until the stream yields its first item, then replays that item ahead of the rest.
/// Chunks are `Result<TChunk, E>` so a failure before the first chunk is retried.
pub async fn retry_provider_stream_request<TChunk, TMeta, E, F, Fut, S>(
    mut request: F,
    options: &ProviderRetryOptions,
) -> Result<(BoxStream<'static, Result<TChunk, E>>, TMeta), ProviderRetryError<E>>
where
    E: ProviderRequestError + Send + 'static,
    TChunk: Send + 'static,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(S, TMeta), E>>,
    S: Stream<Item = Result<TChunk, E>> + Send + Unpin + 'static,
{
    retry_provider_request(
        || {
            let attempt = request();
            async move {
                let (mut stream, metadata) = attempt.await?;
                let first = match stream.next().await {
                    Some(Ok(chunk)) => Some(chunk),
                    Some(Err(error)) => return Err(error),
                    None => None,
                };
                let replay: BoxStream<'static, Result<TChunk, E>> = match first {
                    Some(chunk) => futures::stream::once(async move { Ok(chunk) }).chain(stream).boxed(),
                    None => futures::stream::empty().boxed(),
                };
                Ok((replay, metadata))
            }
        },
        options,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug)]
    struct HttpError {
        status: Option<u16>,
        headers: Option<HeaderMap>,
        message: String,
    }

    impl std::fmt::Display for HttpError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl ProviderRequestError for HttpError {
        fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
            Some(ProviderErrorStatus { status: self.status, headers: self.headers.as_ref() })
        }
    }

    /// `providerError(status, headers)` from provider-retry.test.ts.
    fn err(status: Option<u16>, headers: &[(&'static str, &str)]) -> HttpError {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            map.insert(*k, v.parse().expect("header"));
        }
        HttpError { status, headers: Some(map), message: format!("Provider error: {}", status.map_or("undefined".to_owned(), |s| s.to_string())) }
    }

    /// "retries retryable provider errors"
    #[tokio::test(start_paused = true)]
    async fn retries_retryable_provider_errors() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 { Err(err(Some(429), &[("retry-after-ms", "1000")])) } else { Ok("ok") }
                }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "does not retry errors the provider marks as non-retryable"
    #[tokio::test]
    async fn does_not_retry_errors_the_provider_marks_as_non_retryable() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(2), ..Default::default() };
        let result = retry_provider_request::<(), _, _, _>(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(err(Some(429), &[("x-should-retry", "false")])) }
            },
            &options,
        )
        .await;
        // `rejects.toBe(error)`: the original provider error propagates unchanged.
        match result {
            Err(ProviderRetryError::Request(error)) => assert_eq!(error.to_string(), "Provider error: 429"),
            other => panic!("expected the original provider error to propagate, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// "rejects a provider-requested retry delay above the limit"
    #[tokio::test]
    async fn rejects_a_provider_requested_retry_delay_above_the_limit() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), max_retry_delay_ms: Some(1000), signal: None };
        let result = retry_provider_request::<(), _, _, _>(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(err(Some(429), &[("retry-after", "277403")])) }
            },
            &options,
        )
        .await;
        match result {
            Err(ProviderRetryError::RetryDelay { message, .. }) => {
                assert!(message.starts_with("Server requested 277403s retry delay (max: 1s)"), "{message}");
            }
            other => panic!("expected retry delay error, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// "allows disabling the provider-requested retry delay cap"
    #[tokio::test(start_paused = true)]
    async fn allows_disabling_the_provider_requested_retry_delay_cap() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), max_retry_delay_ms: Some(0), signal: None };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(429), &[("retry-after", "2")])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "aborts a provider-requested retry delay"
    #[tokio::test(start_paused = true)]
    async fn aborts_a_provider_requested_retry_delay() {
        use crate::utils::abort::AbortController;
        let controller = AbortController::new();
        let calls = AtomicU32::new(0);
        let options =
            ProviderRetryOptions { max_retries: Some(2), max_retry_delay_ms: Some(0), signal: Some(controller.signal()) };
        let result_future = retry_provider_request::<(), _, _, _>(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(err(Some(429), &[("retry-after", "277403")])) }
            },
            &options,
        );
        tokio::pin!(result_future);
        // Drive the pinned future to its first await point (the abortable sleep) without
        // advancing virtual time, mirroring `vi.advanceTimersByTimeAsync(0)` letting the
        // first synchronous portion of the retry loop run.
        let _ = futures::poll!(result_future.as_mut());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        controller.abort(None);
        let result = result_future.await;
        assert!(matches!(result, Err(ProviderRetryError::Aborted)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Wraps a stream so its drop is observable. Breaking out of the TS test's
    /// `for await (...)` calls the provider iterator's `return()`; in Rust that is the
    /// source stream being dropped instead of drained.
    struct DropGuardedStream<S> {
        inner: S,
        drops: Arc<AtomicU32>,
    }

    impl<S> Drop for DropGuardedStream<S> {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl<S: Stream + Unpin> Stream for DropGuardedStream<S> {
        type Item = S::Item;

        fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_next(cx)
        }
    }

    /// "forwards early consumer cancellation to the provider stream"
    #[tokio::test(start_paused = true)]
    async fn forwards_early_consumer_cancellation_to_the_provider_stream() {
        let drops = Arc::new(AtomicU32::new(0));
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let (stream, meta) = retry_provider_stream_request(
            || {
                let drops = Arc::clone(&drops);
                async move {
                    let inner = futures::stream::iter([Ok("first"), Ok("second")]);
                    Ok::<_, HttpError>((DropGuardedStream { inner, drops }.boxed(), "response"))
                }
            },
            &options,
        )
        .await
        .expect("stream");
        let mut items = Vec::new();
        {
            let mut iter = std::pin::pin!(stream);
            if let Some(chunk) = iter.next().await {
                items.push(chunk.expect("chunk"));
            }
        }
        assert_eq!((items, meta), (vec!["first"], "response"));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    // -----------------------------------------------------------------------
    // Structured hint propagation (todo 4)
    // -----------------------------------------------------------------------

    /// "stamps retryAfterMs and canonical marker on over-cap 429 with retry-after header"
    #[tokio::test]
    async fn stamps_retry_after_ms_and_canonical_marker_on_over_cap_429_with_retry_after_header() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request::<(), _, _, _>(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(err(Some(429), &[("retry-after", "1258")])) }
            },
            &options,
        )
        .await;
        match result {
            Err(ProviderRetryError::RetryDelay { message, retry_after_ms }) => {
                assert_eq!(retry_after_ms, 1_258_000);
                assert!(message.contains("(retry-after-ms: 1258000)"), "{message}");
            }
            other => panic!("expected retry delay error, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// "honors retry-after: 2 header delay under cap with fake timers"
    #[tokio::test(start_paused = true)]
    async fn honors_retry_after_2_header_delay_under_cap() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(429), &[("retry-after", "2")])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "honors retry-after-ms header on non-429 (500) errors"
    #[tokio::test(start_paused = true)]
    async fn honors_retry_after_ms_header_on_non_429_500_errors() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(500), &[("retry-after-ms", "100")])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "uses exponential path for non-429 (500) errors — characterization pin"
    #[tokio::test(start_paused = true)]
    async fn uses_exponential_path_for_non_429_500_errors() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(500), &[])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "preserves exponential fallback for 429 without any hint"
    #[tokio::test(start_paused = true)]
    async fn preserves_exponential_fallback_for_429_without_any_hint() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(429), &[])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "honors x-should-retry: true header (characterization pin)"
    #[tokio::test(start_paused = true)]
    async fn honors_x_should_retry_true_header() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(400), &[("x-should-retry", "true")])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "handles malformed retry-after headers gracefully (falls through to exponential)"
    #[tokio::test(start_paused = true)]
    async fn handles_malformed_retry_after_headers_gracefully() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n == 0 { Err(err(Some(429), &[("retry-after", "garbage-later")])) } else { Ok("ok") } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "preserves DigitalOcean stream failure special-case"
    #[tokio::test(start_paused = true)]
    async fn preserves_digitalocean_stream_failure_special_case() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Err(HttpError { status: None, headers: None, message: DIGITALOCEAN_STREAM_FAILURE_MESSAGE.to_owned() })
                    } else {
                        Ok("ok")
                    }
                }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// "stamps retryAfterMs when retry-after-ms header exceeds cap"
    #[tokio::test]
    async fn stamps_retry_after_ms_when_retry_after_ms_header_exceeds_cap() {
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let result = retry_provider_request::<(), _, _, _>(
            || async { Err(err(Some(429), &[("retry-after-ms", "90000")])) },
            &options,
        )
        .await;
        match result {
            Err(ProviderRetryError::RetryDelay { message, retry_after_ms }) => {
                assert_eq!(retry_after_ms, 90_000);
                assert!(message.contains("(retry-after-ms: 90000)"), "{message}");
            }
            other => panic!("expected retry delay error, got {other:?}"),
        }
    }

    /// pre-existing regression coverage kept from the initial port: exercises the exact
    /// `HTTP {status:?}` Display shape and the stream-retry replay path together.
    #[tokio::test(start_paused = true)]
    async fn stream_retry_replays_the_prefetched_first_chunk() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let (stream, meta) = retry_provider_stream_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    let items: Vec<Result<u32, HttpError>> = if n == 0 { vec![Err(err(Some(500), &[]))] } else { vec![Ok(1), Ok(2)] };
                    Ok((futures::stream::iter(items), n))
                }
            },
            &options,
        )
        .await
        .expect("stream");
        let items: Vec<u32> = stream.map(|r| r.expect("chunk")).collect().await;
        assert_eq!((items, meta), (vec![1, 2], 1));
    }
}
