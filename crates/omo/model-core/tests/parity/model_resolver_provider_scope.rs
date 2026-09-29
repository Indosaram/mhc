use model_core::ExtendedModelResolutionInput;
use model_core::ModelResolutionProvenance;
use model_core::resolve_model_with_fallback;
use pretty_assertions::assert_eq;

use crate::support::LogCapture;
use crate::support::entry;
use crate::support::set;

#[test]
fn skips_same_name_models_from_other_providers_when_preferred_provider_unavailable() {
    let log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        fallback_chain: Some(vec![
            entry(&["zai-coding-plan"], "glm-5", None),
            entry(&["anthropic"], "claude-sonnet-4-6", None),
        ]),
        available_models: set(&["opencode/glm-5", "anthropic/claude-sonnet-4-6"]),
        system_default_model: Some("google/gemini-3.1-pro".to_string()),
        ..Default::default()
    };

    let resolved = resolve_model_with_fallback(&input).expect("expected model resolution result");

    assert_eq!(resolved.model, "anthropic/claude-sonnet-4-6");
    assert_eq!(resolved.source, ModelResolutionProvenance::ProviderFallback);
    assert!(
        !log.was_called_with_message(
            "Model resolved via fallback chain (cross-provider fuzzy match)"
        )
    );
}

#[test]
fn prefers_specified_provider_over_same_name_model_from_another_provider() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        fallback_chain: Some(vec![entry(&["zai-coding-plan"], "glm-5", None)]),
        available_models: set(&["zai-coding-plan/glm-5", "opencode/glm-5"]),
        system_default_model: Some("google/gemini-3.1-pro".to_string()),
        ..Default::default()
    };

    let resolved = resolve_model_with_fallback(&input).expect("expected model resolution result");

    assert_eq!(resolved.model, "zai-coding-plan/glm-5");
    assert_eq!(resolved.source, ModelResolutionProvenance::ProviderFallback);
}

#[test]
fn does_not_preserve_variant_from_an_unmatched_provider_scoped_entry() {
    let _log = LogCapture::install();
    let input = ExtendedModelResolutionInput {
        fallback_chain: Some(vec![entry(&["zai-coding-plan"], "glm-5", Some("high"))]),
        available_models: set(&["opencode/glm-5"]),
        system_default_model: Some("google/gemini-3.1-pro".to_string()),
        ..Default::default()
    };

    let resolved = resolve_model_with_fallback(&input).expect("expected model resolution result");

    assert_eq!(resolved.model, "google/gemini-3.1-pro");
    assert_eq!(resolved.source, ModelResolutionProvenance::SystemDefault);
    assert_eq!(resolved.variant, None);
}
