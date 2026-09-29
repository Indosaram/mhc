use model_core::AliasSource;
use model_core::ModelIdAliasResolution;
use model_core::resolve_model_id_alias;
use pretty_assertions::assert_eq;

fn resolution(
    requested: &str,
    canonical: &str,
    source: AliasSource,
    rule_id: Option<&str>,
) -> ModelIdAliasResolution {
    ModelIdAliasResolution {
        requested_model_id: requested.to_string(),
        canonical_model_id: canonical.to_string(),
        source,
        rule_id: rule_id.map(str::to_string),
    }
}

fn canonical(requested: &str, canonical_id: &str) -> ModelIdAliasResolution {
    resolution(requested, canonical_id, AliasSource::Canonical, None)
}

#[test]
fn keeps_canonical_model_ids_unchanged() {
    assert_eq!(
        resolve_model_id_alias("gpt-5.4", None),
        canonical("gpt-5.4", "gpt-5.4")
    );
}

#[test]
fn strips_provider_prefixes_when_the_input_is_already_canonical() {
    assert_eq!(
        resolve_model_id_alias("anthropic/claude-sonnet-4-6", None),
        canonical("anthropic/claude-sonnet-4-6", "claude-sonnet-4-6")
    );
}

#[test]
fn normalizes_gemini_tier_aliases_through_a_pattern_rule() {
    assert_eq!(
        resolve_model_id_alias("gemini-3.1-pro-high", None),
        resolution(
            "gemini-3.1-pro-high",
            "gemini-3.1-pro",
            AliasSource::PatternAlias,
            Some("gemini-3.1-pro-tier-alias")
        )
    );
}

#[test]
fn normalizes_provider_prefixed_gemini_tier_aliases_to_bare_canonical_ids() {
    assert_eq!(
        resolve_model_id_alias("google/gemini-3.1-pro-high", None),
        resolution(
            "google/gemini-3.1-pro-high",
            "gemini-3.1-pro",
            AliasSource::PatternAlias,
            Some("gemini-3.1-pro-tier-alias")
        )
    );
}

#[test]
fn keeps_exceptional_gemini_preview_aliases_as_exact_rules() {
    assert_eq!(
        resolve_model_id_alias("gemini-3-pro-high", None),
        resolution(
            "gemini-3-pro-high",
            "gemini-3-pro-preview",
            AliasSource::ExactAlias,
            Some("gemini-3-pro-tier-alias")
        )
    );
}

#[test]
fn leaves_discontinued_kimi_k2pb_ids_unaliased() {
    assert_eq!(
        resolve_model_id_alias("kimi-for-coding/k2pb", None),
        canonical("kimi-for-coding/k2pb", "k2pb")
    );
}

#[test]
fn treats_github_copilot_dotted_claude_opus_4_7_as_canonical() {
    assert_eq!(
        resolve_model_id_alias("github-copilot/claude-opus-4.7", None),
        canonical("github-copilot/claude-opus-4.7", "claude-opus-4.7")
    );
}

#[test]
fn does_not_resolve_prototype_keys_as_aliases() {
    assert_eq!(
        resolve_model_id_alias("constructor", None),
        canonical("constructor", "constructor")
    );
}

#[test]
fn normalizes_provider_prefixed_claude_thinking_aliases_through_a_pattern_rule() {
    assert_eq!(
        resolve_model_id_alias("anthropic/claude-opus-4-7-thinking", None),
        resolution(
            "anthropic/claude-opus-4-7-thinking",
            "claude-opus-4-7",
            AliasSource::PatternAlias,
            Some("claude-thinking-legacy-alias")
        )
    );
}

#[test]
fn does_not_pattern_match_nearby_canonical_claude_ids_incorrectly() {
    assert_eq!(
        resolve_model_id_alias("claude-opus-4-7-think", None),
        canonical("claude-opus-4-7-think", "claude-opus-4-7-think")
    );
}

#[test]
fn does_not_pattern_match_canonical_gemini_preview_ids_incorrectly() {
    assert_eq!(
        resolve_model_id_alias("gemini-3.1-pro-preview", None),
        canonical("gemini-3.1-pro-preview", "gemini-3.1-pro-preview")
    );
}

#[test]
fn normalizes_legacy_claude_thinking_aliases_through_a_pattern_rule() {
    assert_eq!(
        resolve_model_id_alias("claude-opus-4-7-thinking", None),
        resolution(
            "claude-opus-4-7-thinking",
            "claude-opus-4-7",
            AliasSource::PatternAlias,
            Some("claude-thinking-legacy-alias")
        )
    );
}

#[test]
fn treats_claude_opus_4_6_thinking_as_canonical_not_as_a_legacy_alias() {
    assert_eq!(
        resolve_model_id_alias("claude-opus-4-6-thinking", None),
        canonical("claude-opus-4-6-thinking", "claude-opus-4-6-thinking")
    );
}

#[test]
fn normalizes_openai_gpt_5_6_fast_service_tier_aliases() {
    for (alias_model_id, canonical_model_id) in [
        ("gpt-5.6-sol-fast", "gpt-5.6-sol"),
        ("gpt-5.6-terra-fast", "gpt-5.6-terra"),
        ("gpt-5.6-luna-fast", "gpt-5.6-luna"),
    ] {
        assert_eq!(
            resolve_model_id_alias(alias_model_id, Some("openai")),
            resolution(
                alias_model_id,
                canonical_model_id,
                AliasSource::PatternAlias,
                Some("openai-gpt-5.6-fast-service-tier-alias")
            )
        );
    }
}

#[test]
fn does_not_normalize_gpt_5_6_fast_suffixes_for_unrelated_providers_or_nearby_ids() {
    let copilot = resolve_model_id_alias("gpt-5.6-sol-fast", Some("github-copilot"));
    assert_eq!(
        (copilot.canonical_model_id.as_str(), copilot.source),
        ("gpt-5.6-sol-fast", AliasSource::Canonical)
    );
    let preview = resolve_model_id_alias("gpt-5.6-sol-fast-preview", Some("openai"));
    assert_eq!(
        (preview.canonical_model_id.as_str(), preview.source),
        ("gpt-5.6-sol-fast-preview", AliasSource::Canonical)
    );
}

#[test]
fn does_not_normalize_openai_subprovider_aliases_for_unrelated_top_level_providers() {
    assert_eq!(
        resolve_model_id_alias("openai/gpt-5.6-sol-fast", Some("anthropic")),
        canonical("openai/gpt-5.6-sol-fast", "gpt-5.6-sol-fast")
    );
}
