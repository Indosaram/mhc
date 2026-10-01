use super::*;
use crate::models_generated::{CatalogError, MODELS, get_builtin_model, get_builtin_provider_models, get_builtin_providers};
use crate::types::{AssistantMessageEvent, DoneReason, ModelCost, ModelCostTier, ModelThinkingLevel as L, StopReason};
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
        stream.push(AssistantMessageEvent::Start { partial: message.clone() });
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

// senpi test/models-runtime.test.ts has 40 `it()` cases. 18 exercise `InMemoryCredentialStore`
// (auth/credential-store.ts) directly or through `models.checkAuth`/`login`/`logout`/`getAuth`
// OAuth dispatch, all of which live behind `auth/` (todo 13, still a `// ported by todo 13` stub
// in this crate) — excluded below by title, owned by todo 13. The remaining 22 cases are ported
// here (each carries its TS title verbatim in a local `title` binding used in assertion
// messages), on top of the pre-existing catalog/cost/thinking-level/family-matching tests above
// which port the model-catalog behavior this file's TS counterpart re-exercises only
// incidentally through `testModel`/`testProvider` fixtures.
//
// Excluded (owned by todo 13, auth/credential-store.ts + auth/resolve.ts OAuth dispatch):
// - "enumerates credential metadata without exposing secrets"
// - "persists dynamic catalogs and restores them without network access"
// - "refreshes expired OAuth before refreshing models"
// - "cancels queued credential mutations without running them later"
// - "passes cancellation to OAuth refresh and preserves the previous credential"
// - "resolves auth: stored credential owns the provider, ambient only when nothing stored"
// - "checks provider auth without refreshing OAuth and filters available models"
// - "uses an OAuth availability check for stored and ambient auth"
// - "runs provider login and logout through the credential store"
// - "a stored credential without a matching handler blocks ambient fallback"
// - "refreshes expired oauth credentials and persists the rotated credential"
// - "refreshes oauth credentials with less than five minutes remaining"
// - "honors a caller's longer OAuth minimum validity"
// - "rejects with code oauth when refresh fails, preserving the stored credential"
// - "serializes concurrent OAuth refreshes through store.modify (no double refresh)"
// - "valid oauth tokens resolve without touching modify"
// - "wraps credential store failures in ModelsError"
// - "keeps the underlying reason in wrapped oauth refresh errors"

fn plain_provider(id: &str, models: Vec<Model>) -> Arc<dyn Provider> {
    test_provider(id, models, None)
}

#[test]
fn registers_replaces_and_deletes_providers() {
    let title = "registers, replaces, and deletes providers";
    let models = create_models(None);
    models.set_provider(plain_provider("p1", vec![]));
    models.set_provider(plain_provider("p2", vec![]));
    assert_eq!(models.get_providers().iter().map(|p| p.id().to_owned()).collect::<Vec<_>>(), ["p1", "p2"], "{title}");

    let replacement = plain_provider("p1", vec![]);
    models.set_provider(replacement.clone());
    assert!(Arc::ptr_eq(&models.get_provider("p1").expect("p1"), &replacement), "{title}");
    assert_eq!(models.get_providers().len(), 2, "{title}");

    models.delete_provider("p1");
    assert!(models.get_provider("p1").is_none(), "{title}");

    models.clear_providers();
    assert_eq!(models.get_providers().len(), 0, "{title}");
}

#[test]
fn lists_and_finds_models_per_provider() {
    let title = "lists and finds models per provider";
    let base = builtin("anthropic", "claude-opus-4-8");
    let m1 = with_provider(&base, "p1");
    let mut m2 = with_provider(&base, "p1");
    m2.id = "m2".into();
    let m3 = with_provider(&base, "p2");
    let models = create_models(None);
    models.set_provider(plain_provider("p1", vec![m1.clone(), m2.clone()]));
    models.set_provider(plain_provider("p2", vec![m3.clone()]));

    assert_eq!(models.get_models(None).iter().map(|m| m.id.clone()).collect::<Vec<_>>(), [m1.id.clone(), m2.id.clone(), m3.id.clone()], "{title}");
    assert_eq!(models.get_models(Some("p1")).iter().map(|m| m.id.clone()).collect::<Vec<_>>(), [m1.id.clone(), m2.id.clone()], "{title}");
    assert_eq!(models.get_models(Some("nope")).len(), 0, "{title}");
    assert_eq!(models.get_model("p2", &m3.id).map(|m| m.id), Some(m3.id.clone()), "{title}");
    assert!(models.get_model("p2", "missing").is_none(), "{title}");

    let found = models.get_model("p2", &m3.id).expect("m3");
    assert!(has_api(&found, "anthropic-messages"), "{title}");
    assert!(!has_api(&found, "openai-completions"), "{title}");
}

struct ThrowingProvider;
impl Provider for ThrowingProvider {
    fn id(&self) -> &str {
        "broken"
    }
    fn name(&self) -> &str {
        "broken"
    }
    fn get_models(&self) -> Vec<Model> {
        panic!("boom")
    }
    fn stream(&self, model: &Model, _c: &Context, _o: Option<StreamOptions>) -> AssistantMessageEventStream {
        error_stream_for_test(model)
    }
    fn stream_simple(&self, model: &Model, _c: &Context, _o: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        error_stream_for_test(model)
    }
}

fn error_stream_for_test(model: &Model) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    stream.end(None);
    let _ = model;
    stream
}

#[test]
#[should_panic(expected = "boom")]
fn swallows_provider_source_failures_for_both_all_provider_and_single_provider_listing() {
    let title = "swallows provider source failures for both all-provider and single-provider listing";
    let models = create_models(None);
    models.set_provider(Arc::new(ThrowingProvider));
    models.set_provider(plain_provider("ok", vec![with_provider(&builtin("anthropic", "claude-opus-4-8"), "ok")]));

    // `Models::get_models` swallows a panicking `getModels()` the way TS swallows a throw.
    let ok_ids: Vec<String> = models.get_models(None).into_iter().map(|m| m.id).collect();
    assert_eq!(ok_ids, vec!["claude-opus-4-8".to_owned()], "{title}");
    assert_eq!(models.get_models(Some("broken")).len(), 0, "{title}");
    // precise failures come from the provider directly: calling through panics, matching TS's
    // `toThrow("boom")` on `getProvider("broken")?.getModels()`.
    models.get_provider("broken").expect("provider").get_models();
}

fn refreshable_provider(id: &str, models: Vec<Model>, fetch: FetchModels) -> Arc<dyn Provider> {
    test_provider(id, models, Some(fetch))
}

type RefreshScript = Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<'static, Result<(), ModelsError>> + Send + Sync>;

/// A provider whose `refreshModels` is the test's own function, mirroring the TS fixture
/// `testProvider({ getModels, refreshModels })`: it runs in BOTH the cache and the network phase
/// (unlike `createProvider`'s `fetchModels`, which only runs with network access), and `getModels`
/// reads the live list the refresh publishes into.
struct ScriptedProvider {
    id: String,
    models: Arc<Mutex<Vec<Model>>>,
    refresh: Option<RefreshScript>,
}

impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.id
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    fn supports_refresh(&self) -> bool {
        self.refresh.is_some()
    }

    fn refresh_models<'a>(&'a self, context: RefreshModelsContext) -> BoxFuture<'a, Result<(), ModelsError>> {
        match &self.refresh {
            Some(refresh) => Box::pin(async move { refresh(context).await }),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn stream(&self, model: &Model, _context: &Context, _options: Option<StreamOptions>) -> AssistantMessageEventStream {
        error_stream_for_test(model)
    }

    fn stream_simple(&self, model: &Model, _context: &Context, _options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
        error_stream_for_test(model)
    }
}

fn scripted_models(models: Vec<Model>) -> Arc<Mutex<Vec<Model>>> {
    Arc::new(Mutex::new(models))
}

fn scripted_provider(id: &str, models: Arc<Mutex<Vec<Model>>>, refresh: Option<RefreshScript>) -> Arc<dyn Provider> {
    Arc::new(ScriptedProvider { id: id.to_owned(), models, refresh })
}

/// `testModel(provider, id)` from the TS fixture: a model whose identity the case controls.
fn test_model(provider: &str, id: &str) -> Model {
    let mut model = with_provider(&builtin("openai", "gpt-6-astra"), provider);
    model.id = id.to_owned();
    model
}

/// The `ModelsAuth` seam's equivalent of the TS fixture `envKeyAuth("key")`: a provider-scoped
/// apiKey auth with a concrete key, so the network refresh phase runs.
fn keyed_auth(key: &'static str) -> Arc<dyn ModelsAuth> {
    Arc::new(CallbackAuth {
        resolve: Box::new(move |_p: &dyn Provider, o: &AuthResolutionOverrides| {
            let explicit = o.api_key.clone();
            Box::pin(async move {
                Ok(Some(AuthResolution {
                    auth: ProviderAuthResult { api_key: Some(explicit.unwrap_or_else(|| key.to_owned())), ..Default::default() },
                    env: None,
                }))
            })
        }),
        refresh_credential: Box::new(move |_p: &dyn Provider, _s: Option<&Credential>, _g: &AbortSignal| {
            Box::pin(async move { Ok(Some(serde_json::json!({ "type": "api_key", "key": key }))) })
        }),
    })
}

#[tokio::test]
async fn refresh_updates_every_configured_dynamic_provider_and_reports_failures() {
    let title = "refresh() updates every configured dynamic provider and reports failures";
    let list: Arc<Mutex<Vec<Model>>> = Arc::new(Mutex::new(vec![test_model("dyn", "before")]));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let (list_for_refresh, refreshes_for_refresh) = (list.clone(), refreshes.clone());
    let refresh: RefreshScript = Arc::new(move |context: RefreshModelsContext| {
        let (list, refreshes) = (list_for_refresh.clone(), refreshes_for_refresh.clone());
        Box::pin(async move {
            if !context.allow_network {
                return Ok(());
            }
            refreshes.fetch_add(1, Ordering::SeqCst);
            let publish = context.publisher();
            publish(ModelsPublication {
                persist: None,
                update: Some(Box::new(move || {
                    *list.lock().expect("list") = vec![test_model("dyn", "after")];
                })),
            })
            .await
            .expect("publish");
            Ok(())
        })
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(ambient_auth()) }));
    models.set_provider(scripted_provider("dyn", list.clone(), Some(refresh)));
    models.set_provider(plain_provider("static", vec![test_model("static", "s1")]));

    assert!(models.get_model("dyn", "before").is_some(), "{title}");
    let first = models.refresh(ModelsRefreshOptions::default()).await;
    assert_eq!(first.errors.len(), 0, "{title}");
    assert_eq!(refreshes.load(Ordering::SeqCst), 1, "{title}");
    assert!(models.get_model("dyn", "after").is_some(), "{title}");
    assert!(models.get_model("dyn", "before").is_none(), "{title}");

    let flaky: RefreshScript = Arc::new(|context: RefreshModelsContext| {
        Box::pin(async move {
            if context.allow_network {
                return Err(ModelsError::new(ModelsErrorCode::ModelSource, "fetch failed"));
            }
            Ok(())
        })
    });
    models.set_provider(scripted_provider("flaky", scripted_models(vec![]), Some(flaky)));
    let second = models.refresh(ModelsRefreshOptions::default()).await;
    assert_eq!(refreshes.load(Ordering::SeqCst), 2, "{title}");
    assert_eq!(second.errors.get("flaky").map(|error| error.message.as_str()), Some("fetch failed"), "{title}");
}

#[tokio::test]
async fn restricts_refresh_work_to_selected_providers() {
    let title = "restricts refresh work to selected providers";
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(ambient_auth()) }));
    for id in ["one", "two"] {
        let calls = calls.clone();
        let id_owned = id.to_owned();
        let refresh: RefreshScript = Arc::new(move |context: RefreshModelsContext| {
            let (calls, id_owned) = (calls.clone(), id_owned.clone());
            let phase = if context.allow_network { "network" } else { "cache" };
            Box::pin(async move {
                calls.lock().expect("calls").push(format!("{id_owned}:{phase}"));
                Ok(())
            })
        });
        models.set_provider(scripted_provider(id, scripted_models(vec![]), Some(refresh)));
    }

    let result = models
        .refresh(ModelsRefreshOptions { providers: Some(vec!["two".into(), "unknown".into()]), ..ModelsRefreshOptions::default() })
        .await;

    assert_eq!(result.errors.len(), 0, "{title}");
    assert_eq!(*calls.lock().expect("calls"), vec!["two:cache".to_owned(), "two:network".to_owned()], "{title}");
}

#[tokio::test]
async fn lets_providers_choose_persistent_deletion_and_ephemeral_publication_atomically() {
    let title = "lets providers choose persistent deletion and ephemeral publication atomically";
    let entry: Arc<Mutex<Option<ModelsStoreEntry>>> =
        Arc::new(Mutex::new(Some(ModelsStoreEntry { models: vec![test_model("dynamic", "stored")], ..ModelsStoreEntry::default() })));

    struct RecordingStore {
        entry: Arc<Mutex<Option<ModelsStoreEntry>>>,
    }
    impl ModelsStore for RecordingStore {
        fn read<'a>(&'a self, _p: &'a str, _o: Option<&'a ModelsStoreOperationOptions>) -> crate::types::BoxFuture<'a, Result<Option<ModelsStoreEntry>, AbortReason>> {
            let entry = self.entry.lock().expect("entry").clone();
            Box::pin(async move { Ok(entry) })
        }
        fn write<'a>(&'a self, _p: &'a str, next: &'a ModelsStoreEntry, _o: Option<&'a ModelsStoreOperationOptions>) -> crate::types::BoxFuture<'a, Result<(), AbortReason>> {
            *self.entry.lock().expect("entry") = Some(next.clone());
            Box::pin(async { Ok(()) })
        }
        fn delete<'a>(&'a self, _p: &'a str, _o: Option<&'a ModelsStoreOperationOptions>) -> crate::types::BoxFuture<'a, Result<(), AbortReason>> {
            *self.entry.lock().expect("entry") = None;
            Box::pin(async { Ok(()) })
        }
    }

    let state = Arc::new(Mutex::new("initial".to_owned()));
    let (entry_for_refresh, state_for_delete, state_for_ephemeral) = (entry.clone(), state.clone(), state.clone());
    let refresh: RefreshScript = Arc::new(move |context: RefreshModelsContext| {
        let (entry, state_delete, state_ephemeral) =
            (entry_for_refresh.clone(), state_for_delete.clone(), state_for_ephemeral.clone());
        let stored = context.stored.clone();
        let publish = context.publisher();
        Box::pin(async move {
            assert_eq!(stored.map(|entry| entry.models[0].id.clone()), Some("stored".to_owned()), "stored catalog is restored");
            publish(ModelsPublication {
                persist: Some(None),
                update: Some(Box::new(move || {
                    assert!(entry.lock().expect("entry").is_none(), "persist: null deletes the stored entry");
                    *state_delete.lock().expect("state") = "deleted".to_owned();
                })),
            })
            .await
            .expect("publish delete");
            publish(ModelsPublication {
                persist: None,
                update: Some(Box::new(move || {
                    *state_ephemeral.lock().expect("state") = "ephemeral".to_owned();
                })),
            })
            .await
            .expect("publish ephemeral");
            Ok(())
        })
    });
    let store: Arc<dyn ModelsStore> = Arc::new(RecordingStore { entry: entry.clone() });
    let models = create_models(Some(CreateModelsOptions { models_store: Some(store), auth: Some(ambient_auth()) }));
    models.set_provider(scripted_provider("dynamic", scripted_models(vec![]), Some(refresh)));

    let result = models.refresh(ModelsRefreshOptions { allow_network: Some(false), ..ModelsRefreshOptions::default() }).await;

    assert_eq!(result.errors.len(), 0, "{title}");
    assert!(entry.lock().expect("entry").is_none(), "{title}");
    assert_eq!(*state.lock().expect("state"), "ephemeral", "{title}");
}

#[tokio::test]
async fn always_gives_providers_a_concrete_signal() {
    let title = "always gives providers a concrete signal";
    let seen: Arc<Mutex<Vec<AbortSignal>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_refresh = seen.clone();
    let refresh: RefreshScript = Arc::new(move |context: RefreshModelsContext| {
        seen_for_refresh.lock().expect("seen").push(context.signal.clone());
        Box::pin(async { Ok(()) })
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(ambient_auth()) }));
    models.set_provider(scripted_provider("dynamic", scripted_models(vec![]), Some(refresh)));

    let result = models.refresh(ModelsRefreshOptions::default()).await;
    assert!(!result.aborted, "{title}");
    let seen = seen.lock().expect("seen");
    assert!(!seen.is_empty(), "{title}");
    assert!(seen.iter().all(|signal| !signal.aborted()), "{title}");
}

#[tokio::test]
async fn binds_model_store_waits_to_the_provider_refresh_signal() {
    let title = "binds model-store waits to the provider refresh signal";
    let storage_signals: Arc<Mutex<Vec<Option<AbortSignal>>>> = Arc::new(Mutex::new(Vec::new()));
    let (write_started_tx, mut write_started_rx) = tokio::sync::watch::channel(false);
    let (write_outcome_tx, write_outcome_rx) = tokio::sync::oneshot::channel::<Result<(), AbortReason>>();

    /// Owned solely by the parked write future: completing it reports the write's own outcome,
    /// and dropping it (the write was cancelled) reports the abort reason its signal carries.
    struct CancelGuard {
        slot: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Result<(), AbortReason>>>>,
        signal: Option<AbortSignal>,
    }
    impl CancelGuard {
        fn finish(&self, result: Result<(), AbortReason>) {
            if let Some(sender) = self.slot.lock().expect("slot").take() {
                let _ = sender.send(result);
            }
        }
    }
    impl Drop for CancelGuard {
        fn drop(&mut self) {
            let cancelled = self.signal.as_ref().and_then(AbortSignal::reason).map(Err).unwrap_or(Ok(()));
            self.finish(cancelled);
        }
    }

    struct RecordingStore {
        signals: Arc<Mutex<Vec<Option<AbortSignal>>>>,
        write_started: tokio::sync::watch::Sender<bool>,
        write_outcome: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Result<(), AbortReason>>>>,
    }
    impl ModelsStore for RecordingStore {
        fn read<'a>(&'a self, _p: &'a str, o: Option<&'a ModelsStoreOperationOptions>) -> crate::types::BoxFuture<'a, Result<Option<ModelsStoreEntry>, AbortReason>> {
            self.signals.lock().expect("signals").push(o.and_then(|o| o.signal.clone()));
            Box::pin(async { Ok(None) })
        }
        fn write<'a>(&'a self, _p: &'a str, _e: &'a ModelsStoreEntry, o: Option<&'a ModelsStoreOperationOptions>) -> crate::types::BoxFuture<'a, Result<(), AbortReason>> {
            self.signals.lock().expect("signals").push(o.and_then(|o| o.signal.clone()));
            let signal = o.and_then(|o| o.signal.clone());
            self.write_started.send_replace(true);
            let guard = CancelGuard {
                slot: std::sync::Mutex::new(self.write_outcome.lock().expect("slot").take()),
                signal: signal.clone(),
            };
            Box::pin(async move {
                let result = match signal {
                    Some(signal) => race_with_abort_signal(std::future::pending::<()>(), &signal).await,
                    None => Ok(()),
                };
                guard.finish(result.clone());
                result
            })
        }
        fn delete<'a>(&'a self, _p: &'a str, o: Option<&'a ModelsStoreOperationOptions>) -> crate::types::BoxFuture<'a, Result<(), AbortReason>> {
            self.signals.lock().expect("signals").push(o.and_then(|o| o.signal.clone()));
            Box::pin(async { Ok(()) })
        }
    }
    let provider_signal: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let provider_signal_clone = provider_signal.clone();
    let fetch: FetchModels = Arc::new(move |ctx| {
        *provider_signal_clone.lock().expect("signal") = Some(ctx.signal.clone());
        let publish_ctx_signal = ctx.signal.clone();
        let _ = publish_ctx_signal;
        Box::pin(async move { Ok(vec![with_provider(&builtin("anthropic", "claude-opus-4-8"), "dynamic")]) })
    });
    let store: Arc<dyn ModelsStore> =
        Arc::new(RecordingStore { signals: storage_signals.clone(), write_started: write_started_tx, write_outcome: std::sync::Mutex::new(Some(write_outcome_tx)) });
    let models = create_models(Some(CreateModelsOptions { models_store: Some(store), auth: Some(keyed_auth("key")) }));
    models.set_provider(refreshable_provider("dynamic", vec![], fetch));

    let caller = AbortController::new();
    let models_for_spawn = models.clone();
    let signal = Some(caller.signal());
    let pending = tokio::spawn(async move {
        models_for_spawn.refresh(ModelsRefreshOptions { providers: Some(vec!["dynamic".into()]), signal, ..ModelsRefreshOptions::default() }).await
    });

    // The network-phase write parks on the signal it was handed, so the store waits can be shown
    // to be bound to the provider's refresh signal: aborting the refresh cancels the parked write
    // with the refresh's own reason and aborts the very signal the provider received.
    write_started_rx.wait_for(|started| *started).await.expect("write parks");
    let signals = storage_signals.lock().expect("signals").clone();
    assert_eq!(signals.len(), 3, "{title}");
    assert!(signals.iter().all(Option::is_some), "{title}");
    let provider_signal = provider_signal.lock().expect("signal").clone().expect("provider signal");
    assert!(!provider_signal.aborted(), "{title}");

    let reason = AbortReason::new("Test", "refresh aborted");
    caller.abort(Some(reason.clone()));
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), write_outcome_rx).await.expect("write settles").expect("write outcome");
    assert_eq!(outcome, Err(reason), "{title}");
    assert!(provider_signal.aborted(), "{title}");
    assert!(signals.iter().flatten().all(AbortSignal::aborted), "{title}");
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), pending).await.expect("refresh settles").expect("join");
    assert!(result.aborted, "{title}");
    assert_eq!(result.errors.len(), 0, "{title}");
}

#[tokio::test]
async fn returns_aborted_state_without_reporting_cancellation_as_a_provider_error() {
    let title = "returns aborted state without reporting cancellation as a provider error";
    let controller = AbortController::new();
    let controller_for_refresh = controller.clone();
    let refresh: RefreshScript = Arc::new(move |_context: RefreshModelsContext| {
        let controller = controller_for_refresh.clone();
        Box::pin(async move {
            controller.abort(None);
            Ok(())
        })
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(ambient_auth()) }));
    models.set_provider(scripted_provider("dynamic", scripted_models(vec![]), Some(refresh)));

    let result = models.refresh(ModelsRefreshOptions { signal: Some(controller.signal()), ..ModelsRefreshOptions::default() }).await;

    assert!(result.aborted, "{title}");
    assert_eq!(result.errors.len(), 0, "{title}");
}

#[tokio::test]
async fn stops_waiting_on_abort_when_a_provider_ignores_its_signal() {
    let title = "stops waiting on abort when a provider ignores its signal";
    let controller = AbortController::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (started_tx, mut started_rx) = tokio::sync::watch::channel(false);
    let (stall_tx, stall_rx) = tokio::sync::watch::channel(false);
    let (calls_for_refresh, started_for_refresh) = (calls.clone(), Arc::new(started_tx));
    let stall_rx_for_refresh = stall_rx.clone();
    let refresh: RefreshScript = Arc::new(move |_context: RefreshModelsContext| {
        let (calls, started_tx) = (calls_for_refresh.clone(), started_for_refresh.clone());
        let mut stall_rx = stall_rx_for_refresh.clone();
        Box::pin(async move {
            let current = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if current != 1 {
                return Ok(());
            }
            started_tx.send_replace(true);
            // Ignores the refresh signal deliberately, mirroring the TS provider that never
            // observes `signal.aborted` and instead awaits an externally-controlled promise.
            let _ = stall_rx.wait_for(|open| *open).await;
            Ok(())
        })
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(ambient_auth()) }));
    models.set_provider(scripted_provider("dynamic", scripted_models(vec![]), Some(refresh)));

    let models_for_spawn = models.clone();
    let refresh_signal = Some(controller.signal());
    let pending = tokio::spawn(async move {
        models_for_spawn.refresh(ModelsRefreshOptions { signal: refresh_signal, ..ModelsRefreshOptions::default() }).await
    });
    started_rx.wait_for(|started| *started).await.expect("started sender alive");
    controller.abort(None);

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), pending).await.expect("refresh settles").expect("join");
    assert!(result.aborted, "{title}");
    assert_eq!(result.errors.len(), 0, "{title}");
    // The abandoned provider future is dropped, so its late completion can never reach the
    // result: TS's `rejectRefresh(new Error("late provider failure"))` is unobservable here.
    stall_tx.send_replace(true);
}

#[tokio::test]
async fn rejects_late_publication_from_a_superseded_non_cooperative_provider() {
    let title = "rejects late publication from a superseded non-cooperative provider";
    let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let captured: Arc<Mutex<Option<PublishFn>>> = Arc::new(Mutex::new(None));
    let (calls_for_refresh, captured_for_refresh) = (calls.clone(), captured.clone());
    let refresh: RefreshScript = Arc::new(move |context: RefreshModelsContext| {
        if !context.allow_network {
            return Box::pin(async { Ok(()) });
        }
        let (calls, captured) = (calls_for_refresh.clone(), captured_for_refresh.clone());
        Box::pin(async move {
            let current = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if current == 1 {
                // Non-cooperative: the first refresh never publishes on its own. It hands its
                // publisher to the test, which replays the publication after a later refresh
                // superseded that generation.
                *captured.lock().expect("captured") = Some(context.publisher());
                return Ok(());
            }
            let entry = ModelsStoreEntry {
                models: vec![test_model("dynamic", &format!("generation-{current}"))],
                ..ModelsStoreEntry::default()
            };
            context
                .publisher()(ModelsPublication { persist: Some(Some(entry)), update: None })
                .await
                .expect("publish");
            Ok(())
        })
    });
    let models = create_models(Some(CreateModelsOptions { models_store: Some(store.clone()), auth: Some(ambient_auth()) }));
    models.set_provider(scripted_provider("dynamic", scripted_models(vec![]), Some(refresh)));

    let first = models.refresh(ModelsRefreshOptions { providers: Some(vec!["dynamic".into()]), ..ModelsRefreshOptions::default() }).await;
    assert_eq!(first.errors.len(), 0, "{title}");
    let second = models.refresh(ModelsRefreshOptions { providers: Some(vec!["dynamic".into()]), ..ModelsRefreshOptions::default() }).await;
    assert_eq!(second.errors.len(), 0, "{title}");
    let persisted = store.read("dynamic", None).await.expect("read").expect("entry");
    assert_eq!(persisted.models[0].id, "generation-2", "{title}");

    let publish = captured.lock().expect("captured").take().expect("captured publisher");
    let stale = ModelsStoreEntry { models: vec![test_model("dynamic", "generation-1")], ..ModelsStoreEntry::default() };
    let accepted = publish(ModelsPublication { persist: Some(Some(stale)), update: None }).await.expect("late publish");
    assert!(!accepted, "{title}");
    let persisted = store.read("dynamic", None).await.expect("read").expect("entry");
    assert_eq!(persisted.models[0].id, "generation-2", "{title}");
}

// The remaining cases exercise provider-scoped `apiKey`/`oauth` auth
// callbacks through `models.getAuth`/`completeSimple`/`getAvailable`. In TS these dispatch
// through `auth/resolve.ts::resolveProviderAuth` against the provider's own `ProviderAuth`
// field; this crate's `Provider` trait has no `auth` field (todo 5's documented seam moves auth
// resolution to the `ModelsAuth` trait it injects into `Models` instead, deferring `auth/`'s
// real dispatch to todo 13). Each TS case is ported against that seam with a test-local
// `ModelsAuth` impl that reproduces the same `apiKey.resolve` callback behavior the TS case
// exercises, since `ModelsAuth::resolve`/`refresh_credential` is exactly the dispatch
// `apply_auth` and the refresh path call — this is genuinely todo-5 scope, not todo 13's.

type ResolveFn = Box<dyn Fn(&dyn Provider, &AuthResolutionOverrides) -> BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>> + Send + Sync>;
type RefreshCredentialFn =
    Box<dyn Fn(&dyn Provider, Option<&Credential>, &AbortSignal) -> BoxFuture<'static, Result<Option<Credential>, ModelsError>> + Send + Sync>;

struct CallbackAuth {
    resolve: ResolveFn,
    refresh_credential: RefreshCredentialFn,
}

impl ModelsAuth for CallbackAuth {
    fn resolve<'a>(&'a self, provider: &'a dyn Provider, overrides: &'a AuthResolutionOverrides) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        (self.resolve)(provider, overrides)
    }
    fn refresh_credential<'a>(&'a self, provider: &'a dyn Provider, stored: Option<&'a Credential>, signal: &'a AbortSignal) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        (self.refresh_credential)(provider, stored, signal)
    }
}

fn no_refresh_credential() -> RefreshCredentialFn {
    Box::new(|_p: &dyn Provider, _s: Option<&Credential>, _g: &AbortSignal| Box::pin(async { Ok(None) }))
}

fn ambient_auth() -> Arc<dyn ModelsAuth> {
    Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, _o: &AuthResolutionOverrides| Box::pin(async { Ok(Some(AuthResolution::default())) })),
        refresh_credential: Box::new(|_p: &dyn Provider, _s: Option<&Credential>, _g: &AbortSignal| {
            Box::pin(async { Ok(Some(serde_json::json!({ "type": "api_key" }))) })
        }),
    })
}

#[tokio::test]
async fn passes_caller_signals_to_provider_auth_callbacks() {
    let title = "passes caller signals to provider auth callbacks";
    let received: Arc<Mutex<Vec<AbortSignal>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(move |_p: &dyn Provider, o: &AuthResolutionOverrides| {
            let received = received_clone.clone();
            let signal = o.signal.clone().expect("signal");
            Box::pin(async move {
                received.lock().expect("received").push(signal);
                Ok(Some(AuthResolution { auth: ProviderAuthResult { api_key: Some("resolved".into()), ..Default::default() }, env: None }))
            }) as BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>>
        }),
        refresh_credential: no_refresh_credential(),
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));
    models.set_provider(plain_provider("p1", vec![]));

    let controller = AbortController::new();
    models.get_auth("p1", &AuthResolutionOverrides { signal: Some(controller.signal()), ..Default::default() }).await.expect("auth");

    let received = received.lock().expect("received");
    assert_eq!(received.len(), 1, "{title}");
    assert_eq!(received[0].reason(), controller.signal().reason(), "{title}");
}

#[tokio::test]
async fn stops_waiting_for_non_cooperative_auth_callbacks() {
    let title = "stops waiting for non-cooperative auth callbacks";
    let (start_tx, mut start_rx) = tokio::sync::watch::channel(false);
    let start_tx = Arc::new(start_tx);
    let (finish_tx, finish_rx) = tokio::sync::watch::channel(false);
    let finish_rx = Arc::new(Mutex::new(finish_rx));
    let start_tx_clone = start_tx.clone();
    let finish_rx_clone = finish_rx.clone();
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(move |_p: &dyn Provider, _o: &AuthResolutionOverrides| {
            let (start_tx, finish_rx) = (start_tx_clone.clone(), finish_rx_clone.clone());
            Box::pin(async move {
                start_tx.send_replace(true);
                let mut rx = finish_rx.lock().expect("rx").clone();
                let _ = rx.wait_for(|open| *open).await;
                Ok(Some(AuthResolution { auth: ProviderAuthResult { api_key: Some("key".into()), ..Default::default() }, env: None }))
            }) as BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>>
        }),
        refresh_credential: no_refresh_credential(),
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));
    models.set_provider(plain_provider("p1", vec![]));

    let auth_controller = AbortController::new();
    let models_for_spawn = models.clone();
    let signal = Some(auth_controller.signal());
    let pending = tokio::spawn(async move { models_for_spawn.get_auth("p1", &AuthResolutionOverrides { signal, ..Default::default() }).await });
    start_rx.wait_for(|started| *started).await.expect("resolve started");
    auth_controller.abort(None);

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), pending).await.expect("settles").expect("join");
    assert!(result.is_err(), "{title}");

    finish_tx.send_replace(true);
}

#[tokio::test]
async fn wraps_api_key_auth_failures_in_models_error() {
    let title = "wraps api-key auth failures in ModelsError";
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, _o: &AuthResolutionOverrides| Box::pin(async { Err(ModelsError::new(ModelsErrorCode::Auth, "nope")) })),
        refresh_credential: no_refresh_credential(),
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));
    models.set_provider(plain_provider("p1", vec![]));

    let error = models.get_auth("p1", &AuthResolutionOverrides::default()).await.expect_err("auth fails");
    assert_eq!(error.code, ModelsErrorCode::Auth, "{title}");
}

#[tokio::test]
async fn uses_explicit_request_api_key_and_env_during_provider_auth_resolution() {
    let title = "uses explicit request api key and env during provider auth resolution";
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, o: &AuthResolutionOverrides| {
            let account = o.env.as_ref().and_then(|e| e.get("ACCOUNT_ID").cloned());
            let key = o.api_key.clone();
            Box::pin(async move {
                let (Some(account), Some(key)) = (account, key) else { return Ok(None) };
                let env: ProviderEnv = [("ACCOUNT_ID".to_owned(), account.clone())].into_iter().collect();
                Ok(Some(AuthResolution {
                    auth: ProviderAuthResult { api_key: Some(key), base_url: Some(format!("https://example.test/{account}")), ..Default::default() },
                    env: Some(env),
                }))
            }) as BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>>
        }),
        refresh_credential: no_refresh_credential(),
    });
    let base = builtin("anthropic", "claude-opus-4-8");
    let model = with_provider(&base, "p1");
    let calls: RecordedCalls = Arc::new(Mutex::new(Vec::new()));
    let echo = Arc::new(RecordingEcho { calls: calls.clone() });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));
    models.set_provider(create_provider(CreateProviderOptions {
        id: "p1".into(),
        name: None,
        base_url: None,
        headers: None,
        models: vec![model.clone()],
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(echo),
    }));

    let mut options = SimpleStreamOptions::default();
    options.stream.request.api_key = Some("explicit-key".into());
    options.stream.request.env = Some([("ACCOUNT_ID".to_owned(), "acct".to_owned())].into_iter().collect());
    models.complete_simple(&model, &Context::default(), Some(options), ModelsRequestTransforms::default()).await.expect("result");

    let calls = calls.lock().expect("calls");
    assert_eq!(calls[0].0.base_url, "https://example.test/acct", "{title}");
    assert_eq!(calls[0].1.as_ref().and_then(|o| o.request.api_key.clone()), Some("explicit-key".into()), "{title}");
    let expected_env: ProviderEnv = [("ACCOUNT_ID".to_owned(), "acct".to_owned())].into_iter().collect();
    assert_eq!(calls[0].1.as_ref().and_then(|o| o.request.env.clone()), Some(expected_env), "{title}");
}

type RecordedCalls = Arc<Mutex<Vec<(Model, Option<StreamOptions>)>>>;

struct RecordingEcho {
    calls: RecordedCalls,
}

impl ProviderStreams for RecordingEcho {
    fn stream(&self, model: &Model, _context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        self.calls.lock().expect("calls").push((model.clone(), options.clone()));
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

#[tokio::test]
async fn merges_resolved_auth_into_stream_options_explicit_options_win_per_field() {
    let title = "merges resolved auth into stream options; explicit options win per field";
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, _o: &AuthResolutionOverrides| {
            Box::pin(async {
                let headers: ProviderHeaders = [
                    ("Authorization".to_owned(), Some("Bearer resolved-key".to_owned())),
                    ("x-a".to_owned(), Some("auth".to_owned())),
                    ("x-b".to_owned(), Some("auth".to_owned())),
                ]
                .into_iter()
                .collect();
                Ok(Some(AuthResolution {
                    auth: ProviderAuthResult { api_key: Some("resolved-key".into()), headers: Some(headers), base_url: Some("https://auth.test/v1".into()) },
                    env: None,
                }))
            }) as BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>>
        }),
        refresh_credential: no_refresh_credential(),
    });
    let base = builtin("anthropic", "claude-opus-4-8");
    let model = with_provider(&base, "p1");
    let calls: RecordedCalls = Arc::new(Mutex::new(Vec::new()));
    let echo = Arc::new(RecordingEcho { calls: calls.clone() });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));
    models.set_provider(create_provider(CreateProviderOptions {
        id: "p1".into(),
        name: None,
        base_url: None,
        headers: None,
        models: vec![model.clone()],
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(echo),
    }));

    let mut options = SimpleStreamOptions::default();
    options.stream.request.api_key = Some("explicit-key".into());
    options.stream.request.headers =
        Some([("authorization".to_owned(), Some("Explicit token".to_owned())), ("x-b".to_owned(), Some("explicit".to_owned()))].into_iter().collect());
    let result = models.complete_simple(&model, &Context::default(), Some(options), ModelsRequestTransforms::default()).await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Stop, "{title}");
    {
        let calls = calls.lock().expect("calls");
        assert_eq!(calls.len(), 1, "{title}");
        let expected_headers: ProviderHeaders = [
            ("authorization".to_owned(), Some("Explicit token".to_owned())),
            ("x-a".to_owned(), Some("auth".to_owned())),
            ("x-b".to_owned(), Some("explicit".to_owned())),
        ]
        .into_iter()
        .collect();
        assert_eq!(calls[0].1.as_ref().and_then(|o| o.request.api_key.clone()), Some("explicit-key".into()), "{title}");
        assert_eq!(calls[0].1.as_ref().and_then(|o| o.request.headers.clone()), Some(expected_headers), "{title}");
        assert_eq!(calls[0].0.base_url, "https://auth.test/v1", "{title}");
    }

    // without explicit options, resolved auth applies
    let result2 = models.complete_simple(&model, &Context::default(), None, ModelsRequestTransforms::default()).await.expect("result");
    assert_eq!(result2.stop_reason, StopReason::Stop, "{title}");
    let calls = calls.lock().expect("calls");
    assert_eq!(calls[1].1.as_ref().and_then(|o| o.request.api_key.clone()), Some("resolved-key".into()), "{title}");
}

#[tokio::test]
async fn adds_model_headers_only_for_model_auth_and_transforms_assembled_headers_once() {
    let title = "adds model headers only for model auth and transforms assembled headers once";
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, o: &AuthResolutionOverrides| {
            let key = o.api_key.clone();
            Box::pin(async move { Ok(key.map(|key| AuthResolution { auth: ProviderAuthResult { api_key: Some(key), ..Default::default() }, env: None })) })
                as BoxFuture<'static, Result<Option<AuthResolution>, ModelsError>>
        }),
        refresh_credential: no_refresh_credential(),
    });
    let base = builtin("anthropic", "claude-opus-4-8");
    let mut model = with_provider(&base, "p1");
    model.headers = Some([("x-model".to_owned(), "model".to_owned()), ("x-shared".to_owned(), "model".to_owned())].into_iter().collect());
    let calls: RecordedCalls = Arc::new(Mutex::new(Vec::new()));
    let echo = Arc::new(RecordingEcho { calls: calls.clone() });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));
    models.set_provider(create_provider(CreateProviderOptions {
        id: "p1".into(),
        name: None,
        base_url: None,
        headers: None,
        models: vec![model.clone()],
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(echo),
    }));

    let plain_overrides = AuthResolutionOverrides { api_key: Some("key".into()), ..Default::default() };
    assert_eq!(models.get_auth("p1", &plain_overrides).await.expect("auth").and_then(|r| r.auth.headers), None, "{title}");
    let model_auth = models.get_auth_for_model(&model, &plain_overrides).await.expect("auth").expect("resolution");
    let model_headers: ProviderHeaders =
        [("x-model".to_owned(), Some("model".to_owned())), ("x-shared".to_owned(), Some("model".to_owned()))].into_iter().collect();
    assert_eq!(model_auth.auth.headers, Some(model_headers), "{title}");

    let transforms_ran = Arc::new(AtomicUsize::new(0));
    let transforms_ran_clone = transforms_ran.clone();
    let transforms = ModelsRequestTransforms {
        transform_headers: Some(Arc::new(move |headers: ProviderHeaders| {
            transforms_ran_clone.fetch_add(1, Ordering::SeqCst);
            let expected: ProviderHeaders = [
                ("x-model".to_owned(), Some("model".to_owned())),
                ("x-explicit".to_owned(), Some("explicit".to_owned())),
                ("X-Shared".to_owned(), Some("explicit".to_owned())),
            ]
            .into_iter()
            .collect();
            assert_eq!(headers, expected, "transform receives assembled headers exactly once");
            Box::pin(async move {
                let mut merged = headers;
                merged.insert("x-transformed".to_owned(), Some("yes".to_owned()));
                merged
            }) as crate::types::BoxFuture<'static, ProviderHeaders>
        })),
    };
    let mut options = SimpleStreamOptions::default();
    options.stream.request.api_key = Some("key".into());
    options.stream.request.headers =
        Some([("x-explicit".to_owned(), Some("explicit".to_owned())), ("X-Shared".to_owned(), Some("explicit".to_owned()))].into_iter().collect());
    models.complete_simple(&model, &Context::default(), Some(options), transforms).await.expect("result");

    assert_eq!(transforms_ran.load(Ordering::SeqCst), 1, "{title}");
    let calls = calls.lock().expect("calls");
    let expected_headers: ProviderHeaders = [
        ("x-model".to_owned(), Some("model".to_owned())),
        ("x-explicit".to_owned(), Some("explicit".to_owned())),
        ("X-Shared".to_owned(), Some("explicit".to_owned())),
        ("x-transformed".to_owned(), Some("yes".to_owned())),
    ]
    .into_iter()
    .collect();
    assert_eq!(calls[0].1.as_ref().and_then(|o| o.request.headers.clone()), Some(expected_headers), "{title}");
}

#[tokio::test]
async fn produces_an_error_stream_for_unknown_providers_instead_of_throwing() {
    let title = "produces an error stream for unknown providers instead of throwing";
    let models = create_models(None);
    let base = builtin("anthropic", "claude-opus-4-8");
    let ghost = with_provider(&base, "ghost");
    let result = models.complete_simple(&ghost, &Context::default(), None, ModelsRequestTransforms::default()).await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Error, "{title}");
    assert!(result.error_message.as_deref().is_some_and(|m| m.contains("Unknown provider: ghost")), "{title}");
}

#[tokio::test]
async fn streams_through_the_provider() {
    let title = "streams through the provider";
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(ambient_auth()) }));
    let base = builtin("anthropic", "claude-opus-4-8");
    let model = with_provider(&base, "p1");
    models.set_provider(plain_provider("p1", vec![model.clone()]));

    let mut events = Vec::new();
    let stream = models.stream_simple(&model, &Context::default(), None, ModelsRequestTransforms::default());
    loop {
        match stream.next().await {
            Ok(Some(event)) => events.push(match event {
                AssistantMessageEvent::Start { .. } => "start",
                AssistantMessageEvent::Done { .. } => "done",
                _ => "other",
            }),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(events, vec!["start", "done"], "{title}");
    let message = stream.result().await.expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop, "{title}");
}

#[tokio::test]
async fn applies_request_wide_pricing_tiers_above_the_configured_input_threshold() {
    let title = "applies request-wide pricing tiers above the configured input threshold";
    let mut model = with_provider(&builtin("openai", "gpt-6-astra"), "openai");
    model.cost = ModelCost {
        input: 5.0,
        output: 30.0,
        cache_read: 0.5,
        cache_write: 6.25,
        tiers: Some(vec![ModelCostTier {
            input: 10.0,
            output: 45.0,
            cache_read: 1.0,
            cache_write: 12.5,
            input_tokens_above: 272_000,
        }]),
    };
    let usage = |cache_write: u64| Usage {
        input: 200_000,
        output: 100_000,
        cache_read: 72_000,
        cache_write,
        total_tokens: 372_000 + cache_write,
        ..Usage::default()
    };

    // 200000 + 72000 + 0 lands exactly on the threshold; the tier needs strictly more input.
    let mut short = usage(0);
    let short_cost = calculate_cost(&model, &mut short);
    assert_eq!(short_cost.input, 1.0, "{title}");
    assert_eq!(short_cost.output, 3.0, "{title}");
    assert_eq!(short_cost.cache_read, 0.036, "{title}");
    assert_eq!(short_cost.cache_write, 0.0, "{title}");

    let mut long = usage(1);
    let long_cost = calculate_cost(&model, &mut long);
    assert_eq!(long_cost.input, 2.0, "{title}");
    assert_eq!(long_cost.output, 4.5, "{title}");
    assert_eq!(long_cost.cache_read, 0.072, "{title}");
    assert_eq!(long_cost.cache_write, 0.000_012_5, "{title}");
}

#[tokio::test]
async fn restores_cached_models_before_waiting_for_network_auth() {
    let title = "restores cached models before waiting for network auth";
    let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
    let mut cached = with_provider(&builtin("openai", "gpt-6-astra"), "dynamic");
    cached.id = "cached".into();
    store
        .write("dynamic", &ModelsStoreEntry { models: vec![cached], ..ModelsStoreEntry::default() }, None)
        .await
        .expect("seed store");

    let (auth_started_tx, mut auth_started_rx) = tokio::sync::watch::channel(false);
    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, _o: &AuthResolutionOverrides| Box::pin(async { Ok(Some(AuthResolution::default())) })),
        refresh_credential: Box::new(move |_p: &dyn Provider, _s: Option<&Credential>, _g: &AbortSignal| {
            auth_started_tx.send_replace(true);
            let mut gate_rx = gate_rx.clone();
            Box::pin(async move {
                let _ = gate_rx.wait_for(|open| *open).await;
                Ok(Some(serde_json::json!({ "type": "api_key", "key": "key" })))
            }) as BoxFuture<'static, Result<Option<Credential>, ModelsError>>
        }),
    });
    let fetch: FetchModels =
        Arc::new(|_ctx| Box::pin(async { Err(ModelsError::new(ModelsErrorCode::ModelSource, "must not fetch")) }));
    let models = create_models(Some(CreateModelsOptions { models_store: Some(store), auth: Some(auth) }));
    models.set_provider(refreshable_provider("dynamic", vec![], fetch));

    let caller = AbortController::new();
    let models_for_spawn = models.clone();
    let signal = Some(caller.signal());
    let pending = tokio::spawn(async move {
        models_for_spawn.refresh(ModelsRefreshOptions { providers: Some(vec!["dynamic".into()]), signal, ..ModelsRefreshOptions::default() }).await
    });
    auth_started_rx.wait_for(|started| *started).await.expect("auth started");

    assert!(models.get_model("dynamic", "cached").is_some(), "{title}");
    caller.abort(None);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), pending).await.expect("refresh settles").expect("join");
    assert!(result.aborted, "{title}");
    assert_eq!(result.errors.len(), 0, "{title}");
    gate_tx.send_replace(true);
}

#[tokio::test]
async fn passes_effective_api_key_credentials_and_refresh_options_while_skipping_unconfigured_providers() {
    let title = "passes effective API-key credentials and refresh options while skipping unconfigured providers";
    let effective: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));
    let forced: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let unconfigured_refreshes = Arc::new(AtomicUsize::new(0));
    let auth: Arc<dyn ModelsAuth> = Arc::new(CallbackAuth {
        resolve: Box::new(|_p: &dyn Provider, _o: &AuthResolutionOverrides| Box::pin(async { Ok(Some(AuthResolution::default())) })),
        refresh_credential: Box::new(|provider: &dyn Provider, _s: Option<&Credential>, _g: &AbortSignal| {
            let configured = provider.id() == "configured";
            Box::pin(async move { Ok(configured.then(|| serde_json::json!({ "type": "api_key", "key": "ambient-key" }))) })
                as BoxFuture<'static, Result<Option<Credential>, ModelsError>>
        }),
    });
    let models = create_models(Some(CreateModelsOptions { models_store: None, auth: Some(auth) }));

    let (seen_credential, seen_force) = (effective.clone(), forced.clone());
    let fetch: FetchModels = Arc::new(move |ctx: &RefreshModelsContext| {
        if ctx.allow_network {
            *seen_credential.lock().expect("credential") = ctx.credential.clone();
            *seen_force.lock().expect("force") = ctx.force;
        }
        Box::pin(async { Ok(vec![]) })
    });
    models.set_provider(refreshable_provider("configured", vec![], fetch));

    let unconfigured = unconfigured_refreshes.clone();
    let fetch: FetchModels = Arc::new(move |ctx: &RefreshModelsContext| {
        if ctx.allow_network {
            unconfigured.fetch_add(1, Ordering::SeqCst);
        }
        Box::pin(async { Ok(vec![]) })
    });
    models.set_provider(refreshable_provider("unconfigured", vec![], fetch));

    let result = models.refresh(ModelsRefreshOptions { force: Some(true), ..ModelsRefreshOptions::default() }).await;
    assert_eq!(result.errors.len(), 0, "{title}");
    let credential = effective.lock().expect("credential").clone().expect("credential passed to the network phase");
    assert_eq!(credential["type"], "api_key", "{title}");
    assert_eq!(credential["key"], "ambient-key", "{title}");
    assert_eq!(*forced.lock().expect("force"), Some(true), "{title}");
    assert_eq!(unconfigured_refreshes.load(Ordering::SeqCst), 0, "{title}");
}
// senpi test/supports-xhigh.test.ts (30 `it()` cases plus 43 `it.each` rows = 73 cases) — the
// thinking-level surface `models.ts` owns and `compat.ts` re-exports.

fn level(name: &str) -> L {
    match name {
        "off" => L::Off,
        "minimal" => L::Minimal,
        "low" => L::Low,
        "medium" => L::Medium,
        "high" => L::High,
        "xhigh" => L::Xhigh,
        "max" => L::Max,
        other => panic!("unknown thinking level {other}"),
    }
}

fn levels(names: &[&str]) -> Vec<L> {
    names.iter().map(|name| level(name)).collect()
}

/// `maplessModel(api, id)` from the TS fixture: a custom-provider model with no thinkingLevelMap.
fn mapless_model(api: &str, id: &str) -> Model {
    let mut model = builtin("anthropic", "claude-opus-4-8");
    model.id = id.to_owned();
    model.api = api.to_owned();
    model.provider = "codex-lb".to_owned();
    model.base_url = "https://example.invalid".to_owned();
    model.reasoning = true;
    model.thinking_level_map = None;
    model
}

enum Check {
    Exact(&'static [&'static str]),
    Has(&'static [&'static str]),
    Lacks(&'static [&'static str]),
}

fn assert_levels(title: &str, model: &Model, checks: &[Check]) {
    let actual = get_supported_thinking_levels(model);
    for check in checks {
        match check {
            Check::Exact(names) => assert_eq!(actual, levels(names), "{title}"),
            Check::Has(names) => {
                for name in *names {
                    assert!(actual.contains(&level(name)), "{title}: missing {name}");
                }
            }
            Check::Lacks(names) => {
                for name in *names {
                    assert!(!actual.contains(&level(name)), "{title}: unexpected {name}");
                }
            }
        }
    }
}

struct CatalogCase {
    title: &'static str,
    provider: &'static str,
    ids: &'static [&'static str],
    checks: &'static [Check],
}

const CATALOG_CASES: &[CatalogCase] = &[
    CatalogCase {
        title: "includes max but not xhigh for Anthropic Opus 4.6 on anthropic-messages API",
        provider: "anthropic",
        ids: &["claude-opus-4-6"],
        checks: &[Check::Has(&["max"]), Check::Lacks(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh and max for Anthropic Opus 4.8 on anthropic-messages API",
        provider: "anthropic",
        ids: &["claude-opus-4-8"],
        checks: &[Check::Has(&["xhigh", "max"])],
    },
    CatalogCase {
        title: "includes xhigh and max for Anthropic Opus 5 on anthropic-messages API",
        provider: "anthropic",
        ids: &["claude-opus-5"],
        checks: &[Check::Has(&["xhigh", "max"])],
    },
    CatalogCase {
        title: "includes max but not xhigh for Anthropic Sonnet 4.6 on anthropic-messages API",
        provider: "anthropic",
        ids: &["claude-sonnet-4-6"],
        checks: &[Check::Has(&["max"]), Check::Lacks(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh and max for Anthropic Sonnet 5 on anthropic-messages API",
        provider: "anthropic",
        ids: &["claude-sonnet-5"],
        checks: &[Check::Has(&["xhigh", "max"])],
    },
    CatalogCase {
        title: "includes off, xhigh and max for Anthropic Fable 5 on anthropic-messages API",
        provider: "anthropic",
        ids: &["claude-fable-5"],
        checks: &[Check::Has(&["off", "xhigh", "max"])],
    },
    CatalogCase {
        title: "does not include xhigh or max for Claude Sonnet 4.5",
        provider: "anthropic",
        ids: &["claude-sonnet-4-5"],
        checks: &[Check::Lacks(&["xhigh", "max"])],
    },
    CatalogCase {
        title: "includes xhigh for openai-codex gpt-5.5 models",
        provider: "chatgpt-subscription",
        ids: &["gpt-5.5"],
        checks: &[Check::Has(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh for openai-codex gpt-5.6-sol models",
        provider: "chatgpt-subscription",
        ids: &["gpt-5.6-sol"],
        checks: &[Check::Has(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh for openai-codex gpt-5.6-terra models",
        provider: "chatgpt-subscription",
        ids: &["gpt-5.6-terra"],
        checks: &[Check::Has(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh for openai-codex gpt-5.6-luna models",
        provider: "chatgpt-subscription",
        ids: &["gpt-5.6-luna"],
        checks: &[Check::Has(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh for openai-codex gpt-6-astra models",
        provider: "chatgpt-subscription",
        ids: &["gpt-6-astra"],
        checks: &[Check::Has(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh and max for OpenAI gpt-5.6-sol models",
        provider: "openai",
        ids: &["gpt-5.6-sol"],
        checks: &[Check::Exact(&["off", "minimal", "low", "medium", "high", "xhigh", "max"])],
    },
    CatalogCase {
        title: "includes xhigh and max for OpenAI gpt-5.6-terra models",
        provider: "openai",
        ids: &["gpt-5.6-terra"],
        checks: &[Check::Exact(&["off", "minimal", "low", "medium", "high", "xhigh", "max"])],
    },
    CatalogCase {
        title: "includes xhigh and max for OpenAI gpt-5.6-luna models",
        provider: "openai",
        ids: &["gpt-5.6-luna"],
        checks: &[Check::Exact(&["off", "minimal", "low", "medium", "high", "xhigh", "max"])],
    },
    CatalogCase {
        title: "includes only medium/high/xhigh for OpenAI GPT-5.5 Pro",
        provider: "openai",
        ids: &["gpt-5.5-pro"],
        checks: &[Check::Exact(&["medium", "high", "xhigh"])],
    },
    CatalogCase {
        title: "includes only medium/high/xhigh for OpenRouter GPT-5.5 Pro",
        provider: "openrouter",
        ids: &["openai/gpt-5.5-pro"],
        checks: &[Check::Exact(&["medium", "high", "xhigh"])],
    },
    CatalogCase {
        title: "includes low/high/max plus off for DeepSeek V4.1 Flash on the DeepSeek provider",
        provider: "deepseek",
        ids: &["deepseek-flash"],
        checks: &[Check::Exact(&["off", "low", "high", "max"])],
    },
    CatalogCase {
        title: "includes low/high/max plus off for DeepSeek V4 Flash on opencode-go",
        provider: "opencode-go",
        ids: &["deepseek-v4-flash"],
        checks: &[Check::Exact(&["off", "low", "high", "max"])],
    },
    CatalogCase {
        title: "includes only high plus off for OpenCode Go Kimi K2.6",
        provider: "opencode-go",
        ids: &["kimi-k2.6"],
        checks: &[Check::Exact(&["off", "high"])],
    },
    CatalogCase {
        title: "excludes thinking off for Moonshot Kimi K2.7 Code models",
        provider: "moonshotai",
        ids: &["kimi-k2.7-code"],
        checks: &[Check::Exact(&["minimal", "low", "medium", "high"])],
    },
    CatalogCase {
        title: "excludes thinking off for Moonshot Kimi K2.7 Code models",
        provider: "moonshotai-cn",
        ids: &["kimi-k2.7-code"],
        checks: &[Check::Exact(&["minimal", "low", "medium", "high"])],
    },
    CatalogCase {
        title: "uses the verified effort options for moonshotai Kimi K3",
        provider: "moonshotai",
        ids: &["kimi-k3"],
        checks: &[Check::Exact(&["low", "high", "max"])],
    },
    CatalogCase {
        title: "uses the verified effort options for moonshotai-cn Kimi K3",
        provider: "moonshotai-cn",
        ids: &["kimi-k3"],
        checks: &[Check::Exact(&["low", "high", "max"])],
    },
    CatalogCase {
        title: "includes only high for OpenCode Grok Build",
        provider: "opencode",
        ids: &["grok-build-0.1"],
        checks: &[Check::Exact(&["high"])],
    },
    CatalogCase {
        title: "includes only high/xhigh plus off for DeepSeek V4 Flash on OpenRouter",
        provider: "openrouter",
        ids: &["deepseek/deepseek-v4-flash"],
        checks: &[Check::Exact(&["off", "high", "xhigh"])],
    },
    CatalogCase {
        title: "includes max but not xhigh for OpenRouter Opus 4.6 (openai-completions API)",
        provider: "openrouter",
        ids: &["anthropic/claude-opus-4.6"],
        checks: &[Check::Has(&["max"]), Check::Lacks(&["xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh and max for Bedrock Claude Opus 5",
        provider: "amazon-bedrock",
        ids: &["global.anthropic.claude-opus-5"],
        checks: &[Check::Has(&["xhigh", "max"])],
    },
    CatalogCase {
        title: "includes xhigh but not off or max for xAI Grok 4.6",
        provider: "xai",
        ids: &["grok-4.6"],
        checks: &[Check::Exact(&["low", "medium", "high", "xhigh"])],
    },
    CatalogCase {
        title: "includes xhigh and max but not off for Bedrock Claude Fable 5",
        provider: "amazon-bedrock",
        ids: &["global.anthropic.claude-fable-5"],
        checks: &[Check::Has(&["xhigh", "max"]), Check::Lacks(&["off"])],
    },
];

#[test]
fn get_supported_thinking_levels_matches_senpi_for_catalog_models() {
    for case in CATALOG_CASES {
        for id in case.ids {
            let model = get_builtin_model(case.provider, id)
                .unwrap_or_else(|_| panic!("{}: missing {}/{}", case.title, case.provider, id))
                .clone();
            assert_levels(case.title, &model, case.checks);
        }
    }
}

#[test]
fn delegates_extended_tier_precedence_to_the_exported_capability_predicates() {
    let title = "delegates extended-tier precedence to the exported capability predicates";
    // The TS case pins models.ts's source text: the reported extended tiers must come from the
    // exported predicates rather than a separate model-id check. Rust's static analogue is that
    // `get_supported_thinking_levels` routes xhigh/max through `supports_xhigh`/`supports_max`,
    // which this cross-check observes for both mapped and map-less models.
    for (provider, id) in [("anthropic", "claude-opus-4-8"), ("openai", "gpt-6-astra"), ("openai", "gpt-5.5-pro"), ("xai", "grok-4.6")] {
        let model = builtin(provider, id);
        let reported = get_supported_thinking_levels(&model);
        assert_eq!(reported.contains(&L::Xhigh), supports_xhigh(&model), "{title} ({provider}/{id})");
        assert_eq!(reported.contains(&L::Max), supports_max(&model), "{title} ({provider}/{id})");
    }
    for (api, id) in [("openai-responses", "gpt-5.6-sol"), ("anthropic-messages", "gpt-5.6-terra")] {
        let model = mapless_model(api, id);
        let reported = get_supported_thinking_levels(&model);
        assert_eq!(reported.contains(&L::Xhigh), supports_xhigh(&model), "{title} ({api}/{id})");
        assert_eq!(reported.contains(&L::Max), supports_max(&model), "{title} ({api}/{id})");
    }
}

struct MaplessCase {
    title: &'static str,
    api: &'static str,
    id: &'static str,
    reasoning: bool,
    map: Option<&'static [(&'static str, Option<&'static str>)]>,
    xhigh: Option<bool>,
    max: Option<bool>,
    checks: &'static [Check],
}

const MAPLESS_CASES: &[MaplessCase] = &[
    MaplessCase {
        title: "detects the xhigh tier for map-less ",
        api: "anthropic-messages",
        id: "claude-opus-5",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: None,
        checks: &[],
    },
    MaplessCase {
        title: "detects the xhigh tier for map-less ",
        api: "anthropic-messages",
        id: "claude-sonnet-5",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: None,
        checks: &[],
    },
    MaplessCase {
        title: "detects the xhigh tier for map-less ",
        api: "anthropic-messages",
        id: "claude-fable-5",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: None,
        checks: &[],
    },
    MaplessCase {
        title: "detects the xhigh tier for map-less gpt-5.6-sol",
        api: "anthropic-messages",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: None,
        checks: &[],
    },
    MaplessCase {
        title: "still reports no xhigh tier for a map-less Sonnet 4.5",
        api: "anthropic-messages",
        id: "claude-sonnet-4-5",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: None,
        checks: &[],
    },
    MaplessCase {
        title: "includes max for a map-less gpt-5.6-sol model",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: None,
        checks: &[Check::Has(&["max"])],
    },
    MaplessCase {
        title: "does not infer max for a map-less gpt-5.6-terra model",
        api: "openai-responses",
        id: "gpt-5.6-terra",
        reasoning: true,
        map: None,
        xhigh: None,
        max: None,
        checks: &[Check::Lacks(&["max"])],
    },
    MaplessCase {
        title: "infers max for a map-less gpt-5.6-sol model on openai-responses",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[Check::Has(&["max"])],
    },
    MaplessCase {
        title: "infers max for a map-less gpt-5.6-sol model on azure-openai-responses",
        api: "azure-openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[Check::Has(&["max"])],
    },
    MaplessCase {
        title: "infers max for a map-less gpt-5.6-sol model on openai-codex-responses",
        api: "openai-codex-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[Check::Has(&["max"])],
    },
    MaplessCase {
        title: "infers max for a map-less gpt-5.6-sol model on openai-completions",
        api: "openai-completions",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[Check::Has(&["max"])],
    },
    MaplessCase {
        title: "infers max for the map-less Sol variant gpt-5.6-sol-fast",
        api: "openai-responses",
        id: "gpt-5.6-sol-fast",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "infers max for the map-less Sol variant openai/gpt-5.6-sol",
        api: "openai-responses",
        id: "openai/gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "rejects gpt-5.6-solar",
        api: "openai-responses",
        id: "gpt-5.6-solar",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "rejects gpt-5.6-solaris",
        api: "openai-responses",
        id: "gpt-5.6-solaris",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "rejects my-gpt-5.6-sol",
        api: "openai-responses",
        id: "my-gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "rejects xgpt-5.6-sol",
        api: "openai-responses",
        id: "xgpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "rejects legacy-gpt-5.6-solstice",
        api: "openai-responses",
        id: "legacy-gpt-5.6-solstice",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for my-gpt-5.60",
        api: "openai-responses",
        id: "my-gpt-5.60",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for notopus-5ive",
        api: "openai-responses",
        id: "notopus-5ive",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for opus-50",
        api: "openai-responses",
        id: "opus-50",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for not-sonnet-500",
        api: "openai-responses",
        id: "not-sonnet-500",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for xgpt-5.2y",
        api: "openai-responses",
        id: "xgpt-5.2y",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for gpt-5.6-solar",
        api: "openai-responses",
        id: "gpt-5.6-solar",
        reasoning: true,
        map: None,
        xhigh: Some(false),
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for GPT-5.6-SOL",
        api: "openai-responses",
        id: "GPT-5.6-SOL",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for openai/gpt-5.6-sol",
        api: "openai-responses",
        id: "openai/gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "matches model-family boundaries for quotio-openai/gpt-5.6-sol-fast",
        api: "openai-responses",
        id: "quotio-openai/gpt-5.6-sol-fast",
        reasoning: true,
        map: None,
        xhigh: Some(true),
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less gpt-5.6-sol model on a non-OpenAI-compatible api",
        api: "anthropic-messages",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less non-Sol gpt-5.6-terra model",
        api: "openai-responses",
        id: "gpt-5.6-terra",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less non-Sol gpt-5.6-luna model",
        api: "openai-responses",
        id: "gpt-5.6-luna",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less non-Sol gpt-5.6 model",
        api: "openai-responses",
        id: "gpt-5.6",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less non-Sol gpt-5.5 model",
        api: "openai-responses",
        id: "gpt-5.5",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less non-Sol upstage/solar-pro-3 model",
        api: "openai-responses",
        id: "upstage/solar-pro-3",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "does not infer max for a map-less non-reasoning gpt-5.6-sol model",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: false,
        map: None,
        xhigh: None,
        max: Some(false),
        checks: &[],
    },
    MaplessCase {
        title: "treats an empty thinking-level map as authoritative",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: Some(&[]),
        xhigh: Some(false),
        max: Some(false),
        checks: &[Check::Lacks(&["xhigh", "max"])],
    },
    MaplessCase {
        title: "honors an explicit max veto on gpt-5.6-sol",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: Some(&[("max", None)]),
        xhigh: Some(false),
        max: Some(false),
        checks: &[Check::Lacks(&["xhigh", "max"])],
    },
    MaplessCase {
        title: "treats a map omitting max as authoritative for gpt-5.6-sol",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: Some(&[("xhigh", Some("xhigh"))]),
        xhigh: None,
        max: Some(false),
        checks: &[Check::Lacks(&["max"]), Check::Has(&["xhigh"])],
    },
    MaplessCase {
        title: "treats a map omitting xhigh as authoritative for gpt-5.6-sol",
        api: "openai-responses",
        id: "gpt-5.6-sol",
        reasoning: true,
        map: Some(&[("max", Some("max"))]),
        xhigh: Some(false),
        max: Some(true),
        checks: &[Check::Lacks(&["xhigh"]), Check::Has(&["max"])],
    },
    MaplessCase {
        title: "keeps inferring max for map-less Anthropic ",
        api: "anthropic-messages",
        id: "claude-opus-4-8",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "keeps inferring max for map-less Anthropic ",
        api: "anthropic-messages",
        id: "claude-opus-5",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "keeps inferring max for map-less Anthropic ",
        api: "anthropic-messages",
        id: "claude-sonnet-5",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[],
    },
    MaplessCase {
        title: "keeps inferring max for map-less Anthropic ",
        api: "anthropic-messages",
        id: "claude-fable-5",
        reasoning: true,
        map: None,
        xhigh: None,
        max: Some(true),
        checks: &[],
    },
];

#[test]
fn supports_xhigh_and_supports_max_match_senpi_for_map_less_models() {
    for case in MAPLESS_CASES {
        let mut model = mapless_model(case.api, case.id);
        model.reasoning = case.reasoning;
        model.thinking_level_map =
            case.map.map(|entries| entries.iter().map(|(name, value)| (level(name), value.map(str::to_owned))).collect());
        if let Some(expected) = case.xhigh {
            assert_eq!(supports_xhigh(&model), expected, "{}: supportsXhigh", case.title);
        }
        if let Some(expected) = case.max {
            assert_eq!(supports_max(&model), expected, "{}: supportsMax", case.title);
        }
        assert_levels(case.title, &model, case.checks);
    }
}

// senpi test/max-thinking.test.ts cases whose subject is this module's helper surface. The two
// `it.each("sends max to the Codex Responses API for %s")` cases in that file drive
// `api/openai-codex-responses.ts` payloads and are owned by the api todos.

#[test]
fn is_opt_in_for_ordinary_reasoning_models() {
    let title = "is opt-in for ordinary reasoning models";
    let model = mapless_model("openai-completions", "ordinary-reasoning");
    assert_eq!(get_supported_thinking_levels(&model), levels(&["off", "minimal", "low", "medium", "high"]), "{title}");
    assert_eq!(clamp_thinking_level(&model, L::Max), L::High, "{title}");
}

#[test]
fn supports_a_hole_between_high_and_max() {
    let title = "supports a hole between high and max";
    let mut model = mapless_model("openai-completions", "high-and-max");
    model.thinking_level_map = Some([(L::Xhigh, None), (L::Max, Some("max".to_owned()))].into_iter().collect());
    assert_eq!(get_supported_thinking_levels(&model), levels(&["off", "minimal", "low", "medium", "high", "max"]), "{title}");
    assert_eq!(clamp_thinking_level(&model, L::Xhigh), L::Max, "{title}");
}
