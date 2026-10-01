//! Port of senpi `packages/agent/src/harness/runtime/drive/retry.ts`.

use std::time::Duration;

use maho_ai::utils::abort::{AbortReason, AbortSignal};
use maho_ai::utils::retry::{RetryPolicy, retry_delay_ms};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub fn retry_not_before(policy: &RetryPolicy, attempt: u32, now: i64) -> i64 {
    let sum = now as f64 + retry_delay_ms(policy, attempt) as f64;
    if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&sum) {
        sum as i64
    } else {
        9_007_199_254_740_991
    }
}

pub fn retry_not_before_now(policy: &RetryPolicy, attempt: u32) -> i64 {
    retry_not_before(policy, attempt, now_ms())
}

pub async fn wait_until(not_before: i64, signal: &AbortSignal) -> Result<(), AbortReason> {
    loop {
        if signal.aborted() {
            return Err(signal.reason().unwrap_or_else(AbortReason::dom_default));
        }
        let remaining = not_before.saturating_sub(now_ms());
        if remaining <= 0 {
            return Ok(());
        }
        let capped = remaining.min(2_147_483_647) as u64;
        tokio::select! {
            biased;
            () = signal.cancelled() => {
                return Err(signal.reason().unwrap_or_else(AbortReason::dom_default));
            }
            () = tokio::time::sleep(Duration::from_millis(capped)) => {}
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as i64)
}
