//! Port of senpi packages/agent/src/harness/config.ts.

use maho_ai::utils::retry::{DEFAULT_MAX_AGENT_RETRY_DELAY_MS, RetryPolicy};

use super::compaction::CompactionSettings;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `DEFAULT_RETRY_POLICY`.
pub fn default_retry_policy() -> RetryPolicy {
    RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 1_000,
        max_agent_delay_ms: Some(DEFAULT_MAX_AGENT_RETRY_DELAY_MS),
        random: None,
    }
}

/// `validateToolNames(tools)`: reject duplicate tool names.
pub fn validate_tool_names<S: AsRef<str>>(tools: &[S]) -> Result<(), String> {
    let mut names: Vec<&str> = Vec::with_capacity(tools.len());
    for tool in tools {
        let name = tool.as_ref();
        if names.contains(&name) {
            return Err(format!("Duplicate tool name: {}", serde_json::Value::String(name.to_string())));
        }
        names.push(name);
    }
    Ok(())
}

/// `validateRetryPolicy(policy)`: reject non-finite or negative integer values.
pub fn validate_retry_policy(policy: &RetryPolicy) -> Result<(), String> {
    // TS also rejects maxRetries/baseDelayMs that are not safe integers; u32/u64 cannot carry a
    // negative or fractional value, and only the upper bound is representable here.
    if policy.base_delay_ms > MAX_SAFE_INTEGER
        || policy.max_agent_delay_ms.is_some_and(|value| value > MAX_SAFE_INTEGER)
    {
        return Err("Retry policy values must be finite non-negative safe integers".to_string());
    }
    Ok(())
}

/// `validateCompactionSettings(settings)`: reject non-finite or negative token counts.
pub fn validate_compaction_settings(settings: &CompactionSettings) -> Result<(), String> {
    if settings.reserve_tokens > MAX_SAFE_INTEGER || settings.keep_recent_tokens > MAX_SAFE_INTEGER {
        return Err("Compaction token counts must be finite non-negative safe integers".to_string());
    }
    Ok(())
}
