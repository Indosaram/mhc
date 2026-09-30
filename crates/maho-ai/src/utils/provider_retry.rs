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
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug)]
    struct HttpError {
        status: Option<u16>,
        headers: HeaderMap,
    }

    impl std::fmt::Display for HttpError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "HTTP {:?}", self.status)
        }
    }

    impl ProviderRequestError for HttpError {
        fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
            Some(ProviderErrorStatus { status: self.status, headers: Some(&self.headers) })
        }
    }

    fn err(status: u16, headers: &[(&'static str, &str)]) -> HttpError {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            map.insert(*k, v.parse().expect("header"));
        }
        HttpError { status: Some(status), headers: map }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_retryable_statuses_until_success() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(2), ..Default::default() };
        let result = retry_provider_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n < 2 { Err(err(503, &[])) } else { Ok(n) } }
            },
            &options,
        )
        .await;
        assert_eq!(result.ok(), Some(2));
    }

    #[tokio::test(start_paused = true)]
    async fn honours_should_retry_and_rejects_long_server_delays() {
        let options = ProviderRetryOptions { max_retries: Some(3), max_retry_delay_ms: Some(1000), signal: None };
        let non = retry_provider_request(|| async { Err::<(), _>(err(503, &[("x-should-retry", "false")])) }, &options).await;
        assert!(matches!(non, Err(ProviderRetryError::Request(_))));
        let long = retry_provider_request(|| async { Err::<(), _>(err(429, &[("retry-after", "5")])) }, &options).await;
        match long {
            Err(ProviderRetryError::RetryDelay { message, retry_after_ms }) => {
                assert_eq!(retry_after_ms, 5000);
                assert_eq!(message, "Server requested 5s retry delay (max: 1s). HTTP Some(429) (retry-after-ms: 5000)");
            }
            _ => panic!("expected retry delay error"),
        }
        let bad = retry_provider_request(|| async { Err::<(), _>(err(400, &[])) }, &options).await;
        assert!(matches!(bad, Err(ProviderRetryError::Request(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_retry_replays_the_prefetched_first_chunk() {
        let calls = AtomicU32::new(0);
        let options = ProviderRetryOptions { max_retries: Some(1), ..Default::default() };
        let (stream, meta) = retry_provider_stream_request(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    let items: Vec<Result<u32, HttpError>> = if n == 0 { vec![Err(err(500, &[]))] } else { vec![Ok(1), Ok(2)] };
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
