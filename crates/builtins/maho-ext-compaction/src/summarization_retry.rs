//! Summarization retry attempts share half of one attempt's wall-clock budget.
use maho_ai::utils::retry::RetryPolicy;
pub const MAX_SUMMARIZATION_ATTEMPT_RETRIES: u32 = 3;
pub const DEFAULT_SUMMARIZATION_RETRY_BASE_DELAY_MS: u64 = 1000;
pub const DEFAULT_SUMMARIZATION_RETRY_POLICY: RetryPolicy = RetryPolicy {
    enabled: true,
    max_retries: MAX_SUMMARIZATION_ATTEMPT_RETRIES,
    base_delay_ms: DEFAULT_SUMMARIZATION_RETRY_BASE_DELAY_MS,
    max_agent_delay_ms: None,
    random: None,
};
pub fn summarization_retry_total_budget_ms(attempt_budget_ms: f64) -> f64 {
    attempt_budget_ms / 2.0
}
pub fn allow_summarization_retry(elapsed_ms: f64, attempt_budget_ms: Option<f64>) -> bool {
    elapsed_ms < summarization_retry_total_budget_ms(attempt_budget_ms.unwrap_or(120_000.0))
}
