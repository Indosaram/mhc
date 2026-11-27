//! Translation of `fable-52-recognition.test.ts`.

use crate::support::strings;
use model_core::{
    ContextLimitEnv, ContextLimitModelCacheState, GetModelCapabilitiesInput,
    ModelCapabilitiesSnapshot, SnapshotModalities, SnapshotSource,
    get_bundled_model_capabilities_snapshot, get_model_capabilities,
    is_claude_fable_or_mythos_model, is_claude_fable5_model, is_claude_opus47_or_later_model,
    resolve_actual_context_limit_with_env, transform_model_for_provider,
};
use std::collections::HashMap;

const MODEL: &str = "claude-fable-5-2";

#[test]
fn recognizes_fable_and_mythos_across_provider_spellings() {
    assert_eq!(model_core::latest_models::CLAUDE_FABLE, MODEL);
    for selector in [
        MODEL,
        "anthropic/claude-fable-5-2",
        "github-copilot/claude-fable-5.2",
        "anthropic.claude-fable-5-2",
        "us.anthropic.claude-fable-5-2",
    ] {
        assert!(is_claude_fable5_model(selector));
        assert!(is_claude_fable_or_mythos_model(selector));
        assert!(is_claude_opus47_or_later_model(selector));
    }
    assert!(is_claude_fable_or_mythos_model("claude-mythos-5-2"));
    assert_eq!(
        transform_model_for_provider("github-copilot", MODEL),
        "claude-fable-5.2"
    );
    assert_eq!(transform_model_for_provider("anthropic", MODEL), MODEL);
}

#[test]
fn supplies_released_capabilities_without_a_generated_row() {
    let snapshot = get_bundled_model_capabilities_snapshot(&ModelCapabilitiesSnapshot::default());
    assert!(!snapshot.models.contains_key("claude-fable-5-5"));
    let caps = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "anthropic",
        model_id: MODEL,
        bundled_snapshot: Some(&snapshot),
        ..Default::default()
    });
    assert_eq!(caps.diagnostics.snapshot, SnapshotSource::BundledSnapshot);
    assert_eq!(caps.family.as_deref(), Some("claude-fable"));
    assert_eq!(caps.max_output_tokens, Some(128_000));
    assert_eq!(caps.tool_call, Some(true));
    assert_eq!(
        caps.modalities,
        Some(SnapshotModalities {
            input: Some(strings(&["text", "image", "pdf"])),
            output: Some(strings(&["text"]))
        })
    );
}

#[test]
fn only_ga_spellings_get_one_million_without_opt_in() {
    for (provider, model, expected) in [
        ("anthropic", MODEL, 1_000_000),
        ("anthropic", "claude-fable-5.2", 1_000_000),
        ("anthropic", "claude-fable-5-2-20261109", 1_000_000),
        (
            "google-vertex-anthropic",
            "claude-fable-5-2@default",
            1_000_000,
        ),
        (
            "aws-bedrock-anthropic",
            "anthropic.claude-fable-5-2",
            1_000_000,
        ),
        (
            "aws-bedrock-anthropic",
            "us.anthropic.claude-fable-5-2",
            1_000_000,
        ),
        ("anthropic", "claude-mythos-5-2", 1_000_000),
        ("anthropic", "claude-fable-5-20", 200_000),
        ("anthropic", "claude-fable-5-2-2026110", 200_000),
        ("anthropic", "claude-fable-5-21", 200_000),
        ("anthropic", "claude-opus-5-2", 200_000),
        ("anthropic", "claude-fable-5-1-20260601", 200_000),
        (
            "aws-bedrock-anthropic",
            "anthropic.claude-fable-5-20",
            200_000,
        ),
    ] {
        assert_eq!(
            resolve_actual_context_limit_with_env(
                provider,
                model,
                None,
                ContextLimitEnv::default()
            ),
            Some(expected),
            "{provider}/{model}"
        );
    }
}

#[test]
fn other_providers_keep_their_listed_limit() {
    let state = ContextLimitModelCacheState {
        model_context_limits_cache: Some(HashMap::from([(
            format!("opengateway/{MODEL}"),
            345_678,
        )])),
        ..Default::default()
    };
    assert_eq!(
        resolve_actual_context_limit_with_env(
            "opengateway",
            MODEL,
            Some(&state),
            ContextLimitEnv::default()
        ),
        Some(345_678)
    );
}
