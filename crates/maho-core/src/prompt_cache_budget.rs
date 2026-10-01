//! Port of senpi packages/coding-agent/src/core/prompt-cache-budget.ts.

use std::collections::HashMap;

use maho_ai::model::Model;
use maho_ai::types::ProviderEnv;
use maho_ai::utils::prompt_cache_ttl::resolve_prompt_cache_ttl_seconds;

pub const DEFAULT_PROMPT_CACHE_SAFETY_BUFFER_SECONDS: i64 = 30;
pub const PROMPT_CACHE_SAFE_WAIT_ENV: &str = "MAHO_PROMPT_CACHE_SAFE_WAIT_SECONDS";

fn to_provider_env(env: &HashMap<String, String>) -> ProviderEnv {
    env.iter().map(|(key, value)| (key.clone(), value.clone())).collect()
}

fn resolve_buffer_seconds(configured: Option<f64>) -> i64 {
    match configured {
        Some(value) if value.is_finite() && value >= 0.0 => value.trunc() as i64,
        _ => DEFAULT_PROMPT_CACHE_SAFETY_BUFFER_SECONDS,
    }
}

/// Longest a tool may block in the foreground without risking prompt-cache expiry: the active
/// model's cache TTL minus a safety buffer. None means "no cache-derived budget".
pub fn resolve_prompt_cache_safe_wait_seconds(
    model: Option<&Model>,
    cache_aware_timeouts: bool,
    safety_buffer_seconds: Option<f64>,
    env: &HashMap<String, String>,
) -> Option<i64> {
    if !cache_aware_timeouts {
        return None;
    }
    let model = model?;
    let ttl_seconds = resolve_prompt_cache_ttl_seconds(model, Some(&to_provider_env(env)))?;
    let safe_wait = ttl_seconds as i64 - resolve_buffer_seconds(safety_buffer_seconds);
    if safe_wait >= 1 { Some(safe_wait) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "m", "name": "M", "api": "openai-completions", "provider": "anthropic", "baseUrl": "",
            "reasoning": false, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model")
    }

    #[test]
    fn disabled_cache_aware_timeouts_yield_no_budget() {
        assert!(resolve_prompt_cache_safe_wait_seconds(Some(&model()), false, None, &HashMap::new()).is_none());
    }

    #[test]
    fn a_missing_model_yields_no_budget() {
        assert!(resolve_prompt_cache_safe_wait_seconds(None, true, None, &HashMap::new()).is_none());
    }

    #[test]
    fn a_negative_buffer_falls_back_to_the_default() {
        assert_eq!(resolve_buffer_seconds(Some(-5.0)), DEFAULT_PROMPT_CACHE_SAFETY_BUFFER_SECONDS);
        assert_eq!(resolve_buffer_seconds(Some(10.5)), 10);
        assert_eq!(resolve_buffer_seconds(None), DEFAULT_PROMPT_CACHE_SAFETY_BUFFER_SECONDS);
    }
}
