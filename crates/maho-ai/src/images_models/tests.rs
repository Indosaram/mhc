use super::*;
use crate::models::ProviderAuthResult;
use crate::types::{ContentBlock, ImagesStopReason, ProviderEnv};
use std::sync::atomic::{AtomicUsize, Ordering};

fn test_image_model(provider: &str, id: &str) -> ImagesModel {
    let mut model = crate::image_models::get_image_model("openai", "gpt-image-2.5-flare").expect("model").clone();
    model.id = id.into();
    model.name = id.into();
    model.api = "test-images".into();
    model.provider = provider.into();
    model.base_url = "https://example.test/v1".into();
    model
}

fn ok_result(model: &ImagesModel) -> AssistantImages {
    let mut result = images_error(model, String::new());
    result.stop_reason = ImagesStopReason::Stop;
    result.error_message = None;
    result
}

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(ImagesModel, Option<ImagesOptions>)>>,
}

impl ProviderImages for Recorder {
    fn generate_images<'a>(&'a self, model: &'a ImagesModel, _c: &'a ImagesContext, options: Option<ImagesOptions>) -> BoxFuture<'a, AssistantImages> {
        self.calls.lock().expect("calls").push((model.clone(), options));
        Box::pin(async move { ok_result(model) })
    }
}

fn env_auth(env: &'static [(&'static str, &'static str)], var: Option<&'static str>) -> ResolveImagesAuth {
    Arc::new(move |overrides: &AuthResolutionOverrides| {
        let explicit = overrides.api_key.clone();
        Box::pin(async move {
            let Some(var) = var else { return Ok(Some(AuthResolution::default())) };
            let key = explicit.or_else(|| env.iter().find(|(k, _)| *k == var).map(|(_, v)| (*v).to_owned()));
            Ok(key.map(|key| AuthResolution { auth: ProviderAuthResult { api_key: Some(key), ..Default::default() }, env: None }))
        })
    })
}

fn provider(id: &str, models: Vec<ImagesModel>, auth: ResolveImagesAuth, api: Arc<Recorder>) -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions { id: id.into(), name: None, auth, models, refresh_models: None, api })
}

fn context() -> ImagesContext {
    ImagesContext { input: vec![ContentBlock::text("a red circle")] }
}

#[test]
fn registers_providers_and_reads_models_synchronously() {
    let title = "registers providers and reads models synchronously";
    let models = create_images_models();
    let api = Arc::new(Recorder::default());
    models.set_provider(provider("p1", vec![test_image_model("p1", "m1"), test_image_model("p1", "m2")], env_auth(&[], None), api.clone()));
    models.set_provider(provider("p2", vec![test_image_model("p2", "m3")], env_auth(&[], None), api));
    assert_eq!(models.get_providers().iter().map(|p| p.id().to_owned()).collect::<Vec<_>>(), ["p1", "p2"], "{title}");
    assert_eq!(models.get_models(None).iter().map(|m| m.id.clone()).collect::<Vec<_>>(), ["m1", "m2", "m3"], "{title}");
    assert_eq!(models.get_models(Some("p1")).iter().map(|m| m.id.clone()).collect::<Vec<_>>(), ["m1", "m2"], "{title}");
    assert_eq!(models.get_model("p2", "m3").map(|m| m.id), Some("m3".into()), "{title}");
    assert!(models.get_model("p2", "missing").is_none(), "{title}");
    models.delete_provider("p1");
    assert!(models.get_provider("p1").is_none(), "{title}");
}

#[tokio::test]
async fn resolves_auth_through_the_provider_and_merges_it_into_requests_explicit_options_win() {
    let title = "resolves auth through the provider and merges it into requests; explicit options win";
    let models = create_images_models();
    let api = Arc::new(Recorder::default());
    models.set_provider(provider("p1", vec![test_image_model("p1", "model-a")], env_auth(&[("TEST_KEY", "env-key")], Some("TEST_KEY")), api.clone()));
    let model = models.get_model("p1", "model-a").expect("model");
    let key = |r: Option<AuthResolution>| r.and_then(|r| r.auth.api_key);
    assert_eq!(key(models.get_auth("p1", &AuthResolutionOverrides::default()).await.expect("auth")), Some("env-key".into()), "{title}");
    let explicit = AuthResolutionOverrides { api_key: Some("explicit-key".into()), ..Default::default() };
    assert_eq!(key(models.get_auth("p1", &explicit).await.expect("auth")), Some("explicit-key".into()), "{title}");

    assert_eq!(models.generate_images(&model, &context(), None).await.stop_reason, ImagesStopReason::Stop, "{title}");
    let mut options = ImagesOptions::default();
    options.request.api_key = Some("explicit".into());
    models.generate_images(&model, &context(), Some(options)).await;
    let calls = api.calls.lock().expect("calls");
    assert_eq!(calls[0].1.as_ref().and_then(|o| o.request.api_key.clone()), Some("env-key".into()), "{title}");
    assert_eq!(calls[1].1.as_ref().and_then(|o| o.request.api_key.clone()), Some("explicit".into()), "{title}");
}

#[tokio::test]
async fn merges_provider_resolved_env_into_image_options() {
    let title = "merges provider-resolved env into image options";
    let models = create_images_models();
    let api = Arc::new(Recorder::default());
    let auth: ResolveImagesAuth = Arc::new(|_| {
        Box::pin(async {
            let env: ProviderEnv = [("PROVIDER_ONLY", "provider"), ("SHARED", "provider")].iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
            Ok(Some(AuthResolution { auth: ProviderAuthResult { api_key: Some("provider-key".into()), ..Default::default() }, env: Some(env) }))
        })
    });
    models.set_provider(provider("p1", vec![test_image_model("p1", "model-a")], auth, api.clone()));
    let model = models.get_model("p1", "model-a").expect("model");
    let mut options = ImagesOptions::default();
    options.request.api_key = Some("request-key".into());
    options.request.env = Some([("REQUEST_ONLY", "request"), ("SHARED", "request")].iter().map(|(k, v)| ((*k).into(), (*v).into())).collect());
    models.generate_images(&model, &context(), Some(options)).await;
    let calls = api.calls.lock().expect("calls");
    let request = &calls[0].1.as_ref().expect("options").request;
    assert_eq!(request.api_key.as_deref(), Some("request-key"), "{title}");
    let expected: ProviderEnv =
        [("PROVIDER_ONLY", "provider"), ("REQUEST_ONLY", "request"), ("SHARED", "request")].iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
    assert_eq!(request.env, Some(expected), "{title}");
}

#[tokio::test]
async fn returns_an_error_result_for_unknown_providers_and_unconfigured_auth_rejections() {
    let title = "returns an error result for unknown providers and unconfigured auth rejections";
    let models = create_images_models();
    let ghost = models.generate_images(&test_image_model("ghost", "m"), &context(), None).await;
    assert_eq!(ghost.stop_reason, ImagesStopReason::Error, "{title}");
    assert!(ghost.error_message.as_deref().is_some_and(|m| m.contains("Unknown provider: ghost")), "{title}");
    assert!(ghost.output.is_empty(), "{title}");

    let api = Arc::new(Recorder::default());
    models.set_provider(provider("p1", vec![test_image_model("p1", "model-a")], env_auth(&[], Some("MISSING")), api.clone()));
    let model = models.get_model("p1", "model-a").expect("model");
    assert_eq!(models.get_auth("p1", &AuthResolutionOverrides::default()).await, Ok(None), "{title}");
    models.generate_images(&model, &context(), None).await;
    assert_eq!(api.calls.lock().expect("calls")[0].1.as_ref().and_then(|o| o.request.api_key.clone()), None, "{title}");
}

#[tokio::test]
async fn supports_dynamic_providers_via_refresh_with_in_flight_dedupe() {
    let title = "supports dynamic providers via refresh with in-flight dedupe";
    let fetches = Arc::new(AtomicUsize::new(0));
    // Latched signals: `started` counts fetches, `gate` stays open once released, so the later
    // refresh-all pass (which refetches `dyn`) does not wait on a one-shot wake-up.
    let (started_tx, mut started_rx) = tokio::sync::watch::channel(0usize);
    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let (counter, started_tx) = (fetches.clone(), Arc::new(started_tx));
    let refresh: RefreshImagesModels = Arc::new(move || {
        let (counter, started_tx, mut gate_rx) = (counter.clone(), started_tx.clone(), gate_rx.clone());
        Box::pin(async move {
            let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
            started_tx.send_replace(count);
            gate_rx.wait_for(|open| *open).await.expect("gate sender alive");
            Ok(vec![test_image_model("dyn", "listed")])
        })
    });
    let models = create_images_models();
    models.set_provider(create_images_provider(CreateImagesProviderOptions {
        id: "dyn".into(),
        name: None,
        auth: env_auth(&[], None),
        models: Vec::new(),
        refresh_models: Some(refresh),
        api: Arc::new(Recorder::default()),
    }));
    assert!(models.get_models(Some("dyn")).is_empty(), "{title}");
    let both = futures::future::join(models.refresh(Some("dyn")), models.refresh(Some("dyn")));
    let releaser = async {
        started_rx.wait_for(|count| *count > 0).await.expect("started sender alive");
        gate_tx.send_replace(true);
    };
    let ((a, b), ()) = tokio::time::timeout(std::time::Duration::from_secs(10), futures::future::join(both, releaser))
        .await
        .expect("deduped refresh completes");
    assert_eq!((a, b), (Ok(()), Ok(())), "{title}");
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "{title}");
    assert!(models.get_model("dyn", "listed").is_some(), "{title}");

    let flaky: RefreshImagesModels = Arc::new(|| Box::pin(async { Err(ModelsError::new(ModelsErrorCode::Provider, "fetch failed")) }));
    models.set_provider(create_images_provider(CreateImagesProviderOptions {
        id: "flaky".into(),
        name: None,
        auth: env_auth(&[], None),
        models: Vec::new(),
        refresh_models: Some(flaky),
        api: Arc::new(Recorder::default()),
    }));
    let error = models.refresh(Some("flaky")).await.expect_err("refresh fails");
    assert_eq!(error.code, ModelsErrorCode::ModelSource, "{title}");
    assert_eq!(error.message, "Model refresh failed for flaky: fetch failed", "{title}");
    let all = tokio::time::timeout(std::time::Duration::from_secs(10), models.refresh(None)).await;
    assert_eq!(all.expect("refresh-all completes"), Ok(()), "{title}");
    assert_eq!(fetches.load(Ordering::SeqCst), 2, "{title}");
}

// senpi test/images-models.test.ts's 6th case, "builtinImagesModels registers the openrouter
// provider with its catalog", drives `builtinImagesModels()` from `providers/all.ts`, which
// aggregates every provider factory (including `openrouter-images.ts`) into one registry. Both
// `crates/maho-ai/src/providers/all.rs` and `.../providers/openrouter_images.rs` are still todo-13
// stubs (`// ported by todo 13`) in this crate, so there is no `builtin_images_models()` to call
// yet; porting this case now would mean writing todo 13's registry ahead of its lane. Excluded;
// owned by todo 13 (providers/).

