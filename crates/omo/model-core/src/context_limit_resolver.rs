use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

const DEFAULT_ANTHROPIC_ACTUAL_LIMIT: i64 = 200_000;
const ANTHROPIC_GA_1M_LIMIT: i64 = 1_000_000;

/// Adapter-owned model cache state consulted for context limits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextLimitModelCacheState {
    pub anthropic_context_1m_enabled: bool,
    /// Keyed by `provider/model`.
    pub model_context_limits_cache: Option<HashMap<String, i64>>,
}

/// The `ANTHROPIC_1M_CONTEXT` / `VERTEX_ANTHROPIC_1M_CONTEXT` environment switches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextLimitEnv {
    pub anthropic_1m_context: bool,
    pub vertex_anthropic_1m_context: bool,
}

impl ContextLimitEnv {
    /// Reads the switches from the process environment (`"true"` enables).
    #[must_use]
    pub fn from_process() -> Self {
        let enabled = |key: &str| std::env::var(key).is_ok_and(|value| value == "true");
        Self {
            anthropic_1m_context: enabled("ANTHROPIC_1M_CONTEXT"),
            vertex_anthropic_1m_context: enabled("VERTEX_ANTHROPIC_1M_CONTEXT"),
        }
    }
}

fn is_anthropic_provider(provider_id: &str, model_id: &str) -> bool {
    let normalized = provider_id.to_lowercase();
    matches!(
        normalized.as_str(),
        "anthropic" | "google-vertex-anthropic" | "aws-bedrock-anthropic"
    ) || (normalized == "google" && model_id.to_lowercase().starts_with("claude-"))
}

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
fn has_ga_1m_context(model_id: &str) -> bool {
    static CLAUDE_4X: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^claude-(opus|sonnet)-4(?:-|\.)(?:6|7|8)(?:-high)?$").expect("valid regex")
    });
    static CLAUDE_5: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^claude-(?:fable|mythos|sonnet)-5$").expect("valid regex"));
    CLAUDE_4X.is_match(model_id) || CLAUDE_5.is_match(model_id)
}

/// Context limit using the process environment switches.
#[must_use]
pub fn resolve_actual_context_limit(
    provider_id: &str,
    model_id: &str,
    model_cache_state: Option<&ContextLimitModelCacheState>,
) -> Option<i64> {
    resolve_actual_context_limit_with_env(
        provider_id,
        model_id,
        model_cache_state,
        ContextLimitEnv::from_process(),
    )
}

/// Anthropic-family models get 1M when enabled or GA, else 200K; others read the cache.
#[must_use]
pub fn resolve_actual_context_limit_with_env(
    provider_id: &str,
    model_id: &str,
    model_cache_state: Option<&ContextLimitModelCacheState>,
    env: ContextLimitEnv,
) -> Option<i64> {
    let cached_limit = model_cache_state
        .and_then(|state| state.model_context_limits_cache.as_ref())
        .and_then(|cache| cache.get(&format!("{provider_id}/{model_id}")))
        .copied();

    if !is_anthropic_provider(provider_id, model_id) {
        return cached_limit;
    }

    let explicit_1m = model_cache_state.is_some_and(|state| state.anthropic_context_1m_enabled)
        || env.anthropic_1m_context
        || env.vertex_anthropic_1m_context;
    if explicit_1m {
        return Some(ANTHROPIC_GA_1M_LIMIT);
    }
    if !has_ga_1m_context(model_id) {
        return Some(DEFAULT_ANTHROPIC_ACTUAL_LIMIT);
    }
    Some(
        cached_limit
            .filter(|limit| *limit != 0)
            .unwrap_or(ANTHROPIC_GA_1M_LIMIT),
    )
}
