//! Port of senpi `packages/agent/test/harness/runtime/drive-retry.test.ts`.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use maho_agent::harness::runtime::drive::retry::retry_not_before;
use maho_ai::utils::retry::RetryPolicy;

fn policy(base_delay_ms: u64, max_agent_delay_ms: u64, sample: f64) -> RetryPolicy {
    RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms,
        max_agent_delay_ms: Some(max_agent_delay_ms),
        random: Some(Arc::new(move || sample)),
    }
}

#[test]
fn uses_capped_delay_when_computing_retry_readiness() {
    assert_eq!(retry_not_before(&policy(2000, 30000, 1.0), 5, 100), 30100);
}

#[test]
fn keeps_an_uncapped_delay_inside_the_jitter_band() {
    assert_eq!(retry_not_before(&policy(1000, 30000, 0.0), 1, 100), 1000);
    assert_eq!(retry_not_before(&policy(1000, 30000, 1.0), 1, 100), 1200);
}
