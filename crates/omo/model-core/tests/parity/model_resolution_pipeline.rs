use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::FABLE_55_LAUNCH_ID;
use model_core::FABLE_55_RELEASED;
use model_core::ModelResolutionProvenance;
use model_core::ModelResolutionRequest;
use model_core::PipelineModelResolutionResult;
use model_core::ResolutionConstraints;
use model_core::ResolutionIntent;
use model_core::ResolutionPolicy;
use model_core::resolve_model_pipeline;
use pretty_assertions::assert_eq;

use crate::support::entry;
use crate::support::set;

/// The Rust result type has a fixed field set, so "no explicitUserConfig field" is structural;
/// the whole-object comparison below is the behavioural half of the TS test.
#[test]
fn does_not_return_unused_explicit_user_config_metadata_in_override_result() {
    let result = resolve_model_pipeline(&ModelResolutionRequest {
        intent: ResolutionIntent {
            user_model: Some("openai/gpt-5.5".to_string()),
            ..Default::default()
        },
        ..Default::default()
    });

    assert_eq!(
        result,
        Some(PipelineModelResolutionResult {
            model: "openai/gpt-5.5".to_string(),
            provenance: ModelResolutionProvenance::Override,
            variant: None,
            attempted: None,
            reason: None,
        })
    );
}

#[test]
fn does_not_resolve_provider_fallback_entries_through_a_different_provider_with_the_same_model_name()
 {
    let mut request = ModelResolutionRequest {
        policy: ResolutionPolicy {
            fallback_chain: Some(vec![entry(&["anthropic"], "claude-opus-4-7", Some("max"))]),
            system_default_model: Some("openai/gpt-5.5".to_string()),
        },
        ..Default::default()
    };
    request.constraints.available_models = set(&["other/claude-opus-4-7"]);

    let result = resolve_model_pipeline(&request).expect("resolved");

    assert_eq!(result.model, "openai/gpt-5.5");
    assert_eq!(result.provenance, ModelResolutionProvenance::SystemDefault);
}

#[test]
fn strips_matching_provider_prefix_before_transforming_fallback_model() {
    let mut request = ModelResolutionRequest {
        policy: ResolutionPolicy {
            fallback_chain: Some(vec![entry(
                &["openrouter"],
                "openrouter/qwen3.5-plus",
                None,
            )]),
            system_default_model: Some("other/default".to_string()),
        },
        ..Default::default()
    };
    request.constraints.available_models = set(&["openrouter/qwen3.5-plus"]);

    let result = resolve_model_pipeline(&request).expect("resolved");

    assert_eq!(result.model, "openrouter/qwen3.5-plus");
    assert_eq!(
        result.provenance,
        ModelResolutionProvenance::ProviderFallback
    );
}

fn user_model_request(user_model: &str) -> ModelResolutionRequest {
    ModelResolutionRequest {
        intent: ResolutionIntent {
            user_model: Some(user_model.to_string()),
            ..Default::default()
        },
        policy: ResolutionPolicy {
            fallback_chain: Some(vec![entry(
                &["openai", "vercel"],
                "gpt-5.6-sol",
                Some("high"),
            )]),
            system_default_model: None,
        },
        ..Default::default()
    }
}

#[test]
fn inherits_the_fallback_variant_for_an_explicit_matching_user_model() {
    let result =
        resolve_model_pipeline(&user_model_request("openai/gpt-5.6-sol")).expect("resolved");

    assert_eq!(result.model, "openai/gpt-5.6-sol");
    assert_eq!(result.provenance, ModelResolutionProvenance::Override);
    assert_eq!(result.variant.as_deref(), Some("high"));
}

#[test]
fn inherits_the_fallback_variant_for_an_explicit_transformed_gateway_model() {
    let result =
        resolve_model_pipeline(&user_model_request("vercel/openai/gpt-5.6-sol")).expect("resolved");

    assert_eq!(result.model, "vercel/openai/gpt-5.6-sol");
    assert_eq!(result.provenance, ModelResolutionProvenance::Override);
    assert_eq!(result.variant.as_deref(), Some("high"));
}

// The provisional Fable 5.5 id must never reach live routing before release: a cold provider-models
// cache with only anthropic connected gives no proof the model exists.
fn launch_expected() -> String {
    if FABLE_55_RELEASED {
        format!("anthropic/{FABLE_55_LAUNCH_ID}")
    } else {
        "anthropic/claude-fable-5-1".to_string()
    }
}

fn cold_anthropic() -> ResolutionConstraints {
    ResolutionConstraints {
        available_models: set(&[]),
        connected_providers: Some(vec!["anthropic".to_string()]),
    }
}

#[test]
fn launch_gated_metis_and_prometheus_resolve_past_the_launch_rung() {
    for agent in ["metis", "prometheus"] {
        let result = resolve_model_pipeline(&ModelResolutionRequest {
            constraints: cold_anthropic(),
            policy: ResolutionPolicy {
                fallback_chain: Some(AGENT_MODEL_REQUIREMENTS[agent].fallback_chain.clone()),
                system_default_model: None,
            },
            ..Default::default()
        });

        assert_eq!(
            result.map(|result| result.model),
            Some(launch_expected()),
            "{agent}"
        );
    }
}

#[test]
fn launch_gated_artistry_category_default_falls_through_to_its_chain() {
    let result = resolve_model_pipeline(&ModelResolutionRequest {
        intent: ResolutionIntent {
            category_default_model: Some(format!("anthropic/{FABLE_55_LAUNCH_ID}")),
            ..Default::default()
        },
        constraints: cold_anthropic(),
        policy: ResolutionPolicy {
            fallback_chain: Some(
                CATEGORY_MODEL_REQUIREMENTS["artistry"]
                    .fallback_chain
                    .clone(),
            ),
            system_default_model: None,
        },
    });

    assert_eq!(result.map(|result| result.model), Some(launch_expected()));
}

#[test]
fn launch_gated_chain_entries_are_skipped_on_a_cold_cache() {
    let result = resolve_model_pipeline(&ModelResolutionRequest {
        constraints: cold_anthropic(),
        policy: ResolutionPolicy {
            fallback_chain: Some(vec![
                entry(&["anthropic"], FABLE_55_LAUNCH_ID, Some("max")),
                entry(&["anthropic"], "claude-fable-5-1", Some("max")),
            ]),
            system_default_model: None,
        },
        ..Default::default()
    });

    assert_eq!(result.map(|result| result.model), Some(launch_expected()));
}
