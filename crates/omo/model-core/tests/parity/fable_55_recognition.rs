//! Translation of `fable-55-recognition.test.ts`.

use std::collections::HashMap;

use model_core::CapabilitySource;
use model_core::ContextLimitEnv;
use model_core::ContextLimitModelCacheState;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilitiesSnapshot;
use model_core::SnapshotSource;
use model_core::get_bundled_model_capabilities_snapshot;
use model_core::get_model_capabilities;
use model_core::is_claude_fable_or_mythos_model;
use model_core::is_claude_fable5_model;
use model_core::is_claude_opus47_or_later_model;
use model_core::resolve_actual_context_limit_with_env;
use pretty_assertions::assert_eq;
use serde_json::json;

const MODEL: &str = "claude-fable-5-5";

fn empty_bundled() -> ModelCapabilitiesSnapshot {
    get_bundled_model_capabilities_snapshot(&ModelCapabilitiesSnapshot::default())
}

#[test]
fn recognizes_the_family_without_depending_on_a_catalog_release() {
    for selector in [
        MODEL.to_string(),
        format!("anthropic/{MODEL}"),
        "github-copilot/claude-fable-5.5".to_string(),
    ] {
        assert!(is_claude_fable5_model(&selector), "{selector}");
        assert!(is_claude_fable_or_mythos_model(&selector), "{selector}");
        assert!(is_claude_opus47_or_later_model(&selector), "{selector}");
    }
}

#[test]
fn registers_the_provisional_model_without_inventing_launch_capabilities() {
    let snapshot = empty_bundled();

    let result = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "anthropic",
        model_id: MODEL,
        bundled_snapshot: Some(&snapshot),
        ..Default::default()
    });

    assert_eq!(result.family.as_deref(), Some("claude-fable"));
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::BundledSnapshot);
    assert_eq!(result.max_output_tokens, None);
    assert_eq!(result.tool_call, None);
    assert_eq!(result.modalities, None);
}

#[test]
fn uses_live_metadata_instead_of_provisional_unknown_limits() {
    let snapshot = empty_bundled();
    let runtime_model =
        json!({ "limit": { "output": 12345 }, "capabilities": { "toolcall": true } });

    let result = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "anthropic",
        model_id: MODEL,
        bundled_snapshot: Some(&snapshot),
        runtime_model: Some(&runtime_model),
        ..Default::default()
    });

    assert_eq!(result.max_output_tokens, Some(12345));
    assert_eq!(result.tool_call, Some(true));
    assert_eq!(
        result.diagnostics.max_output_tokens,
        CapabilitySource::Runtime
    );
}

#[test]
fn does_not_clamp_a_live_context_cache_to_an_unverified_anthropic_default() {
    let state = ContextLimitModelCacheState {
        anthropic_context_1m_enabled: false,
        model_context_limits_cache: Some(HashMap::from([(format!("anthropic/{MODEL}"), 345_678)])),
    };

    let result = resolve_actual_context_limit_with_env(
        "anthropic",
        MODEL,
        Some(&state),
        ContextLimitEnv::default(),
    );

    assert_eq!(result, Some(345_678));
}

#[test]
fn leaves_the_context_unknown_until_the_launch_listing_supplies_it() {
    let state = ContextLimitModelCacheState::default();
    assert_eq!(
        resolve_actual_context_limit_with_env(
            "anthropic",
            MODEL,
            Some(&state),
            ContextLimitEnv::default()
        ),
        None
    );
}
