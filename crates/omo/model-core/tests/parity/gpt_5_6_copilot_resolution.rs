use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::ExtendedModelResolutionInput;
use model_core::FallbackEntry;
use model_core::ModelResolutionProvenance;
use model_core::ModelResolutionResult;
use model_core::resolve_model_with_fallback;
use pretty_assertions::assert_eq;

use crate::support::set;

fn resolve(chain: &[FallbackEntry], available: &[&str]) -> Option<ModelResolutionResult> {
    resolve_model_with_fallback(&ExtendedModelResolutionInput {
        fallback_chain: Some(chain.to_vec()),
        available_models: set(available),
        system_default_model: Some("system/default".to_string()),
        ..Default::default()
    })
}

fn provider_fallback(model: &str, variant: &str) -> Option<ModelResolutionResult> {
    Some(ModelResolutionResult {
        model: model.to_string(),
        source: ModelResolutionProvenance::ProviderFallback,
        variant: Some(variant.to_string()),
    })
}

#[test]
fn unspecified_high_never_borrows_copilot_astra() {
    let with_opus = [
        "github-copilot/gpt-6-astra",
        "github-copilot/claude-opus-5-5",
    ];
    let res_with_opus = resolve(
        &CATEGORY_MODEL_REQUIREMENTS["unspecified-high"].fallback_chain,
        &with_opus,
    );
    assert_eq!(
        res_with_opus,
        provider_fallback("github-copilot/claude-opus-5-5", "medium")
    );

    let gpt_only = ["github-copilot/gpt-6-astra", "github-copilot/gpt-5.6-sol"];
    let res_gpt_only = resolve(
        &CATEGORY_MODEL_REQUIREMENTS["unspecified-high"].fallback_chain,
        &gpt_only,
    );
    assert_eq!(
        res_gpt_only,
        Some(ModelResolutionResult {
            model: "system/default".to_string(),
            source: ModelResolutionProvenance::SystemDefault,
            variant: None,
        })
    );
}

#[test]
fn exact_lanes_skip_older_models_and_unlisted_copilot() {
    let older = &[
        "openai/gpt-6-astra",
        "openai/gpt-6.1-sol",
        "openai/gpt-5.6-sol",
        "github-copilot/gpt-6-astra",
        "github-copilot/gpt-5.6-sol",
    ][..];
    let copilot = &["github-copilot/gpt-6.2-astra", "github-copilot/gpt-6.2-sol"][..];
    for category in ["ultrabrain", "deep-high", "deep-low"] {
        for registry in [older, copilot] {
            assert_eq!(
                resolve(
                    &CATEGORY_MODEL_REQUIREMENTS[category].fallback_chain,
                    registry
                ),
                Some(ModelResolutionResult {
                    model: "system/default".to_string(),
                    source: ModelResolutionProvenance::SystemDefault,
                    variant: None,
                }),
                "{category} with {registry:?}"
            );
        }
    }
}

#[test]
fn exact_lanes_prefer_exact_over_fast_on_the_same_provider() {
    for (category, exact, variant) in [
        ("ultrabrain", "chatgpt-subscription/gpt-6.2-astra", "max"),
        ("deep-high", "chatgpt-subscription/gpt-6.2-astra", "high"),
        ("deep-low", "chatgpt-subscription/gpt-6.2-sol", "medium"),
    ] {
        let registry = [
            "chatgpt-subscription/gpt-6.2-astra-fast",
            "chatgpt-subscription/gpt-6.2-sol-fast",
            "openai/gpt-6-astra",
            "openai/gpt-6.1-sol",
            exact,
        ];
        assert_eq!(
            resolve(
                &CATEGORY_MODEL_REQUIREMENTS[category].fallback_chain,
                &registry
            ),
            provider_fallback(exact, variant),
            "{category}"
        );
    }
}

#[test]
fn each_requirement_selects_its_copilot_gpt_model_with_its_configured_variant() {
    let cases = [
        (
            "hephaestus",
            &AGENT_MODEL_REQUIREMENTS["hephaestus"],
            "github-copilot/gpt-5.6-sol",
            "medium",
        ),
        (
            "momus",
            &AGENT_MODEL_REQUIREMENTS["momus"],
            "github-copilot/gpt-6-astra",
            "high",
        ),
        (
            "unspecified-low",
            &CATEGORY_MODEL_REQUIREMENTS["unspecified-low"],
            "github-copilot/gpt-5.6-sol",
            "medium",
        ),
    ];
    for (name, requirement, expected_model, expected_variant) in cases {
        let result = resolve(
            &requirement.fallback_chain,
            &[expected_model, "github-copilot/gpt-5.5"],
        );

        assert_eq!(
            result,
            provider_fallback(expected_model, expected_variant),
            "{name}"
        );
    }
}

#[test]
fn warm_cache_does_not_pick_up_transformed_vercel_astra() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &["vercel/openai/gpt-6-astra"],
    );

    assert_eq!(
        result,
        Some(ModelResolutionResult {
            model: "system/default".to_string(),
            source: ModelResolutionProvenance::SystemDefault,
            variant: None,
        })
    );
}

#[test]
fn warm_cache_prefers_the_copilot_rung_over_a_transformed_vercel_astra() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &["github-copilot/gpt-6-astra", "vercel/openai/gpt-6-astra"],
    );

    assert_eq!(
        result,
        provider_fallback("github-copilot/gpt-6-astra", "high")
    );
}

#[test]
fn copilot_is_never_included_in_a_gpt_5_6_xhigh_rung() {
    let copilot_xhigh_entries: Vec<&FallbackEntry> = AGENT_MODEL_REQUIREMENTS
        .values()
        .chain(CATEGORY_MODEL_REQUIREMENTS.values())
        .flat_map(|requirement| &requirement.fallback_chain)
        .filter(|entry| {
            entry.model.starts_with("gpt-5.6-")
                && entry.providers.iter().any(|p| p == "github-copilot")
                && entry.variant.as_deref() == Some("xhigh")
        })
        .collect();

    assert_eq!(copilot_xhigh_entries, Vec::<&FallbackEntry>::new());
}

#[test]
fn momus_prefers_native_astra_xhigh_over_the_copilot_astra_rung_when_both_are_available() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &[
            "chatgpt-subscription/gpt-6-astra",
            "github-copilot/gpt-6-astra",
        ],
    );

    assert_eq!(
        result,
        provider_fallback("chatgpt-subscription/gpt-6-astra", "xhigh")
    );
}

#[test]
fn momus_falls_to_claude_opus_5_5_max_when_no_astra_rung_is_available() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &["github-copilot/gpt-5.6-sol", "anthropic/claude-opus-5-5"],
    );

    assert_eq!(
        result,
        provider_fallback("anthropic/claude-opus-5-5", "max")
    );
}

#[test]
fn requirements_ignore_gpt_5_5_when_their_frontier_gpt_rungs_are_unavailable() {
    let cases = [
        ("hephaestus", &AGENT_MODEL_REQUIREMENTS["hephaestus"]),
        ("momus", &AGENT_MODEL_REQUIREMENTS["momus"]),
        ("deep-low", &CATEGORY_MODEL_REQUIREMENTS["deep-low"]),
        ("deep-high", &CATEGORY_MODEL_REQUIREMENTS["deep-high"]),
    ];
    for (name, requirement) in cases {
        let result = resolve(&requirement.fallback_chain, &["github-copilot/gpt-5.5"]);

        assert_eq!(
            result,
            Some(ModelResolutionResult {
                model: "system/default".to_string(),
                source: ModelResolutionProvenance::SystemDefault,
                variant: None,
            }),
            "{name}"
        );
    }
}
