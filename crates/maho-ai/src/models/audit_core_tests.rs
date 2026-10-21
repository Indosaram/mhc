use super::*;
use crate::models_generated::get_builtin_model;

fn operation_options() -> CreateProviderOptions {
    CreateProviderOptions {
        id: "contract-test".into(),
        name: None,
        base_url: None,
        headers: None,
        auth: None,
        models: Vec::new(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(IndexMap::new()),
        images: IndexMap::new(),
    }
}

struct ContractImages;

impl crate::types::ProviderImages for ContractImages {
    fn generate_images<'a>(
        &'a self,
        _: &'a crate::types::ImagesModel,
        _: &'a crate::types::ImagesContext,
        _: Option<crate::types::ImagesOptions>,
    ) -> BoxFuture<'a, Result<crate::types::AssistantImages, String>> {
        Box::pin(async { Err("image-implementation-selected".into()) })
    }
}

#[test]
fn provider_rejects_empty_complete_operation_set() {
    let options = operation_options();
    let result = create_provider_with_classifiers(options, None, Vec::new(), IndexMap::new());
    let error = result.err().expect("empty operations rejected");
    assert_eq!(error.code, ModelsErrorCode::Provider);
}

#[tokio::test]
async fn image_only_provider_dispatches_without_chat_implementation() {
    let mut options = operation_options();
    options.images.insert("contract-images".into(), Arc::new(ContractImages));
    let mut model = crate::image_models::get_image_model("openai", "gpt-image-2.5-flare")
        .expect("image fixture").clone();
    model.api = "contract-images".into();
    let provider = create_provider(options).expect("image-only operation is valid");
    let result = provider.generate_images(&model, &crate::types::ImagesContext::default(), None).await;
    assert_eq!(result.err().as_deref(), Some("image-implementation-selected"));
}

#[test]
fn classifier_only_provider_accepts_concrete_operation_without_catalog_models() {
    let mut classifiers: IndexMap<String, Arc<dyn ProviderClassifier>> = IndexMap::new();
    classifiers.insert("typesafe-system-one".into(), Arc::new(BuiltinClassifier));
    let result = create_provider_with_classifiers(operation_options(), None, Vec::new(), classifiers);
    assert!(result.expect("classifier-only operation is valid").supports_classification());
}

#[test]
fn chat_only_provider_accepts_concrete_operation_without_catalog_models() {
    let mut options = operation_options();
    options.api = ProviderApi::Single(crate::providers::builtin_api_streams("openai-responses"));
    let result = create_provider(options);
    assert!(result.is_ok());
}

#[test]
fn assistant_prefill_requires_explicit_capability_and_compatible_thinking() {
    let base = model("prefill");
    let cases = [
        (None, "anthropic-messages", false, false),
        (Some(false), "anthropic-messages", false, false),
        (Some(true), "anthropic-messages", false, true),
        (Some(true), "anthropic-messages", true, false),
        (Some(true), "openai-completions", true, true),
    ];
    for (capability, api, thinking, expected) in cases {
        let mut model = base.clone();
        model.api = api.into();
        model.supports_assistant_prefill = capability;
        let supported = crate::model::model_supports_assistant_prefill(&model, thinking);
        assert_eq!(supported, expected);
    }
}

#[test]
fn generated_cache_metadata_only_marks_direct_anthropic() {
    let anthropic = get_builtin_model("anthropic", "claude-fable-5").expect("Anthropic");
    let openai = get_builtin_model("openai", "gpt-6-astra").expect("OpenAI");
    let wire = serde_json::to_value(anthropic).expect("serialize model");
    assert_eq!(wire["promptCache"], serde_json::json!({"short": 300, "long": 3600}));
    assert_eq!(openai.prompt_cache, None);
    assert_eq!(anthropic.supports_assistant_prefill, None);
}

#[test]
fn image_limits_round_trip_independently_optional_dimensions() {
    let wire = serde_json::json!({
        "maxRequestBytes": 1024,
        "images": {"maxPerMessage": 2, "maxPerRequest": 4,
            "resize": {"maxWidth": 800, "maxBytes": 512, "jpegQuality": 75}}
    });
    let limits: crate::model::ModelInputLimits = serde_json::from_value(wire.clone()).expect("limits");
    let round_trip = serde_json::to_value(limits).expect("serialize limits");
    assert_eq!(round_trip, wire);
}

#[test]
fn assistant_usage_limit_cause_survives_presentation_replacement() {
    for scope in ["account", "model"] {
        let wire = serde_json::json!({
            "content": [], "api": "openai-responses", "provider": "contract-test", "model": "test",
            "usage": Usage::default(), "stopReason": "error", "timestamp": 0,
            "errorCause": {"type": "usage_limit", "scope": scope}
        });
        let mut message: AssistantMessage = serde_json::from_value(wire).expect("assistant message");
        message.error_message = Some("presentation-only replacement".into());
        let projected = serde_json::to_value(message).expect("serialize assistant");
        assert_eq!(projected["errorCause"], serde_json::json!({"type": "usage_limit", "scope": scope}));
    }
}

#[test]
fn nested_calls_round_trip_omitted_arguments_and_unfinished_status() {
    let wire = serde_json::json!({
        "toolCallId": "parent", "toolName": "eval", "content": [], "isError": false, "timestamp": 0,
        "nestedCalls": {"complete": false, "calls": [
            {"id": "a", "name": "read", "arguments": {"path": "file"}, "status": "ok", "durationMs": 2},
            {"id": "b", "name": "write", "argumentsBytes": 4096, "status": "error", "error": "bounded"},
            {"id": "c", "name": "bash", "status": "unfinished"}
        ]}
    });
    let message: crate::types::ToolResultMessage = serde_json::from_value(wire.clone()).expect("tool result");
    let round_trip = serde_json::to_value(message).expect("serialize result");
    assert_eq!(round_trip, wire);
}

fn model(id: &str) -> Model {
    let mut model = get_builtin_model("openai", "gpt-6-astra")
        .expect("fixture")
        .clone();
    model.id = id.into();
    model.thinking_level_map = None;
    model
}

#[test]
fn gpt_61_sol_infers_documented_tiers_when_metadata_is_absent() {
    // Given a namespaced map-less model, when supported levels are inferred:
    let levels = get_supported_thinking_levels(&model("openai/gpt-6.1-sol"));
    // Then only the documented efforts are offered.
    assert_eq!(
        levels,
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn gpt_61_sol_keeps_explicit_endpoint_efforts() {
    // Given an authoritative endpoint map:
    let mut model = model("gpt-6.1-sol");
    model.thinking_level_map = Some([(ModelThinkingLevel::Max, None)].into_iter().collect());
    // When extended tiers are checked, then the explicit exclusions win.
    assert!(!supports_max(&model));
    assert!(!supports_xhigh(&model));
}

#[test]
fn meta_catalog_preserves_contributor_prices_and_model_specific_max() {
    // Given the embedded catalog, when Meta variants are selected:
    let contributor = get_builtin_model("meta", "muse-spark-1.3-contributor").expect("contributor");
    let regular = get_builtin_model("meta", "muse-spark-1.3").expect("regular");
    // Then price and effort capabilities remain distinct.
    assert!((contributor.cost.input - 0.1).abs() < f64::EPSILON);
    assert!(!supports_max(contributor));
    assert!(supports_max(regular));
}

#[tokio::test]
async fn failed_restore_keeps_only_the_providers_last_usable_stored_models() {
    // Given stored models and a failing migration hook:
    let mut cached = model("cached");
    cached.provider = "audit-restore".into();
    let foreign = model("foreign");
    let provider = create_provider(CreateProviderOptions {
        id: "audit-restore".into(),
        name: None,
        base_url: None,
        headers: None,
        auth: None,
        models: Vec::new(),
        fetch_models: Some(Arc::new(|_| Box::pin(async { Ok(Vec::new()) }))),
        restore_models: Some(Arc::new(|_| {
            Err(ModelsError::new(
                ModelsErrorCode::ModelValidation,
                "migration failed",
            ))
        })),
        filter_models: None,
        api: ProviderApi::Single(crate::providers::builtin_api_streams("openai-responses")),
        images: IndexMap::new(),
    }).expect("chat implementation");
    let context = RefreshModelsContext {
        credential: None,
        stored: Some(ModelsStoreEntry {
            models: vec![cached.clone(), foreign],
            ..Default::default()
        }),
        publish: Arc::new(|publication| {
            Box::pin(async move {
                if let Some(update) = publication.update {
                    update();
                }
                Ok(true)
            })
        }),
        allow_network: false,
        force: None,
        signal: AbortController::new().signal(),
    };
    // When the offline catalog is restored:
    provider.refresh_models(context).await.expect("refresh");
    // Then the last usable provider-filtered list survives.
    assert_eq!(provider.get_models(), vec![cached]);
}
