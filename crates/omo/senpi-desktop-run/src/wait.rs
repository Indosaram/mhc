//! Native Rust port of `src/run/wait.ts`.
//!
//! Run-scoped `wait(ms)` and predicate polling, cancelled with the run signal.

use std::future::Future;
use std::time::Duration;

use crate::context::{ComputerRunError, RunContext};

/// Headroom below the run budget so a predicate deadline fails with its own message first.
pub const RUN_BUDGET_SLACK_MS: u64 = 1_000;
pub const DEFAULT_PREDICATE_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_INTERVAL_MS: u64 = 100;
pub const MIN_INTERVAL_MS: u64 = 10;

#[derive(Debug, Clone, Default)]
pub struct WaitOptions {
    /// Max time to poll a predicate, in ms (default 30 s, clamped below the run budget).
    pub timeout_ms: Option<u64>,
    /// Poll interval in ms (default 100, floor 10).
    pub interval_ms: Option<u64>,
}

pub fn predicate_timeout(run_timeout_ms: u64, explicit: Option<u64>) -> u64 {
    let budget = run_timeout_ms.saturating_sub(RUN_BUDGET_SLACK_MS).max(1);
    match explicit {
        Some(0) => budget,
        Some(ms) => ms.min(budget),
        None => DEFAULT_PREDICATE_TIMEOUT_MS.min(budget),
    }
}

pub async fn run_sleep(ms: u64, context: &RunContext) -> Result<(), ComputerRunError> {
    context.check_aborted().await?;
    let duration = Duration::from_millis(ms);
    let mut check_interval = tokio::time::interval(Duration::from_millis(10));
    let start = tokio::time::Instant::now();
    while start.elapsed() < duration {
        check_interval.tick().await;
        context.check_aborted().await?;
    }
    Ok(())
}

pub async fn run_poll<F, Fut, T>(
    mut predicate: F,
    context: &RunContext,
    timeout_ms: u64,
    interval_ms: u64,
) -> Result<T, ComputerRunError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let interval = interval_ms.max(MIN_INTERVAL_MS);
    let start = tokio::time::Instant::now();
    let max_duration = Duration::from_millis(timeout_ms);

    loop {
        context.check_aborted().await?;
        if let Some(val) = predicate().await {
            return Ok(val);
        }
        context.check_aborted().await?;
        if start.elapsed() >= max_duration {
            return Err(ComputerRunError::wait(format!(
                "wait(predicate) timed out after {timeout_ms} ms; the predicate never returned truthy"
            )));
        }
        run_sleep(interval, context).await?;
    }
}
