use indexmap::IndexMap;
use model_core::AliasSource;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilities;
use model_core::ModelCapabilitiesSnapshot;
use model_core::ResolutionMode;
use model_core::SnapshotSource;
use model_core::get_bundled_model_capabilities_snapshot;
use model_core::get_model_capabilities;
use pretty_assertions::assert_eq;

const FAST_RULE: &str = "openai-gpt-5.6-fast-service-tier-alias";

fn supplemental_only_snapshot() -> ModelCapabilitiesSnapshot {
    get_bundled_model_capabilities_snapshot(&ModelCapabilitiesSnapshot {
        generated_at: "test".to_string(),
        source_url: "test".to_string(),
        models: IndexMap::new(),
    })
}

fn capabilities(
    snapshot: &ModelCapabilitiesSnapshot,
    provider_id: &str,
    model_id: &str,
) -> ModelCapabilities {
    get_model_capabilities(GetModelCapabilitiesInput {
        provider_id,
        model_id,
        bundled_snapshot: Some(snapshot),
        ..Default::default()
    })
}

fn assert_fast_alias_backed(alias: &ModelCapabilities) {
    assert_eq!(
        alias.diagnostics.resolution_mode,
        ResolutionMode::AliasBacked
    );
    assert_eq!(
        alias.diagnostics.canonicalization.source,
        AliasSource::PatternAlias
    );
    assert_eq!(
        alias.diagnostics.canonicalization.rule_id.as_deref(),
        Some(FAST_RULE)
    );
    assert_eq!(alias.diagnostics.snapshot, SnapshotSource::BundledSnapshot);
}

#[test]
fn inherits_each_canonical_snapshot_entry_without_changing_the_requested_model_id() {
    let snapshot = supplemental_only_snapshot();
    for (alias_model_id, canonical_model_id) in [
        ("gpt-5.6-sol-fast", "gpt-5.6-sol"),
        ("gpt-5.6-terra-fast", "gpt-5.6-terra"),
        ("gpt-5.6-luna-fast", "gpt-5.6-luna"),
    ] {
        let canonical = capabilities(&snapshot, "openai", canonical_model_id);
        let alias = capabilities(&snapshot, "openai", alias_model_id);

        assert_eq!(alias.requested_model_id, alias_model_id);
        assert_eq!(alias.canonical_model_id, canonical_model_id);
        assert_eq!(alias.reasoning, canonical.reasoning);
        assert_eq!(alias.supports_temperature, canonical.supports_temperature);
        assert_eq!(alias.tool_call, canonical.tool_call);
        assert_eq!(alias.modalities, canonical.modalities);
        assert_eq!(alias.max_output_tokens, canonical.max_output_tokens);
        assert_fast_alias_backed(&alias);
        assert_eq!(alias.variants, canonical.variants);
        assert_eq!(alias.reasoning_efforts, canonical.reasoning_efforts);
    }
}

#[test]
fn keeps_canonical_and_unrelated_provider_behavior_unchanged() {
    let snapshot = supplemental_only_snapshot();

    let canonical = capabilities(&snapshot, "openai", "gpt-5.6-sol");
    let unrelated_provider = capabilities(&snapshot, "github-copilot", "gpt-5.6-sol-fast");
    let unrelated_suffix = capabilities(&snapshot, "openai", "gpt-5.6-sol-fast-preview");

    assert_eq!(canonical.requested_model_id, "gpt-5.6-sol");
    assert_eq!(canonical.canonical_model_id, "gpt-5.6-sol");
    assert_eq!(
        canonical.diagnostics.resolution_mode,
        ResolutionMode::SnapshotBacked
    );
    assert_eq!(unrelated_provider.requested_model_id, "gpt-5.6-sol-fast");
    assert_eq!(unrelated_provider.canonical_model_id, "gpt-5.6-sol-fast");
    assert_eq!(
        unrelated_provider.diagnostics.resolution_mode,
        ResolutionMode::HeuristicBacked
    );
    assert_eq!(
        unrelated_suffix.requested_model_id,
        "gpt-5.6-sol-fast-preview"
    );
    assert_eq!(
        unrelated_suffix.canonical_model_id,
        "gpt-5.6-sol-fast-preview"
    );
    assert_eq!(
        unrelated_suffix.diagnostics.resolution_mode,
        ResolutionMode::HeuristicBacked
    );
}

#[test]
fn inherits_canonical_capabilities_through_a_vercel_openai_subprovider_prefix() {
    let snapshot = supplemental_only_snapshot();

    let alias = capabilities(&snapshot, "vercel", "openai/gpt-5.6-sol-fast");

    assert_eq!(alias.requested_model_id, "openai/gpt-5.6-sol-fast");
    assert_eq!(alias.canonical_model_id, "gpt-5.6-sol");
    assert_eq!(alias.supports_temperature, Some(false));
    assert_fast_alias_backed(&alias);
}

#[test]
fn inherits_canonical_capabilities_for_suffixed_fast_aliases() {
    let snapshot = supplemental_only_snapshot();
    for (provider_id, model_id) in [
        ("openai", "gpt-5.6-sol-fast:high"),
        ("vercel", "openai/gpt-5.6-sol-fast:high"),
    ] {
        let alias = capabilities(&snapshot, provider_id, model_id);

        assert_eq!(alias.requested_model_id, model_id);
        assert_eq!(alias.canonical_model_id, "gpt-5.6-sol");
        assert_eq!(alias.supports_temperature, Some(false));
        assert_fast_alias_backed(&alias);
    }
}
