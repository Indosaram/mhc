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
fn each_requirement_selects_its_copilot_gpt_5_6_model_with_its_configured_variant() {
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
            "github-copilot/gpt-5.6-terra",
            "high",
        ),
        (
            "ultrabrain",
            &CATEGORY_MODEL_REQUIREMENTS["ultrabrain"],
            "github-copilot/gpt-5.6-sol",
            "max",
        ),
        (
            "deep",
            &CATEGORY_MODEL_REQUIREMENTS["deep"],
            "github-copilot/gpt-5.6-sol",
            "medium",
        ),
        (
            "unspecified-low",
            &CATEGORY_MODEL_REQUIREMENTS["unspecified-low"],
            "github-copilot/gpt-5.6-terra",
            "high",
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
fn warm_cache_resolves_transformed_vercel_gpt_5_6_with_high() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &["vercel/openai/gpt-5.6-terra"],
    );

    assert_eq!(
        result,
        provider_fallback("vercel/openai/gpt-5.6-terra", "high")
    );
}

#[test]
fn warm_cache_keeps_transformed_vercel_terra_ahead_of_copilot_terra() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &[
            "github-copilot/gpt-5.6-terra",
            "vercel/openai/gpt-5.6-terra",
        ],
    );

    assert_eq!(
        result,
        provider_fallback("vercel/openai/gpt-5.6-terra", "high")
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
fn momus_uses_high_for_its_copilot_sol_fallback_when_terra_is_unavailable() {
    let result = resolve(
        &AGENT_MODEL_REQUIREMENTS["momus"].fallback_chain,
        &["github-copilot/gpt-5.6-sol"],
    );

    assert_eq!(
        result,
        provider_fallback("github-copilot/gpt-5.6-sol", "high")
    );
}

#[test]
fn requirements_ignore_gpt_5_5_when_their_gpt_5_6_rungs_are_unavailable() {
    let cases = [
        ("hephaestus", &AGENT_MODEL_REQUIREMENTS["hephaestus"]),
        ("momus", &AGENT_MODEL_REQUIREMENTS["momus"]),
        ("deep", &CATEGORY_MODEL_REQUIREMENTS["deep"]),
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
