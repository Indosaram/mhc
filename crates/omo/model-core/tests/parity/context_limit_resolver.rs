//! The TS suite mutates `process.env`; here the switches are passed explicitly
//! (`ContextLimitEnv`) so the tests never touch the real environment.

use std::collections::HashMap;

use model_core::ContextLimitEnv;
use model_core::ContextLimitModelCacheState;
use model_core::resolve_actual_context_limit_with_env;
use pretty_assertions::assert_eq;

const UNSET: ContextLimitEnv = ContextLimitEnv {
    anthropic_1m_context: false,
    vertex_anthropic_1m_context: false,
};

fn state(cache: &[(&str, i64)]) -> ContextLimitModelCacheState {
    ContextLimitModelCacheState {
        anthropic_context_1m_enabled: false,
        model_context_limits_cache: (!cache.is_empty()).then(|| {
            cache
                .iter()
                .map(|(key, limit)| ((*key).to_string(), *limit))
                .collect::<HashMap<_, _>>()
        }),
    }
}

fn limit(
    provider_id: &str,
    model_id: &str,
    cache: &[(&str, i64)],
    env: ContextLimitEnv,
) -> Option<i64> {
    resolve_actual_context_limit_with_env(provider_id, model_id, Some(&state(cache)), env)
}

#[test]
fn returns_cached_limit_for_non_anthropic_providers() {
    assert_eq!(
        limit("openai", "gpt-5", &[("openai/gpt-5", 400_000)], UNSET),
        Some(400_000)
    );
}

#[test]
fn returns_ga_1m_for_anthropic_4_6_4_7_models_without_explicit_1m_mode() {
    assert_eq!(
        limit("anthropic", "claude-sonnet-4-6", &[], UNSET),
        Some(1_000_000)
    );
}

#[test]
fn returns_ga_1m_for_anthropic_claude_opus_4_8_without_explicit_1m_mode() {
    assert_eq!(
        limit("anthropic", "claude-opus-4-8", &[], UNSET),
        Some(1_000_000)
    );
}

#[test]
fn returns_ga_1m_for_anthropic_claude_opus_4_8_high_without_explicit_1m_mode() {
    assert_eq!(
        limit("anthropic", "claude-opus-4-8-high", &[], UNSET),
        Some(1_000_000)
    );
}

#[test]
fn returns_ga_1m_for_antigravity_claude_models_served_by_google() {
    assert_eq!(
        limit("google", "claude-sonnet-4-6", &[], UNSET),
        Some(1_000_000)
    );
}

#[test]
fn returns_cached_limit_for_gemini_models_served_by_google() {
    assert_eq!(
        limit(
            "google",
            "gemini-3.1-pro",
            &[("google/gemini-3.1-pro", 1_048_576)],
            UNSET
        ),
        Some(1_048_576)
    );
}

#[test]
fn uses_cached_limit_for_ga_anthropic_models_when_cache_exists() {
    assert_eq!(
        limit(
            "anthropic",
            "claude-opus-4-7",
            &[("anthropic/claude-opus-4-7", 700_000)],
            UNSET
        ),
        Some(700_000)
    );
}

#[test]
fn returns_1m_when_anthropic_1m_context_is_true_regardless_of_model() {
    let env = ContextLimitEnv {
        anthropic_1m_context: true,
        vertex_anthropic_1m_context: false,
    };
    assert_eq!(
        limit(
            "anthropic",
            "claude-sonnet-4-5",
            &[("anthropic/claude-sonnet-4-5", 200_000)],
            env
        ),
        Some(1_000_000)
    );
}

#[test]
fn returns_1m_when_vertex_anthropic_1m_context_is_true_for_anthropic_aliases() {
    let env = ContextLimitEnv {
        anthropic_1m_context: false,
        vertex_anthropic_1m_context: true,
    };
    assert_eq!(
        limit("google-vertex-anthropic", "claude-sonnet-4-5", &[], env),
        Some(1_000_000)
    );
}

#[test]
fn returns_ga_1m_for_claude_fable_5_and_claude_mythos_5() {
    assert_eq!(
        limit("anthropic", "claude-fable-5", &[], UNSET),
        Some(1_000_000)
    );
    assert_eq!(
        limit("anthropic", "claude-mythos-5", &[], UNSET),
        Some(1_000_000)
    );
    assert_eq!(
        limit("anthropic", "claude-sonnet-5", &[], UNSET),
        Some(1_000_000)
    );
}

#[test]
fn returns_ga_1m_for_claude_opus_4_8() {
    assert_eq!(
        limit("anthropic", "claude-opus-4-8", &[], UNSET),
        Some(1_000_000)
    );
}

#[test]
fn returns_ga_1m_for_claude_fable_5_on_google_vertex_anthropic() {
    assert_eq!(
        limit("google-vertex-anthropic", "claude-fable-5", &[], UNSET),
        Some(1_000_000)
    );
}
