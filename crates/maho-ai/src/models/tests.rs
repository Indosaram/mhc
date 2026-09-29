use super::*;
use crate::models_generated::{CatalogError, MODELS, get_builtin_model, get_builtin_provider_models, get_builtin_providers};
use crate::types::{AssistantMessageEvent, DoneReason, ModelThinkingLevel as L, StopReason};
use std::sync::atomic::{AtomicUsize, Ordering};

fn builtin(provider: &str, id: &str) -> Model {
    get_builtin_model(provider, id).expect("builtin model").clone()
}

#[test]
fn catalog_matches_senpi_generated_counts_and_order() {
    assert_eq!(get_builtin_providers().len(), 42);
    assert_eq!(MODELS.values().map(IndexMap::len).sum::<usize>(), 1750);
    assert_eq!(get_builtin_providers().first().copied(), Some("alibaba-token-plan"));
    let xai: Vec<&str> = MODELS["xai"].keys().map(String::as_str).collect();
    assert_eq!(
        xai,
        ["grok-4.20-0309-non-reasoning", "grok-4.20-0309-reasoning", "grok-4.3", "grok-4.5", "grok-4.6", "grok-4.7"]
    );
}

#[test]
fn anthropic_claude_models_resolve_with_senpi_context_windows() {
    let claude: Vec<&Model> = get_builtin_provider_models("anthropic")
        .expect("anthropic")
        .into_iter()
        .filter(|m| m.id.starts_with("claude-"))
        .collect();
    assert_eq!(claude.len(), 15);
    let expected = [
        ("claude-fable-5", 1_000_000, 128_000),
        ("claude-fable-5-1", 1_000_000, 128_000),
        ("claude-haiku-4-5", 200_000, 64_000),
        ("claude-haiku-4-5-20251001", 200_000, 64_000),
    ];
    for (id, context_window, max_tokens) in expected {
        let model = builtin("anthropic", id);
        assert_eq!((model.context_window, model.max_tokens), (context_window, max_tokens), "{id}");
        assert_eq!(model.api, "anthropic-messages");
    }
}

#[test]
fn unknown_model_and_provider_return_typed_errors() {
    assert_eq!(
        get_builtin_model("anthropic", "claude-does-not-exist"),
        Err(CatalogError::NotFound { provider: "anthropic".into(), model_id: "claude-does-not-exist".into() })
    );
    assert_eq!(
        get_builtin_provider_models("nope").map(|m| m.len()),
        Err(CatalogError::UnknownProvider { provider: "nope".into() })
    );
    assert_eq!(
        get_builtin_model("anthropic", "x").map(|m| m.id.clone()).map_err(|e| e.to_string()),
        Err("Model not found: anthropic/x".into())
    );
}

#[test]
fn derives_model_api_id_and_provider_literals_from_grouped_model_data() {
    for (provider, id, api) in [
        ("xai", "grok-4.5", "openai-responses"),
        ("xai", "grok-4.6", "openai-responses"),
        ("xai", "grok-4.7", "openai-responses"),
        ("xai", "grok-4.3", "openai-responses"),
        ("github-copilot", "grok-4.7", "openai-responses"),
        ("xiaomi", "mimo-v2.5-pro", "openai-completions"),
        ("xiaomi", "mimo-v2.6-pro", "openai-completions"),
    ] {
        let model = builtin(provider, id);
        assert_eq!((model.id.as_str(), model.provider.as_str(), model.api.as_str()), (id, provider, api));
    }
}

#[test]
fn routes_github_copilot_grok_4_5_through_the_responses_api() {
    assert_eq!(builtin("github-copilot", "grok-4.5").api, "openai-responses");
}

#[test]
fn routes_all_github_copilot_gpt_models_through_the_responses_api() {
    let gpt: Vec<&Model> = MODELS["github-copilot"].values().filter(|m| m.id.starts_with("gpt-")).collect();
    assert!(!gpt.is_empty());
    assert!(gpt.iter().all(|m| m.api == "openai-responses"));
    assert_eq!(builtin("github-copilot", "gpt-6-astra").api, "openai-responses");
}

#[test]
fn ships_grok_4_7_and_mimo_v2_6_pro_on_their_direct_provider_shards() {
    let ids = |p: &str| get_builtin_provider_models(p).expect("provider").into_iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    let xai = ids("xai");
    assert!(xai.contains(&"grok-4.6".into()) && xai.contains(&"grok-4.7".into()));
    let xiaomi = ids("xiaomi");
    assert!(xiaomi.contains(&"mimo-v2.5-pro".into()) && xiaomi.contains(&"mimo-v2.6-pro".into()));
}

#[test]
fn thinking_levels_match_senpi() {
    let opus = builtin("anthropic", "claude-opus-4-8");
    assert_eq!(get_supported_thinking_levels(&opus), ModelThinkingLevel::ALL.to_vec());
    assert_eq!(clamp_thinking_level(&opus, L::Max), L::Max);
    let astra = builtin("openai", "gpt-6-astra");
    assert_eq!(get_supported_thinking_levels(&astra), vec![L::Low, L::Medium, L::High, L::Xhigh, L::Max]);
    assert_eq!(clamp_thinking_level(&astra, L::Minimal), L::Low);
    assert_eq!(clamp_thinking_level(&astra, L::Off), L::Low);
    let gpt4 = builtin("openai", "gpt-4");
    assert_eq!(get_supported_thinking_levels(&gpt4), vec![L::Off]);
    assert_eq!(clamp_thinking_level(&gpt4, L::High), L::Off);
    assert!(supports_xhigh(&opus) && supports_max(&opus));
    assert!(!supports_xhigh(&gpt4) && !supports_max(&gpt4));
}

#[test]
fn model_family_matching_respects_boundaries() {
    assert!(matches_model_family("openai/gpt-5.5", "gpt-5.5"));
    assert!(matches_model_family("gpt-5.5-mini", "gpt-5.5"));
    assert!(!matches_model_family("chatgpt-5.5", "gpt-5.5"));
    assert!(!matches_model_family("gpt-5.55", "gpt-5.5"));
    assert!(matches_model_family("claude-opus-4-8", "opus-4-8"));
    assert!(!matches_model_family("opus-4-80", "opus-4-8"));
}

#[test]
fn calculate_cost_applies_tiers_and_one_hour_cache_writes() {
    let model = builtin("bai", "gpt-5.5");
    let mut usage = Usage { input: 300_000, output: 1000, cache_write: 2000, cache_write_1h: Some(500), ..Usage::default() };
    let cost = calculate_cost(&model, &mut usage);
    assert_eq!(cost.input, 3.000_000_000_000_000_4);
    assert_eq!(cost.output, 0.045_000_000_000_000_005);
    assert_eq!(cost.cache_read, 0.0);
    assert_eq!(cost.cache_write, 0.025);
    assert_eq!(cost.total, 3.070_000_000_000_000_3);
    assert_eq!(usage.cost, cost);
}

#[test]
fn models_are_equal_compares_id_and_provider() {
    let a = builtin("anthropic", "claude-opus-4-8");
    let mut b = a.clone();
    assert!(models_are_equal(Some(&a), Some(&b)));
    b.provider = "other".into();
    assert!(!models_are_equal(Some(&a), Some(&b)));
    assert!(!models_are_equal(Some(&a), None));
    assert!(has_api(&a, "anthropic-messages"));
}

#[test]
fn merge_headers_replaces_case_insensitively() {
    let base: ProviderHeaders = [("X-Key".to_owned(), Some("a".to_owned()))].into_iter().collect();
    let over: ProviderHeaders = [("x-key".to_owned(), None)].into_iter().collect();
    let merged = merge_headers(Some(&base), Some(&over)).expect("merged");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged.get("x-key"), Some(&None));
    assert_eq!(merge_headers(None, None), None);
}

struct EchoStreams {
    calls: AtomicUsize,
}

impl ProviderStreams for EchoStreams {
    fn stream(&self, model: &Model, _context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let stream = AssistantMessageEventStream::assistant();
        let mut message = crate::utils::lazy::setup_error_message(model, "");
        message.stop_reason = StopReason::Stop;
        message.error_message = None;
        message.response_id = options.and_then(|o| o.request.api_key);
        stream.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message });
        stream
    }

    fn stream_simple(&self, model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        self.stream(model, context, options.map(|o| o.stream))
    }
}

fn test_provider(id: &str, models: Vec<Model>, fetch: Option<FetchModels>) -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: id.into(),
        name: None,
        base_url: None,
        headers: None,
        models,
        fetch_models: fetch,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(Arc::new(EchoStreams { calls: AtomicUsize::new(0) })),
    })
}

fn with_provider(model: &Model, provider: &str) -> Model {
    let mut model = model.clone();
    model.provider = provider.into();
    model
}

#[tokio::test]
async fn models_collection_streams_with_explicit_key_and_rejects_unconfigured() {
    let base = builtin("anthropic", "claude-opus-4-8");
    let model = with_provider(&base, "maho-test-provider");
    let models = create_models(None);
    models.set_provider(test_provider("maho-test-provider", vec![model.clone()], None));
    assert_eq!(models.get_model("maho-test-provider", &model.id), Some(model.clone()));
    assert_eq!(models.get_provider("maho-test-provider").map(|p| p.name().to_owned()), Some("maho-test-provider".into()));

    let mut options = StreamOptions::default();
    options.request.api_key = Some("explicit".into());
    let done = models.complete(&model, &Context::default(), Some(options), ModelsRequestTransforms::default()).await;
    assert_eq!(done.expect("result").response_id.as_deref(), Some("explicit"));

    let failed = models.complete(&model, &Context::default(), None, ModelsRequestTransforms::default()).await.expect("result");
    assert_eq!(failed.stop_reason, StopReason::Error);
    assert_eq!(failed.error_message.as_deref(), Some("Provider is not configured: maho-test-provider"));

    let unknown = with_provider(&base, "nobody");
    let failed = models.complete(&unknown, &Context::default(), None, ModelsRequestTransforms::default()).await.expect("result");
    assert_eq!(failed.error_message.as_deref(), Some("Unknown provider: nobody"));
    assert!(models.get_available(None).await.is_empty());
}

#[tokio::test]
async fn refresh_restores_stored_models_then_publishes_fetched_ones() {
    let base = builtin("anthropic", "claude-opus-4-8");
    let baseline = with_provider(&base, "maho-refresh");
    let mut fetched = baseline.clone();
    fetched.id = "fetched-model".into();
    let fetched_clone = fetched.clone();
    let fetch: FetchModels = Arc::new(move |_ctx| {
        let fetched = fetched_clone.clone();
        Box::pin(async move { Ok(vec![fetched]) })
    });

    struct KeyAuth;
    impl ModelsAuth for KeyAuth {
        fn resolve<'a>(&'a self, _p: &'a dyn Provider, _o: &'a AuthResolutionOverrides) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
            Box::pin(async { Ok(Some(AuthResolution::default())) })
        }
        fn refresh_credential<'a>(&'a self, _p: &'a dyn Provider, _s: Option<&'a Credential>, _g: &'a AbortSignal) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
            Box::pin(async { Ok(Some(serde_json::json!({"type": "api_key", "key": "k"}))) })
        }
    }

    let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
    let models = create_models(Some(CreateModelsOptions { models_store: Some(store.clone()), auth: Some(Arc::new(KeyAuth)) }));
    models.set_provider(test_provider("maho-refresh", vec![baseline.clone()], Some(fetch)));

    let result = models.refresh(ModelsRefreshOptions::default()).await;
    assert!(!result.aborted && result.errors.is_empty());
    let ids: Vec<String> = models.get_models(Some("maho-refresh")).into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![baseline.id.clone(), "fetched-model".into()]);
    let persisted = store.read("maho-refresh", None).await.expect("read").expect("entry");
    assert_eq!(persisted.models, vec![fetched]);
    assert!(persisted.checked_at.is_some());

    let offline = models.refresh(ModelsRefreshOptions { allow_network: Some(false), ..ModelsRefreshOptions::default() }).await;
    assert!(offline.errors.is_empty());
    assert_eq!(models.get_available(Some("maho-refresh")).await.len(), 2);

    let controller = AbortController::new();
    controller.abort(None);
    let aborted = models.refresh(ModelsRefreshOptions { signal: Some(controller.signal()), ..ModelsRefreshOptions::default() }).await;
    assert!(aborted.aborted);
}

#[test]
fn models_error_appends_cause_detail_once() {
    let error = ModelsError::with_cause(ModelsErrorCode::ModelSource, "Model refresh failed for p", " boom ");
    assert_eq!(error.message, "Model refresh failed for p: boom");
    assert_eq!(ModelsError::with_cause(ModelsErrorCode::Auth, "has boom", "boom").message, "has boom");
    assert_eq!(error.code.as_str(), "model_source");
}
