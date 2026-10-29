use model_core::CompatibilityCapabilities;
use model_core::CompatibilityChangeReason;
use model_core::CompatibilityField;
use model_core::DesiredModelSettings;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelSettingsCompatibilityChange;
use model_core::ModelSettingsCompatibilityInput;
use model_core::ModelSettingsCompatibilityResult;
use model_core::get_model_capabilities;
use model_core::resolve_compatible_model_settings;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support::strings;

use CompatibilityChangeReason::MaxOutputLimit;
use CompatibilityChangeReason::UnknownModelFamily;
use CompatibilityChangeReason::UnsupportedByModelFamily;
use CompatibilityChangeReason::UnsupportedByModelMetadata;

fn settings(
    provider_id: &str,
    model_id: &str,
    desired: DesiredModelSettings,
    capabilities: Option<CompatibilityCapabilities>,
) -> ModelSettingsCompatibilityResult {
    resolve_compatible_model_settings(&ModelSettingsCompatibilityInput {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        desired,
        capabilities,
    })
}

fn variant(value: &str) -> DesiredModelSettings {
    DesiredModelSettings {
        variant: Some(value.to_string()),
        ..Default::default()
    }
}

fn effort(value: &str) -> DesiredModelSettings {
    DesiredModelSettings {
        reasoning_effort: Some(value.to_string()),
        ..Default::default()
    }
}

fn temperature(value: f64) -> DesiredModelSettings {
    DesiredModelSettings {
        temperature: Some(value),
        ..Default::default()
    }
}

fn thinking_enabled() -> DesiredModelSettings {
    DesiredModelSettings {
        thinking: Some(json!({ "type": "enabled", "budgetTokens": 4096 })),
        ..Default::default()
    }
}

fn change(
    field: CompatibilityField,
    from: &str,
    to: Option<&str>,
    reason: CompatibilityChangeReason,
) -> ModelSettingsCompatibilityChange {
    ModelSettingsCompatibilityChange {
        field,
        from: from.to_string(),
        to: to.map(str::to_string),
        reason,
    }
}

fn result(
    variant: Option<&str>,
    reasoning_effort: Option<&str>,
    changes: Vec<ModelSettingsCompatibilityChange>,
) -> ModelSettingsCompatibilityResult {
    ModelSettingsCompatibilityResult {
        variant: variant.map(str::to_string),
        reasoning_effort: reasoning_effort.map(str::to_string),
        changes,
        ..Default::default()
    }
}

fn heuristic_capabilities(provider_id: &str, model_id: &str) -> CompatibilityCapabilities {
    CompatibilityCapabilities::from(&get_model_capabilities(GetModelCapabilitiesInput {
        provider_id,
        model_id,
        ..Default::default()
    }))
}

fn capabilities_variants(values: &[&str]) -> Option<CompatibilityCapabilities> {
    Some(CompatibilityCapabilities {
        variants: Some(strings(values)),
        ..Default::default()
    })
}

fn max_output(limit: f64) -> Option<CompatibilityCapabilities> {
    Some(CompatibilityCapabilities {
        max_output_tokens: Some(limit),
        ..Default::default()
    })
}

fn supports_temperature(value: bool) -> Option<CompatibilityCapabilities> {
    Some(CompatibilityCapabilities {
        supports_temperature: Some(value),
        ..Default::default()
    })
}

use CompatibilityField::MaxTokens;
use CompatibilityField::ReasoningEffort;
use CompatibilityField::Temperature;
use CompatibilityField::Thinking;
use CompatibilityField::Variant;

#[test]
fn keeps_supported_claude_opus_variant_unchanged() {
    assert_eq!(
        settings("anthropic", "claude-opus-4-7", variant("max"), None),
        result(Some("max"), None, vec![])
    );
}

#[test]
fn uses_model_metadata_first_for_variant_support() {
    assert_eq!(
        settings(
            "anthropic",
            "claude-opus-4-7",
            variant("max"),
            capabilities_variants(&["low", "medium", "high"])
        ),
        result(
            Some("high"),
            None,
            vec![change(
                Variant,
                "max",
                Some("high"),
                UnsupportedByModelMetadata
            )]
        )
    );
}

#[test]
fn prefers_metadata_over_family_heuristics_even_when_family_would_allow_a_higher_level() {
    let result = settings(
        "anthropic",
        "claude-opus-4-7",
        variant("max"),
        capabilities_variants(&["low", "medium"]),
    );
    assert_eq!(result.variant.as_deref(), Some("medium"));
    assert_eq!(
        result.changes,
        vec![change(
            Variant,
            "max",
            Some("medium"),
            UnsupportedByModelMetadata
        )]
    );
}

#[test]
fn downgrades_unsupported_claude_sonnet_max_variant_to_high_when_metadata_is_absent() {
    let result = settings("anthropic", "claude-sonnet-4-6", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes,
        vec![change(
            Variant,
            "max",
            Some("high"),
            UnsupportedByModelFamily
        )]
    );
}

#[test]
fn keeps_supported_gpt_reasoning_effort_unchanged() {
    assert_eq!(
        settings("openai", "gpt-5.4", effort("high"), None),
        result(None, Some("high"), vec![])
    );
}

#[test]
fn keeps_supported_openai_reasoning_family_effort_for_o_series_models() {
    assert_eq!(
        settings("openai", "o3-mini", effort("high"), None),
        result(None, Some("high"), vec![])
    );
}

#[test]
fn does_not_record_case_only_normalization_as_a_compatibility_downgrade() {
    let desired = DesiredModelSettings {
        variant: Some("HIGH".to_string()),
        reasoning_effort: Some("HIGH".to_string()),
        ..Default::default()
    };
    assert_eq!(
        settings("openai", "gpt-5.4", desired, None),
        result(Some("high"), Some("high"), vec![])
    );
}

#[test]
fn drops_reasoning_effort_for_standard_gpt_models() {
    let result = settings("openai", "gpt-4.1", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes,
        vec![change(
            ReasoningEffort,
            "high",
            None,
            UnsupportedByModelFamily
        )]
    );
}

#[test]
fn drops_reasoning_effort_for_claude_family() {
    let result = settings("anthropic", "claude-sonnet-4-6", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes,
        vec![change(
            ReasoningEffort,
            "high",
            None,
            UnsupportedByModelFamily
        )]
    );
}

#[test]
fn handles_combined_variant_and_reasoning_effort_normalization() {
    let desired = DesiredModelSettings {
        variant: Some("max".to_string()),
        reasoning_effort: Some("high".to_string()),
        ..Default::default()
    };
    assert_eq!(
        settings("anthropic", "claude-sonnet-4-6", desired, None),
        result(
            Some("high"),
            None,
            vec![
                change(Variant, "max", Some("high"), UnsupportedByModelFamily),
                change(ReasoningEffort, "high", None, UnsupportedByModelFamily),
            ]
        )
    );
}

#[test]
fn treats_unknown_model_families_conservatively_by_dropping_unsupported_settings() {
    let desired = DesiredModelSettings {
        variant: Some("max".to_string()),
        reasoning_effort: Some("high".to_string()),
        ..Default::default()
    };
    assert_eq!(
        settings("mystery", "mystery-model-1", desired, None),
        result(
            None,
            None,
            vec![
                change(Variant, "max", None, UnknownModelFamily),
                change(ReasoningEffort, "high", None, UnknownModelFamily),
            ]
        )
    );
}

#[test]
fn detects_claude_via_any_provider() {
    for provider_id in [
        "anthropic",
        "aws-bedrock",
        "bedrock",
        "amazon-bedrock",
        "opencode",
        "my-custom-proxy",
        "google-vertex-anthropic",
    ] {
        let result = settings(provider_id, "claude-sonnet-4-6", variant("max"), None);
        assert_eq!(result.variant.as_deref(), Some("high"), "{provider_id}");
        assert_eq!(
            result.changes[0].reason, UnsupportedByModelFamily,
            "{provider_id}"
        );
    }
}

#[test]
fn detects_claude_3_opus_via_any_provider() {
    let result = settings(
        "some-unknown-proxy",
        "claude-3-opus-20240229",
        variant("max"),
        None,
    );
    assert_eq!(result.variant.as_deref(), Some("max"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn detects_openai_reasoning_models_without_requiring_openai_provider() {
    let result = settings("azure-openai", "o3-mini", effort("high"), None);
    assert_eq!(result.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_gemini_keeps_supported_variant() {
    let result = settings("any-provider", "gemini-3.1-pro", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_gemini_downgrades_unsupported_variant() {
    let result = settings("any-provider", "gemini-3.1-pro", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_gemini_drops_reasoning_effort() {
    let result = settings("any-provider", "gemini-3.1-pro", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_grok_keeps_supported_variant() {
    let result = settings("any-provider", "grok-4.3", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_grok_downgrades_unsupported_variant() {
    let result = settings("any-provider", "grok-4.3", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_grok_keeps_reasoning_effort() {
    let result = settings("any-provider", "grok-4.3", effort("high"), None);
    assert_eq!(result.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_kimi_kimi_keeps_supported_variant() {
    let result = settings("any-provider", "kimi-k2.5", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_kimi_kimi_downgrades_unsupported_variant() {
    let result = settings("any-provider", "kimi-k2.5", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_kimi_kimi_drops_reasoning_effort() {
    let result = settings("any-provider", "kimi-k2.5", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_kimi_k2_keeps_supported_variant() {
    let result = settings("any-provider", "k2-v2", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_kimi_k2_downgrades_unsupported_variant() {
    let result = settings("any-provider", "k2-v2", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_kimi_k2_drops_reasoning_effort() {
    let result = settings("any-provider", "k2-v2", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_glm_keeps_supported_variant() {
    let result = settings("any-provider", "glm-5.2", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("max"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_glm_downgrades_unsupported_variant() {
    let result = settings("any-provider", "glm-5.2", variant("xhigh"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_glm_keeps_reasoning_effort() {
    let result = settings("any-provider", "glm-5.2", effort("high"), None);
    assert_eq!(result.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_minimax_keeps_supported_variant() {
    let result = settings("any-provider", "minimax-m2.5", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_minimax_downgrades_unsupported_variant() {
    let result = settings("any-provider", "minimax-m2.5", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_minimax_drops_reasoning_effort() {
    let result = settings("any-provider", "minimax-m2.5", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_deepseek_keeps_supported_variant() {
    let result = settings("any-provider", "deepseek-r2", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("max"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_deepseek_downgrades_unsupported_variant() {
    let result = settings("any-provider", "deepseek-r2", variant("xhigh"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_deepseek_keeps_reasoning_effort() {
    let result = settings("any-provider", "deepseek-r2", effort("high"), None);
    assert_eq!(result.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_mistral_keeps_supported_variant() {
    let result = settings("any-provider", "mistral-large-next", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_mistral_downgrades_unsupported_variant() {
    let result = settings("any-provider", "mistral-large-next", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_mistral_drops_reasoning_effort() {
    let result = settings("any-provider", "mistral-large-next", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_codestral_keeps_supported_variant() {
    let result = settings("any-provider", "codestral-2506", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_codestral_downgrades_unsupported_variant() {
    let result = settings("any-provider", "codestral-2506", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_codestral_drops_reasoning_effort() {
    let result = settings("any-provider", "codestral-2506", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_llama_keeps_supported_variant() {
    let result = settings("any-provider", "llama-4-maverick", variant("high"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn family_llama_downgrades_unsupported_variant() {
    let result = settings("any-provider", "llama-4-maverick", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn family_llama_drops_reasoning_effort() {
    let result = settings("any-provider", "llama-4-maverick", effort("high"), None);
    assert_eq!(result.reasoning_effort, None);
    assert_eq!(
        result.changes[0].reason,
        CompatibilityChangeReason::UnsupportedByModelFamily
    );
}

#[test]
fn gpt_5_keeps_xhigh_variant_and_reasoning_effort() {
    let desired = DesiredModelSettings {
        variant: Some("xhigh".to_string()),
        reasoning_effort: Some("xhigh".to_string()),
        ..Default::default()
    };
    assert_eq!(
        settings("openai", "gpt-5.4", desired, None),
        result(Some("xhigh"), Some("xhigh"), vec![])
    );
}

#[test]
fn github_copilot_gpt_5_high_tier_variants_downgrade_to_high() {
    for model_id in [
        "gpt-5.4",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        for requested in ["xhigh", "max"] {
            let capabilities = heuristic_capabilities("github-copilot", model_id);
            let desired = DesiredModelSettings {
                variant: Some(requested.to_string()),
                reasoning_effort: Some(requested.to_string()),
                ..Default::default()
            };

            assert_eq!(
                settings("github-copilot", model_id, desired, Some(capabilities)),
                result(
                    Some("high"),
                    Some("high"),
                    vec![
                        change(Variant, requested, Some("high"), UnsupportedByModelMetadata),
                        change(
                            ReasoningEffort,
                            requested,
                            Some("high"),
                            UnsupportedByModelMetadata
                        ),
                    ]
                ),
                "{model_id} {requested}"
            );
        }
    }
}

#[test]
fn glm_maps_generic_reasoning_effort_levels_to_zai_high_and_max_values() {
    for (requested, expected) in [
        ("low", "high"),
        ("medium", "high"),
        ("high", "high"),
        ("xhigh", "max"),
        ("max", "max"),
    ] {
        let result = settings("zai-coding-plan", "glm-5.2", effort(requested), None);

        assert_eq!(
            result.reasoning_effort.as_deref(),
            Some(expected),
            "{requested}"
        );
        let expected_changes = if requested == expected {
            vec![]
        } else {
            vec![change(
                ReasoningEffort,
                requested,
                Some(expected),
                UnsupportedByModelFamily,
            )]
        };
        assert_eq!(result.changes, expected_changes, "{requested}");
    }
}

#[test]
fn deepseek_keeps_canonical_high_and_max_reasoning_effort_values() {
    for reasoning_effort in ["high", "max"] {
        let result = settings(
            "openai-compatible",
            "deepseek-v4-pro",
            effort(reasoning_effort),
            None,
        );
        assert_eq!(result.reasoning_effort.as_deref(), Some(reasoning_effort));
        assert_eq!(result.changes, Vec::new());
    }
}

#[test]
fn deepseek_maps_generic_reasoning_effort_levels_to_canonical_api_values() {
    for (requested, expected) in [("low", "high"), ("medium", "high"), ("xhigh", "max")] {
        let result = settings(
            "openai-compatible",
            "deepseek-v4-pro",
            effort(requested),
            None,
        );
        assert_eq!(
            result.reasoning_effort.as_deref(),
            Some(expected),
            "{requested}"
        );
        assert_eq!(
            result.changes,
            vec![change(
                ReasoningEffort,
                requested,
                Some(expected),
                UnsupportedByModelFamily
            )]
        );
    }
}

#[test]
fn deepseek_maps_generic_reasoning_effort_levels_when_capabilities_come_from_heuristics() {
    let capabilities = heuristic_capabilities("openai-compatible", "deepseek-v4-pro");

    let result = settings(
        "openai-compatible",
        "deepseek-v4-pro",
        effort("xhigh"),
        Some(capabilities),
    );

    assert_eq!(result.reasoning_effort.as_deref(), Some("max"));
    assert_eq!(
        result.changes,
        vec![change(
            ReasoningEffort,
            "xhigh",
            Some("max"),
            UnsupportedByModelFamily
        )]
    );
}

#[test]
fn gpt_5_downgrades_unsupported_max_variant_to_xhigh() {
    assert_eq!(
        settings("openai", "gpt-5.4", variant("max"), None),
        result(
            Some("xhigh"),
            None,
            vec![change(
                Variant,
                "max",
                Some("xhigh"),
                UnsupportedByModelFamily
            )]
        )
    );
}

#[test]
fn gpt_5_keeps_none_reasoning_effort() {
    assert_eq!(
        settings("openai", "gpt-5.4", effort("none"), None),
        result(None, Some("none"), vec![])
    );
}

#[test]
fn gpt_5_keeps_minimal_reasoning_effort() {
    assert_eq!(
        settings("openai", "gpt-5.4", effort("minimal"), None),
        result(None, Some("minimal"), vec![])
    );
}

#[test]
fn o_series_keeps_none_reasoning_effort() {
    assert_eq!(
        settings("openai", "o3-mini", effort("none"), None),
        result(None, Some("none"), vec![])
    );
}

#[test]
fn o_series_downgrades_xhigh_reasoning_effort_to_high() {
    let result = settings("openai", "o3-mini", effort("xhigh"), None);
    assert_eq!(result.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        result.changes,
        vec![change(
            ReasoningEffort,
            "xhigh",
            Some("high"),
            UnsupportedByModelFamily
        )]
    );
}

#[test]
fn gpt_5_keeps_xhigh_reasoning_effort() {
    let result = settings("openai", "gpt-5.4", effort("xhigh"), None);
    assert_eq!(result.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn o_series_downgrades_unsupported_variant_to_high() {
    let result = settings("openai", "o3-mini", variant("max"), None);
    assert_eq!(result.variant.as_deref(), Some("high"));
    assert_eq!(
        result.changes,
        vec![change(
            Variant,
            "max",
            Some("high"),
            UnsupportedByModelFamily
        )]
    );
}

#[test]
fn drops_unsupported_temperature_when_capability_metadata_disables_it() {
    let result = settings(
        "openai",
        "gpt-5.4",
        temperature(0.7),
        supports_temperature(false),
    );
    assert_eq!(result.temperature, Some(None));
    assert_eq!(
        result.changes,
        vec![change(Temperature, "0.7", None, UnsupportedByModelMetadata)]
    );
}

#[test]
fn keeps_temperature_when_capability_metadata_explicitly_enables_it() {
    for (provider_id, model_id) in [("anthropic", "claude-opus-4-8"), ("openai", "o3")] {
        let result = settings(
            provider_id,
            model_id,
            temperature(0.7),
            supports_temperature(true),
        );
        assert_eq!(
            result.temperature,
            Some(Some(0.7)),
            "{provider_id}/{model_id}"
        );
        assert_eq!(result.changes, Vec::new(), "{provider_id}/{model_id}");
    }
}

#[test]
fn drops_temperature_for_claude_opus_4_8_without_capability_metadata() {
    let result = settings(
        "anthropic",
        "anthropic/claude-opus-4-8",
        temperature(0.1),
        None,
    );
    assert_eq!(result.temperature, Some(None));
    assert_eq!(
        result.changes,
        vec![change(Temperature, "0.1", None, UnsupportedByModelFamily)]
    );
}

#[test]
fn drops_temperature_for_a_reasoning_suffixed_claude_opus_4_8_id_with_no_capability_metadata() {
    let result = settings(
        "azure-anthropic",
        "azure-anthropic/claude-opus-4-8-thinking",
        temperature(0.1),
        None,
    );
    assert_eq!(result.temperature, Some(None));
    assert_eq!(
        result.changes,
        vec![change(Temperature, "0.1", None, UnsupportedByModelFamily)]
    );
}

#[test]
fn keeps_temperature_when_capability_metadata_explicitly_supports_it() {
    let result = settings(
        "azure-anthropic",
        "azure-anthropic/claude-opus-4-8-thinking",
        temperature(0.1),
        supports_temperature(true),
    );
    assert_eq!(result.temperature, Some(Some(0.1)));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn drops_temperature_for_reasoning_suffixed_o_series_models() {
    for model_id in ["openai/o3:high", "openai/o3(high)", "openai/o3 high"] {
        let result = settings("openai", model_id, temperature(0.1), None);
        assert_eq!(result.temperature, Some(None), "{model_id}");
        assert_eq!(
            result.changes,
            vec![change(Temperature, "0.1", None, UnsupportedByModelFamily)],
            "{model_id}"
        );
    }
}

#[test]
fn keeps_temperature_for_the_deep_research_exceptions() {
    for model_id in ["openai/o3-deep-research", "openai/o4-mini-deep-research"] {
        let result = settings("openai", model_id, temperature(0.1), None);
        assert_eq!(result.temperature, Some(Some(0.1)), "{model_id}");
        assert_eq!(result.changes, Vec::new(), "{model_id}");
    }
}

#[test]
fn drops_thinking_when_model_capabilities_say_it_is_unsupported() {
    let capabilities = Some(CompatibilityCapabilities {
        supports_thinking: Some(false),
        ..Default::default()
    });

    let result = settings("openai", "gpt-5.4", thinking_enabled(), capabilities);

    assert_eq!(result.thinking, Some(None));
    assert_eq!(
        result.changes,
        vec![change(
            Thinking,
            r#"{"type":"enabled","budgetTokens":4096}"#,
            None,
            UnsupportedByModelMetadata
        )]
    );
}

fn assert_thinking_dropped_by_heuristics(provider_id: &str, model_id: &str) {
    let capabilities = heuristic_capabilities(provider_id, model_id);

    let result = settings(
        provider_id,
        model_id,
        thinking_enabled(),
        Some(capabilities),
    );

    assert_eq!(result.thinking, Some(None));
    assert_eq!(result.changes[0].field, Thinking);
    assert_eq!(result.changes[0].reason, UnsupportedByModelMetadata);
}

#[test]
fn drops_thinking_for_minimax_m2_7_capabilities_resolved_from_heuristics() {
    assert_thinking_dropped_by_heuristics("volcengine", "minimax-m2.7");
}

#[test]
fn drops_thinking_for_non_thinking_kimi_k2_6_capabilities_resolved_from_heuristics() {
    assert_thinking_dropped_by_heuristics("volcengine", "kimi-k2.6");
}

#[test]
fn preserves_thinking_for_kimi_for_coding_k2p_model_ids_not_matched_by_generic_kimi_heuristic() {
    for model_id in ["k2p6", "k2-p6", "k2.p6"] {
        let capabilities = heuristic_capabilities("kimi-for-coding", model_id);

        let result = settings(
            "kimi-for-coding",
            model_id,
            thinking_enabled(),
            Some(capabilities),
        );

        assert_eq!(
            result.thinking,
            Some(Some(json!({ "type": "enabled", "budgetTokens": 4096 }))),
            "{model_id}"
        );
        assert_eq!(result.changes, Vec::new(), "{model_id}");
    }
}

#[test]
fn resolves_variant_for_k2p_models_via_kimi_thinking_heuristic_family() {
    for model_id in ["k2p5", "k2p6", "k2-p6", "k2.p6"] {
        let capabilities = heuristic_capabilities("kimi-for-coding", model_id);

        let result = settings(
            "kimi-for-coding",
            model_id,
            variant("high"),
            Some(capabilities),
        );

        assert_eq!(result.variant.as_deref(), Some("high"), "{model_id}");
        assert_eq!(result.changes, Vec::new(), "{model_id}");
    }
}

#[test]
fn detects_k2p_models_as_kimi_thinking_family_with_thinking_and_variants() {
    for model_id in ["k2p5", "k2p6", "k2-p6", "k2.p6"] {
        let capabilities = get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "kimi-for-coding",
            model_id,
            ..Default::default()
        });

        assert_eq!(capabilities.supports_thinking, Some(true), "{model_id}");
        assert_eq!(
            capabilities.family.as_deref(),
            Some("kimi-thinking"),
            "{model_id}"
        );
        assert_eq!(
            capabilities.variants,
            Some(strings(&["low", "medium", "high"])),
            "{model_id}"
        );
    }
}

#[test]
fn does_not_classify_kimi_p_style_ids_as_kimi_thinking() {
    let capabilities = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "kimi-for-coding",
        model_id: "kimi-p6",
        ..Default::default()
    });

    assert_ne!(capabilities.family.as_deref(), Some("kimi-thinking"));
    assert_ne!(capabilities.supports_thinking, Some(true));
}

fn max_tokens(value: f64) -> DesiredModelSettings {
    DesiredModelSettings {
        max_tokens: Some(value),
        ..Default::default()
    }
}

#[test]
fn clamps_max_tokens_to_the_model_output_limit() {
    let result = settings(
        "openai",
        "gpt-5.4",
        max_tokens(200_000.0),
        max_output(128_000.0),
    );
    assert_eq!(result.max_tokens, Some(Some(128_000.0)));
    assert_eq!(
        result.changes,
        vec![change(MaxTokens, "200000", Some("128000"), MaxOutputLimit)]
    );
}

#[test]
fn zero_max_output_tokens_preserves_max_tokens_unchanged() {
    let result = settings("openai", "gpt-5.4", max_tokens(200_000.0), max_output(0.0));
    assert_eq!(result.max_tokens, Some(Some(200_000.0)));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn negative_max_output_tokens_preserves_max_tokens_unchanged() {
    let result = settings("openai", "gpt-5.4", max_tokens(200_000.0), max_output(-1.0));
    assert_eq!(result.max_tokens, Some(Some(200_000.0)));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn zero_desired_max_tokens_is_dropped() {
    let result = settings("openai", "gpt-5.4", max_tokens(0.0), max_output(128_000.0));
    assert_eq!(result.max_tokens, Some(None));
    assert_eq!(result.changes, Vec::new());
}

#[test]
fn no_op_when_desired_settings_are_empty() {
    assert_eq!(
        settings(
            "anthropic",
            "claude-opus-4-7",
            DesiredModelSettings::default(),
            None
        ),
        result(None, None, vec![])
    );
}
