//! Port of senpi packages/ai/src/api/cursor-agent/stream-retry.ts.
// ported by todo 12

use crate::utils::abort::AbortSignal;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStreamRetryCause {
    Stall,
    Transport,
    CleanEnd,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CursorRetryableStreamError {
    message: String,
    pub retry_cause: CursorStreamRetryCause,
}

impl CursorRetryableStreamError {
    pub fn new(message: impl Into<String>, retry_cause: CursorStreamRetryCause) -> Self {
        Self { message: message.into(), retry_cause }
    }
}

pub fn is_cursor_retryable_stream_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<CursorRetryableStreamError>().is_some()
}

#[derive(Debug, Clone, Default)]
pub struct CursorStreamRetryDelayOptions {
    pub attempt: u32,
    pub base_delay_ms: Option<u64>,
    pub fixed_delay_ms: Option<u64>,
}

pub fn cursor_stream_retry_delay_ms(
    options: CursorStreamRetryDelayOptions,
    random: impl FnOnce() -> f64,
) -> u64 {
    if let Some(fixed_delay_ms) = options.fixed_delay_ms {
        return fixed_delay_ms;
    }
    let base_delay_ms = options.base_delay_ms.unwrap_or(1000);
    let backoff_ms = base_delay_ms.saturating_mul(1u64 << options.attempt.min(63)).min(60_000);
    let jitter = (backoff_ms as f64 * 0.2 * random()).floor() as u64;
    backoff_ms + jitter
}

pub struct ShouldRetryCursorStreamOptions<'a> {
    pub error: Option<&'a (dyn std::error::Error + 'static)>,
    pub retries: u32,
    pub max_retries: u32,
    pub saw_turn_ended: bool,
    pub aborted: bool,
}

pub fn should_retry_cursor_stream(options: ShouldRetryCursorStreamOptions<'_>) -> bool {
    !options.saw_turn_ended
        && !options.aborted
        && options.retries < options.max_retries
        && options.error.is_some_and(is_cursor_retryable_stream_error)
}

pub async fn wait_for_cursor_stream_retry(delay_ms: u64, signal: Option<&AbortSignal>) {
    if delay_ms == 0 || signal.is_some_and(AbortSignal::aborted) {
        return;
    }
    let sleep = tokio::time::sleep(Duration::from_millis(delay_ms));
    match signal {
        Some(signal) => {
            tokio::select! {
                () = sleep => {}
                () = signal.cancelled() => {}
            }
        }
        None => sleep.await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_fixed_delay_when_computing_retry_delay_then_ignores_backoff() {
        let ms = cursor_stream_retry_delay_ms(
            CursorStreamRetryDelayOptions { attempt: 5, base_delay_ms: Some(1000), fixed_delay_ms: Some(42) },
            || 0.5,
        );
        assert_eq!(ms, 42);
    }

    #[test]
    fn given_attempt_zero_when_computing_retry_delay_then_uses_base_plus_jitter() {
        let ms = cursor_stream_retry_delay_ms(
            CursorStreamRetryDelayOptions { attempt: 0, base_delay_ms: Some(1000), fixed_delay_ms: None },
            || 0.0,
        );
        assert_eq!(ms, 1000);
    }

    #[test]
    fn given_large_attempt_when_computing_retry_delay_then_caps_backoff_at_60s() {
        let ms = cursor_stream_retry_delay_ms(
            CursorStreamRetryDelayOptions { attempt: 30, base_delay_ms: Some(1000), fixed_delay_ms: None },
            || 0.0,
        );
        assert_eq!(ms, 60_000);
    }

    #[test]
    fn given_max_jitter_when_computing_retry_delay_then_adds_twenty_percent() {
        let ms = cursor_stream_retry_delay_ms(
            CursorStreamRetryDelayOptions { attempt: 0, base_delay_ms: Some(1000), fixed_delay_ms: None },
            || 1.0,
        );
        assert_eq!(ms, 1200);
    }

    #[test]
    fn given_retryable_error_under_max_retries_when_checked_then_should_retry() {
        let error = CursorRetryableStreamError::new("stalled", CursorStreamRetryCause::Stall);
        let should = should_retry_cursor_stream(ShouldRetryCursorStreamOptions {
            error: Some(&error),
            retries: 1,
            max_retries: 10,
            saw_turn_ended: false,
            aborted: false,
        });
        assert!(should);
    }

    #[test]
    fn given_turn_already_ended_when_checked_then_never_retries() {
        let error = CursorRetryableStreamError::new("stalled", CursorStreamRetryCause::Stall);
        let should = should_retry_cursor_stream(ShouldRetryCursorStreamOptions {
            error: Some(&error),
            retries: 0,
            max_retries: 10,
            saw_turn_ended: true,
            aborted: false,
        });
        assert!(!should);
    }

    #[test]
    fn given_aborted_when_checked_then_never_retries() {
        let error = CursorRetryableStreamError::new("stalled", CursorStreamRetryCause::Stall);
        let should = should_retry_cursor_stream(ShouldRetryCursorStreamOptions {
            error: Some(&error),
            retries: 0,
            max_retries: 10,
            saw_turn_ended: false,
            aborted: true,
        });
        assert!(!should);
    }

    #[test]
    fn given_retries_at_max_when_checked_then_stops_retrying() {
        let error = CursorRetryableStreamError::new("stalled", CursorStreamRetryCause::Stall);
        let should = should_retry_cursor_stream(ShouldRetryCursorStreamOptions {
            error: Some(&error),
            retries: 10,
            max_retries: 10,
            saw_turn_ended: false,
            aborted: false,
        });
        assert!(!should);
    }

    #[test]
    fn given_non_retryable_error_when_checked_then_does_not_retry() {
        let error = std::io::Error::other("boom");
        let should = should_retry_cursor_stream(ShouldRetryCursorStreamOptions {
            error: Some(&error),
            retries: 0,
            max_retries: 10,
            saw_turn_ended: false,
            aborted: false,
        });
        assert!(!should);
    }

    #[test]
    fn given_no_error_when_checked_then_does_not_retry() {
        let should = should_retry_cursor_stream(ShouldRetryCursorStreamOptions {
            error: None,
            retries: 0,
            max_retries: 10,
            saw_turn_ended: false,
            aborted: false,
        });
        assert!(!should);
    }

    #[tokio::test]
    async fn given_zero_delay_when_waiting_then_returns_immediately() {
        let start = std::time::Instant::now();
        wait_for_cursor_stream_retry(0, None).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn given_already_aborted_signal_when_waiting_then_returns_immediately() {
        let controller = crate::utils::abort::AbortController::new();
        controller.abort(None);
        let signal = controller.signal();
        let start = std::time::Instant::now();
        wait_for_cursor_stream_retry(5_000, Some(&signal)).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn given_signal_aborted_mid_wait_then_wait_resolves_early() {
        let controller = crate::utils::abort::AbortController::new();
        let signal = controller.signal();
        let wait = tokio::spawn(async move {
            wait_for_cursor_stream_retry(5_000, Some(&signal)).await;
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        controller.abort(None);
        tokio::time::timeout(Duration::from_millis(500), wait)
            .await
            .expect("wait should resolve promptly after abort")
            .expect("wait task should not panic");
    }
}
