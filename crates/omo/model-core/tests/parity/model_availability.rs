use model_core::fuzzy_match_model;
use pretty_assertions::assert_eq;

use crate::support::set;
use crate::support::strings;

#[test]
fn kimi_dash_and_dot_variants_normalize_symmetrically() {
    let result = fuzzy_match_model(
        "kimi-k2.6",
        &set(&["moonshot/kimi-k2-6"]),
        Some(&strings(&["moonshot"])),
    );
    assert_eq!(result.as_deref(), Some("moonshot/kimi-k2-6"));
}

#[test]
fn glm_dash_and_dot_variants_normalize_symmetrically() {
    let result = fuzzy_match_model("glm-5-1", &set(&["zai/glm-5.1"]), Some(&strings(&["zai"])));
    assert_eq!(result.as_deref(), Some("zai/glm-5.1"));
}

#[test]
fn gpt_dash_and_dot_variants_normalize_symmetrically() {
    let result = fuzzy_match_model(
        "gpt-5.4",
        &set(&["openai/gpt-5-4"]),
        Some(&strings(&["openai"])),
    );
    assert_eq!(result.as_deref(), Some("openai/gpt-5-4"));
}

#[test]
fn exact_model_id_matches_follow_provider_preference_before_model_id_length() {
    let available = set(&[
        "opencode/claude-opus-5",
        "anthropic-subscription/claude-opus-5",
    ]);
    assert_eq!(
        fuzzy_match_model(
            "claude-opus-5",
            &available,
            Some(&strings(&["anthropic-subscription", "opencode"])),
        )
        .as_deref(),
        Some("anthropic-subscription/claude-opus-5")
    );
}

#[test]
fn substring_ties_compare_model_id_length_not_full_provider_model_length() {
    let available = set(&[
        "opencode/claude-opus-5-2025",
        "anthropic-subscription/claude-opus-5",
    ]);
    assert_eq!(
        fuzzy_match_model("claude-opus", &available, None).as_deref(),
        Some("anthropic-subscription/claude-opus-5")
    );
}
