//! Port of senpi packages/coding-agent/src/core/compaction/stream-watchdog.ts (the error
//! classes, constants and budget math; the stream consumption loop needs the provider event
//! stream and stays with the AgentSession wiring, todo 21).

use std::sync::Arc;

use maho_ai::utils::abort::AbortSignal;

/// senpi StreamIdleTimeoutError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamIdleTimeoutError {
    pub idle_timeout_ms: i64,
}

impl StreamIdleTimeoutError {
    pub fn new(idle_timeout_ms: i64) -> Self {
        Self { idle_timeout_ms }
    }
}

impl std::fmt::Display for StreamIdleTimeoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Summarization stream stalled: no provider events for {}ms; treating the request as dead",
            self.idle_timeout_ms
        )
    }
}

impl std::error::Error for StreamIdleTimeoutError {}

/// senpi StreamDurationBudgetError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDurationBudgetError {
    pub max_duration_ms: i64,
}

impl StreamDurationBudgetError {
    pub fn new(max_duration_ms: i64) -> Self {
        Self { max_duration_ms }
    }
}

impl std::fmt::Display for StreamDurationBudgetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Summarization stream exceeded its {}ms wall-clock budget; treating the request as too slow to keep the session waiting",
            self.max_duration_ms
        )
    }
}

impl std::error::Error for StreamDurationBudgetError {}

/// senpi DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS.
pub const DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS: i64 = 300_000;

/// senpi DEFAULT_SUMMARIZATION_MAX_DURATION_MS.
pub const DEFAULT_SUMMARIZATION_MAX_DURATION_MS: i64 = 120_000;

/// senpi SUMMARIZATION_MAX_DURATION_PER_TOKEN_MS.
pub const SUMMARIZATION_MAX_DURATION_PER_TOKEN_MS: f64 = 2.0;

/// senpi SUMMARIZATION_MAX_DURATION_CAP_MS.
pub const SUMMARIZATION_MAX_DURATION_CAP_MS: f64 = 1_800_000.0;

/// senpi SUMMARIZATION_TOTAL_BUDGET_MS.
pub const SUMMARIZATION_TOTAL_BUDGET_MS: f64 = 900_000.0;

fn positive_finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

/// senpi summarizationTotalBudgetMs.
pub fn summarization_total_budget_ms(attempt_override_ms: Option<f64>) -> f64 {
    let override_ms = positive_finite(attempt_override_ms).map(|value| value.min(SUMMARIZATION_MAX_DURATION_CAP_MS)).unwrap_or(0.0);
    SUMMARIZATION_TOTAL_BUDGET_MS.max(override_ms)
}

/// senpi SummarizationTotalBudgetError.
#[derive(Debug, Clone, PartialEq)]
pub struct SummarizationTotalBudgetError {
    pub total_budget_ms: f64,
}

impl std::fmt::Display for SummarizationTotalBudgetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Compaction exceeded its {}ms total wall-clock budget across every summarization attempt and retry",
            self.total_budget_ms
        )
    }
}

impl std::error::Error for SummarizationTotalBudgetError {}

type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// senpi SummarizationDeadline.
#[derive(Clone)]
pub struct SummarizationDeadline {
    pub total_budget_ms: f64,
    started_ms: f64,
    now: Clock,
}

impl SummarizationDeadline {
    /// senpi remainingMs: never negative.
    pub fn remaining_ms(&self) -> f64 {
        (self.total_budget_ms - ((self.now)() - self.started_ms)).max(0.0)
    }

    /// senpi attemptBudgetMs: clamped to what the compaction has left.
    pub fn attempt_budget_ms(&self, requested_ms: f64) -> Result<f64, SummarizationTotalBudgetError> {
        let remaining = self.remaining_ms();
        if remaining <= 0.0 {
            return Err(SummarizationTotalBudgetError { total_budget_ms: self.total_budget_ms });
        }
        Ok(requested_ms.min(remaining))
    }
}

impl std::fmt::Debug for SummarizationDeadline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SummarizationDeadline")
            .field("total_budget_ms", &self.total_budget_ms)
            .finish_non_exhaustive()
    }
}

/// senpi createSummarizationDeadline.
pub fn create_summarization_deadline(total_budget_ms: f64, now: Clock) -> SummarizationDeadline {
    let started_ms = now();
    SummarizationDeadline { total_budget_ms, started_ms, now }
}

/// The default clock, senpi's Date.now.
pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

/// senpi summarizationMaxDurationMs: the per-attempt budget, sized to the input.
pub fn summarization_max_duration_ms(estimated_input_tokens: f64, override_ms: Option<f64>) -> f64 {
    if let Some(override_ms) = positive_finite(override_ms) {
        return override_ms.min(SUMMARIZATION_MAX_DURATION_CAP_MS);
    }
    let scaled = estimated_input_tokens * SUMMARIZATION_MAX_DURATION_PER_TOKEN_MS;
    scaled.max(DEFAULT_SUMMARIZATION_MAX_DURATION_MS as f64).min(SUMMARIZATION_MAX_DURATION_CAP_MS)
}

/// The signal type the watchdog observes.
pub type WatchdogSignal = AbortSignal;

/// Drain provider events and settle the final result under the same deadlines.
pub async fn consume_stream_with_idle_timeout(
    stream: &maho_ai::types::AssistantMessageEventStream,
    idle_timeout_ms: u64,
    max_duration_ms: Option<u64>,
    signal: Option<&WatchdogSignal>,
    abort: impl Fn(),
) -> Result<maho_ai::types::AssistantMessage, maho_ai::utils::event_stream::StreamError> {
    use maho_ai::utils::event_stream::StreamError;
    use std::time::Duration;
    let deadline = max_duration_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
    let mut settling = signal.is_some_and(AbortSignal::aborted);
    loop {
        let is_settling = settling;
        let read = async {
            if is_settling { stream.result().await.map(|message| (Some(message), true)) }
            else { stream.next().await.map(|event| (None, event.is_none())) }
        };
        tokio::pin!(read);
        let idle = tokio::time::sleep(Duration::from_millis(idle_timeout_ms));
        tokio::pin!(idle);
        let duration = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(duration);
        let cancelled = async {
            if !is_settling {
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } else { std::future::pending::<()>().await; }
        };
        tokio::pin!(cancelled);
        tokio::select! {
            biased;
            result = &mut read => {
                let (message, ended) = result?;
                if let Some(message) = message { return Ok(message); }
                settling = ended;
            }
            () = &mut cancelled => { settling = true; }
            () = &mut duration => {
                abort();
                return Err(StreamError::new(StreamDurationBudgetError::new(max_duration_ms.unwrap_or_default() as i64).to_string()));
            }
            () = &mut idle => {
                abort();
                return Err(StreamError::new(StreamIdleTimeoutError::new(idle_timeout_ms as i64).to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[tokio::test]
    async fn stalled_and_unsettled_streams_abort_under_watchdogs() {
        for (ended, idle, duration) in [(false, 0, None), (true, 0, None), (false, 1000, Some(0))] {
            let stream = maho_ai::types::AssistantMessageEventStream::assistant();
            if ended { stream.end(None); }
            let aborted = std::sync::atomic::AtomicBool::new(false);
            let result = consume_stream_with_idle_timeout(&stream, idle, duration, None,
                || aborted.store(true, Ordering::SeqCst)).await;
            assert!(result.is_err());
            assert!(aborted.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn caller_abort_settles_ready_terminal_without_watchdog_abort() {
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let stream = maho_ai::types::AssistantMessageEventStream::assistant();
        let message = maho_ai::providers::faux::faux_assistant_message("settled", Default::default());
        stream.end(Some(message.clone()));
        let result = consume_stream_with_idle_timeout(&stream, 0, Some(0), Some(&controller.signal()),
            || panic!("ready settlement must win")).await.expect("settled result");
        assert_eq!(result, message);
    }

    #[test]
    fn the_error_texts_match_the_pinned_source() {
        assert_eq!(
            StreamIdleTimeoutError::new(300_000).to_string(),
            "Summarization stream stalled: no provider events for 300000ms; treating the request as dead"
        );
        assert_eq!(
            StreamDurationBudgetError::new(120_000).to_string(),
            "Summarization stream exceeded its 120000ms wall-clock budget; treating the request as too slow to keep the session waiting"
        );
        assert_eq!(
            SummarizationTotalBudgetError { total_budget_ms: 900_000.0 }.to_string(),
            "Compaction exceeded its 900000ms total wall-clock budget across every summarization attempt and retry"
        );
    }

    #[test]
    fn the_total_budget_never_shrinks_and_only_an_explicit_override_raises_it() {
        assert_eq!(summarization_total_budget_ms(None), 900_000.0);
        assert_eq!(summarization_total_budget_ms(Some(0.0)), 900_000.0);
        assert_eq!(summarization_total_budget_ms(Some(-5.0)), 900_000.0);
        assert_eq!(summarization_total_budget_ms(Some(f64::NAN)), 900_000.0);
        assert_eq!(summarization_total_budget_ms(Some(60_000.0)), 900_000.0);
        assert_eq!(summarization_total_budget_ms(Some(1_200_000.0)), 1_200_000.0);
        assert_eq!(summarization_total_budget_ms(Some(5_000_000.0)), 1_800_000.0);
    }

    #[test]
    fn the_attempt_budget_keeps_the_floor_below_sixty_thousand_tokens() {
        assert_eq!(summarization_max_duration_ms(0.0, None), 120_000.0);
        assert_eq!(summarization_max_duration_ms(50_000.0, None), 120_000.0);
        assert_eq!(summarization_max_duration_ms(60_000.0, None), 120_000.0);
        assert_eq!(summarization_max_duration_ms(100_000.0, None), 200_000.0);
        assert_eq!(summarization_max_duration_ms(2_000_000.0, None), 1_800_000.0);
    }

    #[test]
    fn an_explicit_override_replaces_the_computed_attempt_budget() {
        assert_eq!(summarization_max_duration_ms(0.0, Some(300_000.0)), 300_000.0);
        assert_eq!(summarization_max_duration_ms(500_000.0, Some(1_000.0)), 1_000.0);
        assert_eq!(summarization_max_duration_ms(0.0, Some(9_000_000.0)), 1_800_000.0);
        assert_eq!(summarization_max_duration_ms(0.0, Some(0.0)), 120_000.0);
        assert_eq!(summarization_max_duration_ms(0.0, Some(f64::INFINITY)), 120_000.0);
    }

    #[test]
    fn the_deadline_counts_down_and_clamps_each_attempt() {
        let clock = Arc::new(AtomicI64::new(0));
        let reader = clock.clone();
        let deadline = create_summarization_deadline(1_000.0, Arc::new(move || reader.load(Ordering::SeqCst) as f64));
        assert_eq!(deadline.remaining_ms(), 1_000.0);
        assert_eq!(deadline.attempt_budget_ms(5_000.0).expect("budget"), 1_000.0);
        clock.store(400, Ordering::SeqCst);
        assert_eq!(deadline.remaining_ms(), 600.0);
        assert_eq!(deadline.attempt_budget_ms(5_000.0).expect("budget"), 600.0);
        assert_eq!(deadline.attempt_budget_ms(100.0).expect("budget"), 100.0);
        clock.store(1_500, Ordering::SeqCst);
        assert_eq!(deadline.remaining_ms(), 0.0);
        let error = deadline.attempt_budget_ms(100.0).expect_err("budget exhausted");
        assert_eq!(error.total_budget_ms, 1_000.0);
    }
}
