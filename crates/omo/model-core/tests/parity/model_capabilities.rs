use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::AliasSource;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::CapabilitySource;
use model_core::FamilySource;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilities;
use model_core::ModelCapabilitiesSnapshot;
use model_core::ModelCapabilitiesSnapshotEntry;
use model_core::ReasoningEffortsSource;
use model_core::ResolutionMode;
use model_core::SnapshotLimit;
use model_core::SnapshotModalities;
use model_core::SnapshotSource;
use model_core::VariantsSource;
use model_core::get_model_capabilities;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use crate::support::bundled_snapshot;
use crate::support::strings;

fn snapshot_entry(
    id: &str,
    family: &str,
    temperature: bool,
    modalities: Option<(&[&str], &[&str])>,
    limit: Option<(i64, i64)>,
    tool_call: Option<bool>,
) -> (String, ModelCapabilitiesSnapshotEntry) {
    (
        id.to_string(),
        ModelCapabilitiesSnapshotEntry {
            id: id.to_string(),
            family: Some(family.to_string()),
            reasoning: Some(true),
            temperature: Some(temperature),
            tool_call,
            modalities: modalities.map(|(input, output)| SnapshotModalities {
                input: Some(strings(input)),
                output: Some(strings(output)),
            }),
            limit: limit.map(|(context, output)| SnapshotLimit {
                context: Some(context),
                input: None,
                output: Some(output),
            }),
        },
    )
}

fn fixture_snapshot() -> ModelCapabilitiesSnapshot {
    ModelCapabilitiesSnapshot {
        generated_at: "2026-03-25T00:00:00.000Z".to_string(),
        source_url: "https://models.dev/api.json".to_string(),
        models: [
            snapshot_entry(
                "claude-opus-4-7",
                "claude-opus",
                true,
                Some((&["text", "image", "pdf"], &["text"])),
                Some((1_000_000, 128_000)),
                Some(true),
            ),
            snapshot_entry(
                "gemini-3.1-pro",
                "gemini",
                true,
                Some((&["text", "image"], &["text"])),
                Some((1_000_000, 65_000)),
                None,
            ),
            snapshot_entry(
                "gpt-5.4",
                "gpt",
                false,
                Some((&["text", "image", "pdf"], &["text"])),
                Some((1_050_000, 128_000)),
                None,
            ),
            snapshot_entry("minimax-m2.7", "minimax", true, None, None, None),
        ]
        .into_iter()
        .collect(),
    }
}

fn caps(
    provider_id: &str,
    model_id: &str,
    runtime_model: Option<&Value>,
    snapshot: Option<&ModelCapabilitiesSnapshot>,
) -> ModelCapabilities {
    get_model_capabilities(GetModelCapabilitiesInput {
        provider_id,
        model_id,
        runtime_model,
        bundled_snapshot: snapshot,
        ..Default::default()
    })
}

fn assert_alias_backed(result: &ModelCapabilities, rule_id: &str) {
    assert_eq!(
        result.diagnostics.resolution_mode,
        ResolutionMode::AliasBacked
    );
    assert_eq!(
        result.diagnostics.canonicalization.source,
        AliasSource::PatternAlias
    );
    assert_eq!(
        result.diagnostics.canonicalization.rule_id.as_deref(),
        Some(rule_id)
    );
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::BundledSnapshot);
}

#[test]
fn uses_runtime_metadata_before_snapshot_data() {
    let snapshot = fixture_snapshot();
    let runtime_model = json!({ "variants": { "low": {}, "medium": {}, "high": {} } });

    let result = caps(
        "anthropic",
        "claude-opus-4-7",
        Some(&runtime_model),
        Some(&snapshot),
    );

    assert_eq!(result.canonical_model_id, "claude-opus-4-7");
    assert_eq!(result.family.as_deref(), Some("claude-opus"));
    assert_eq!(result.variants, Some(strings(&["low", "medium", "high"])));
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.supports_temperature, Some(true));
    assert_eq!(result.max_output_tokens, Some(128_000));
    assert_eq!(result.tool_call, Some(true));
    assert_eq!(
        result.diagnostics.resolution_mode,
        ResolutionMode::SnapshotBacked
    );
    assert_eq!(
        result.diagnostics.canonicalization.source,
        AliasSource::Canonical
    );
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::BundledSnapshot);
    assert_eq!(result.diagnostics.variants, VariantsSource::Runtime);
}

#[test]
fn reads_structured_runtime_capabilities_from_the_sdk_v2_shape() {
    let snapshot = fixture_snapshot();
    let runtime_model = json!({
        "capabilities": {
            "reasoning": true,
            "temperature": false,
            "toolcall": true,
            "input": { "text": true, "image": true },
            "output": { "text": true },
        },
    });

    let result = caps("openai", "gpt-5.4", Some(&runtime_model), Some(&snapshot));

    assert_eq!(result.canonical_model_id, "gpt-5.4");
    assert_eq!(result.reasoning, Some(true));
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.supports_temperature, Some(false));
    assert_eq!(result.tool_call, Some(true));
    assert_eq!(
        result.modalities,
        Some(SnapshotModalities {
            input: Some(strings(&["text", "image"])),
            output: Some(strings(&["text"])),
        })
    );
    assert_eq!(
        result.diagnostics.resolution_mode,
        ResolutionMode::SnapshotBacked
    );
    assert_eq!(result.diagnostics.reasoning, CapabilitySource::Runtime);
    assert_eq!(
        result.diagnostics.supports_thinking,
        CapabilitySource::Runtime
    );
    assert_eq!(result.diagnostics.tool_call, CapabilitySource::Runtime);
}

#[test]
fn respects_root_level_thinking_flags_when_providers_do_not_nest_them_under_capabilities() {
    let snapshot = fixture_snapshot();
    let runtime_model = json!({ "supportsThinking": true });

    let result = caps(
        "custom-proxy",
        "gpt-5.4",
        Some(&runtime_model),
        Some(&snapshot),
    );

    assert_eq!(result.canonical_model_id, "gpt-5.4");
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(
        result.diagnostics.supports_thinking,
        CapabilitySource::Runtime
    );
}

#[test]
fn accepts_runtime_variant_arrays_without_corrupting_them_into_numeric_keys() {
    let snapshot = fixture_snapshot();
    let runtime_model = json!({ "variants": ["low", "medium", "high", "xhigh"] });

    let result = caps("openai", "gpt-5.4", Some(&runtime_model), Some(&snapshot));

    assert_eq!(
        result.variants,
        Some(strings(&["low", "medium", "high", "xhigh"]))
    );
}

#[test]
fn normalizes_the_legacy_claude_opus_thinking_alias_before_snapshot_lookup() {
    let snapshot = fixture_snapshot();

    let result = caps(
        "anthropic",
        "claude-opus-4-7-thinking",
        None,
        Some(&snapshot),
    );

    assert_eq!(result.canonical_model_id, "claude-opus-4-7");
    assert_eq!(result.family.as_deref(), Some("claude-opus"));
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.supports_temperature, Some(true));
    assert_eq!(result.max_output_tokens, Some(128_000));
    assert_alias_backed(&result, "claude-thinking-legacy-alias");
}

#[test]
fn maps_local_gemini_aliases_to_canonical_models_dev_entries() {
    let snapshot = fixture_snapshot();

    let result = caps("google", "gemini-3.1-pro-high", None, Some(&snapshot));

    assert_eq!(result.canonical_model_id, "gemini-3.1-pro");
    assert_eq!(result.family.as_deref(), Some("gemini"));
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.supports_temperature, Some(true));
    assert_eq!(result.max_output_tokens, Some(65_000));
    assert_alias_backed(&result, "gemini-3.1-pro-tier-alias");
}

#[test]
fn canonicalizes_provider_prefixed_gemini_aliases_without_changing_the_transport_facing_request() {
    let snapshot = fixture_snapshot();

    let result = caps(
        "google",
        "google/gemini-3.1-pro-high",
        None,
        Some(&snapshot),
    );

    assert_eq!(result.requested_model_id, "google/gemini-3.1-pro-high");
    assert_eq!(result.canonical_model_id, "gemini-3.1-pro");
    assert_eq!(result.family.as_deref(), Some("gemini"));
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.supports_temperature, Some(true));
    assert_eq!(result.max_output_tokens, Some(65_000));
    assert_alias_backed(&result, "gemini-3.1-pro-tier-alias");
}

#[test]
fn canonicalizes_provider_prefixed_claude_thinking_aliases_to_bare_snapshot_ids() {
    let snapshot = fixture_snapshot();

    let result = caps(
        "anthropic",
        "anthropic/claude-opus-4-7-thinking",
        None,
        Some(&snapshot),
    );

    assert_eq!(
        result.requested_model_id,
        "anthropic/claude-opus-4-7-thinking"
    );
    assert_eq!(result.canonical_model_id, "claude-opus-4-7");
    assert_eq!(result.family.as_deref(), Some("claude-opus"));
    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.supports_temperature, Some(true));
    assert_eq!(result.max_output_tokens, Some(128_000));
    assert_alias_backed(&result, "claude-thinking-legacy-alias");
}

#[test]
fn prefers_runtime_models_dev_cache_over_bundled_snapshot() {
    let bundled = fixture_snapshot();
    let mut runtime_snapshot = bundled.clone();
    if let Some(entry) = runtime_snapshot.models.get_mut("gpt-5.4") {
        entry.limit = Some(SnapshotLimit {
            context: Some(1_050_000),
            input: None,
            output: Some(64_000),
        });
    }

    let result = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "openai",
        model_id: "gpt-5.4",
        bundled_snapshot: Some(&bundled),
        runtime_snapshot: Some(&runtime_snapshot),
        ..Default::default()
    });

    assert_eq!(result.canonical_model_id, "gpt-5.4");
    assert_eq!(result.max_output_tokens, Some(64_000));
    assert_eq!(result.supports_temperature, Some(false));
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::RuntimeSnapshot);
    assert_eq!(
        result.diagnostics.max_output_tokens,
        CapabilitySource::RuntimeSnapshot
    );
    assert_eq!(
        result.diagnostics.supports_temperature,
        CapabilitySource::RuntimeSnapshot
    );
}

fn assert_openai_reasoning_heuristic(result: &ModelCapabilities) {
    assert_eq!(result.family.as_deref(), Some("openai-reasoning"));
    assert_eq!(result.variants, Some(strings(&["low", "medium", "high"])));
    assert_eq!(
        result.reasoning_efforts,
        Some(strings(&["none", "minimal", "low", "medium", "high"]))
    );
    assert_eq!(
        result.diagnostics.resolution_mode,
        ResolutionMode::HeuristicBacked
    );
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::None);
    assert_eq!(result.diagnostics.family, FamilySource::Heuristic);
}

#[test]
fn falls_back_to_heuristic_family_rules_when_no_snapshot_entry_exists() {
    let snapshot = fixture_snapshot();

    let result = caps("openai", "o3-mini", None, Some(&snapshot));

    assert_eq!(result.canonical_model_id, "o3-mini");
    assert_openai_reasoning_heuristic(&result);
    assert_eq!(
        result.diagnostics.reasoning_efforts,
        ReasoningEffortsSource::Heuristic
    );
}

#[test]
fn exposes_glm_max_reasoning_effort_through_heuristic_capabilities() {
    let snapshot = fixture_snapshot();

    let result = caps("zai-coding-plan", "glm-5.2", None, Some(&snapshot));

    assert_eq!(result.canonical_model_id, "glm-5.2");
    assert_eq!(result.family.as_deref(), Some("glm"));
    assert_eq!(
        result.variants,
        Some(strings(&["low", "medium", "high", "max"]))
    );
    assert_eq!(result.reasoning_efforts, Some(strings(&["high", "max"])));
    assert_eq!(
        result.diagnostics.resolution_mode,
        ResolutionMode::HeuristicBacked
    );
    assert_eq!(result.diagnostics.family, FamilySource::Heuristic);
    assert_eq!(result.diagnostics.variants, VariantsSource::Heuristic);
    assert_eq!(
        result.diagnostics.reasoning_efforts,
        ReasoningEffortsSource::Heuristic
    );
}

#[test]
fn prefers_snapshot_reasoning_over_heuristic_supports_thinking_for_minimax_m2_7() {
    let snapshot = fixture_snapshot();

    let result = caps("volcengine", "minimax-m2.7", None, Some(&snapshot));

    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(
        result.diagnostics.supports_thinking,
        CapabilitySource::BundledSnapshot
    );
}

#[test]
fn marks_non_thinking_kimi_k2_6_as_not_supporting_thinking() {
    let snapshot = fixture_snapshot();

    let result = caps("volcengine", "kimi-k2.6", None, Some(&snapshot));

    assert_eq!(result.supports_thinking, Some(false));
    assert_eq!(
        result.diagnostics.supports_thinking,
        CapabilitySource::Heuristic
    );
}

#[test]
fn keeps_thinking_flavored_kimi_k2_6_models_as_supporting_thinking() {
    let snapshot = fixture_snapshot();

    let result = caps("volcengine", "kimi-k2.6-thinking", None, Some(&snapshot));

    assert_eq!(result.supports_thinking, Some(true));
    assert_eq!(result.family.as_deref(), Some("kimi-thinking"));
    assert_eq!(
        result.diagnostics.supports_thinking,
        CapabilitySource::Heuristic
    );
}

#[test]
fn detects_prefixed_o_series_model_ids_through_the_heuristic_fallback() {
    let snapshot = fixture_snapshot();

    let result = caps("azure-openai", "openai/o3-mini", None, Some(&snapshot));

    assert_eq!(result.requested_model_id, "openai/o3-mini");
    assert_eq!(result.canonical_model_id, "o3-mini");
    assert_openai_reasoning_heuristic(&result);
}

#[test]
fn keeps_every_built_in_omo_requirement_model_snapshot_backed() {
    let snapshot = bundled_snapshot();
    let mut requirement_models: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for requirement in AGENT_MODEL_REQUIREMENTS
        .values()
        .chain(CATEGORY_MODEL_REQUIREMENTS.values())
    {
        for entry in &requirement.fallback_chain {
            let provider = entry
                .providers
                .first()
                .cloned()
                .unwrap_or_else(|| "test-provider".to_string());
            requirement_models
                .entry(entry.model.clone())
                .or_insert(provider);
        }
    }

    for (model_id, provider_id) in &requirement_models {
        let result = caps(provider_id, model_id, None, Some(&snapshot));

        assert!(
            matches!(
                result.diagnostics.resolution_mode,
                ResolutionMode::SnapshotBacked
                    | ResolutionMode::AliasBacked
                    | ResolutionMode::Unknown
            ),
            "{model_id}: {:?}",
            result.diagnostics.resolution_mode
        );
        assert!(
            matches!(
                result.diagnostics.snapshot,
                SnapshotSource::BundledSnapshot | SnapshotSource::None
            ),
            "{model_id}"
        );
    }
}

#[test]
fn prefers_snapshot_reasoning_over_heuristic_supports_thinking_false() {
    let snapshot = ModelCapabilitiesSnapshot {
        generated_at: "test".to_string(),
        source_url: "test".to_string(),
        models: [snapshot_entry(
            "kimi-k2.5",
            "kimi",
            true,
            Some((&["text"], &["text"])),
            Some((262_144, 32_768)),
            None,
        )]
        .into_iter()
        .collect(),
    };

    let capabilities = caps("moonshotai", "kimi-k2.5", None, Some(&snapshot));

    assert_eq!(capabilities.supports_thinking, Some(true));
    assert_eq!(
        capabilities.diagnostics.supports_thinking,
        CapabilitySource::BundledSnapshot
    );
}

#[test]
fn prefers_runtime_thinking_over_heuristic_supports_thinking_false() {
    let runtime_model = json!({ "reasoning": true });

    let capabilities = caps("moonshotai", "kimi-k2.5", Some(&runtime_model), None);

    assert_eq!(capabilities.supports_thinking, Some(true));
    assert_eq!(
        capabilities.diagnostics.supports_thinking,
        CapabilitySource::Runtime
    );
}
