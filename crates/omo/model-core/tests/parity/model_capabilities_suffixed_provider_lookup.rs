use indexmap::IndexMap;
use model_core::CapabilitySource;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilitiesSnapshot;
use model_core::ModelCapabilitiesSnapshotEntry;
use model_core::get_model_capabilities;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::support::MetadataCache;

fn bare_model_only_cache(bare_model_id: &'static str, temperature: bool) -> MetadataCache {
    MetadataCache(vec![(
        bare_model_id,
        json!({ "id": bare_model_id, "temperature": temperature }),
    )])
}

fn with_cache(
    provider_id: &str,
    model_id: &str,
    cache: &MetadataCache,
) -> model_core::ModelCapabilities {
    get_model_capabilities(GetModelCapabilitiesInput {
        provider_id,
        model_id,
        provider_cache: Some(cache),
        ..Default::default()
    })
}

#[test]
fn colon_suffixed_request_still_resolves_bare_provider_metadata() {
    let cache = bare_model_only_cache("o3", true);

    let capabilities = with_cache("openai", "o3:high", &cache);

    assert_eq!(capabilities.supports_temperature, Some(true));
    assert_eq!(
        capabilities.diagnostics.supports_temperature,
        CapabilitySource::Runtime
    );
}

#[test]
fn parenthesized_or_spaced_suffix_still_resolves_bare_provider_metadata() {
    let cache = bare_model_only_cache("o3", true);

    let parenthesized = with_cache("openai", "o3(high)", &cache);
    let spaced = with_cache("openai", "o3 high", &cache);

    assert_eq!(parenthesized.supports_temperature, Some(true));
    assert_eq!(spaced.supports_temperature, Some(true));
}

#[test]
fn exact_suffixed_entry_wins_over_the_bare_model() {
    let cache = MetadataCache(vec![
        ("o3:high", json!({ "id": "o3:high", "temperature": false })),
        ("o3", json!({ "id": "o3", "temperature": true })),
    ]);

    let capabilities = with_cache("openai", "o3:high", &cache);

    assert_eq!(capabilities.supports_temperature, Some(false));
}

#[test]
fn no_matching_entry_in_either_form_leaves_temperature_unresolved() {
    let cache = bare_model_only_cache("gpt-4o", true);

    let capabilities = with_cache("openai", "o3:high", &cache);

    assert_eq!(capabilities.supports_temperature, None);
}

#[test]
fn same_provider_prefixed_suffixed_id_resolves_bare_cache_entry() {
    let cache = bare_model_only_cache("future-model", false);

    let capabilities = with_cache("custom", "custom/future-model:high", &cache);

    assert_eq!(capabilities.supports_temperature, Some(false));
    assert_eq!(
        capabilities.diagnostics.supports_temperature,
        CapabilitySource::Runtime
    );
}

#[test]
fn different_provider_prefix_is_not_stripped() {
    let cache = bare_model_only_cache("future-model", false);

    let capabilities = with_cache("custom", "other/future-model:high", &cache);

    assert_eq!(capabilities.supports_temperature, None);
}

fn snapshot(models: &[(&str, &str, bool)]) -> ModelCapabilitiesSnapshot {
    ModelCapabilitiesSnapshot {
        generated_at: "2026-08-05T00:00:00.000Z".to_string(),
        source_url: "https://models.dev/api.json".to_string(),
        models: models
            .iter()
            .map(|(key, id, temperature)| {
                (
                    (*key).to_string(),
                    ModelCapabilitiesSnapshotEntry {
                        id: (*id).to_string(),
                        temperature: Some(*temperature),
                        ..Default::default()
                    },
                )
            })
            .collect::<IndexMap<_, _>>(),
    }
}

#[test]
fn snapshot_backed_bare_model_resolves_same_provider_prefixed_suffixed_id() {
    let bundled_snapshot = snapshot(&[("gpt-5.6-sol", "gpt-5.6-sol", false)]);

    let capabilities = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "openai",
        model_id: "openai/gpt-5.6-sol:high",
        bundled_snapshot: Some(&bundled_snapshot),
        ..Default::default()
    });

    assert_eq!(capabilities.supports_temperature, Some(false));
    assert_eq!(
        capabilities.diagnostics.supports_temperature,
        CapabilitySource::BundledSnapshot
    );
}

#[test]
fn provider_specific_snapshot_metadata_wins_over_conflicting_bare_entry() {
    let bundled_snapshot = snapshot(&[
        ("anthropic/claude-opus-4.8", "claude-opus-4.8", true),
        ("claude-opus-4.8", "claude-opus-4.8", false),
    ]);
    for model_id in ["claude-opus-4.8:high", "anthropic/claude-opus-4.8:high"] {
        let capabilities = get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "anthropic",
            model_id,
            bundled_snapshot: Some(&bundled_snapshot),
            ..Default::default()
        });

        assert_eq!(capabilities.supports_temperature, Some(true), "{model_id}");
        assert_eq!(
            capabilities.diagnostics.supports_temperature,
            CapabilitySource::BundledSnapshot,
            "{model_id}"
        );
    }
}
