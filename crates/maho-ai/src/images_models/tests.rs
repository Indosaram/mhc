use super::*;
use crate::auth::types::{ApiKeyAuth, Credential, ModelAuth, OAuthCredential};
use crate::types::{ContentBlock, ImagesStopReason};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use std::collections::HashMap;
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

struct FakeAuthContext(HashMap<String, String>);

#[async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn fake_auth_context(env: &[(&str, &str)]) -> Option<CreateImagesModelsOptions> {
    let env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    Some(CreateImagesModelsOptions { auth_context: Some(Arc::new(FakeAuthContext(env))), ..Default::default() })
}

struct TestKey(Option<&'static str>);

#[async_trait]
impl ApiKeyAuth for TestKey {
    fn name(&self) -> &str {
        "Test key"
    }

    async fn resolve(&self, ctx: &dyn AuthContext, credential: Option<&ApiKeyCredential>, _signal: &AbortSignal) -> anyhow::Result<Option<AuthResult>> {
        let Some(var) = self.0 else { return Ok(Some(AuthResult::default())) };
        let key = match credential.and_then(|c| c.key.clone()) {
            Some(key) => Some(key),
            None => ctx.env(var).await,
        };
        Ok(key.map(|key| AuthResult { auth: ModelAuth { api_key: Some(key), ..Default::default() }, ..Default::default() }))
    }
}

fn env_auth(var: Option<&'static str>) -> ProviderAuth {
    ProviderAuth { api_key: Some(Arc::new(TestKey(var))), oauth: None }
}

struct FixedAuth;

#[async_trait]
impl ApiKeyAuth for FixedAuth {
    fn name(&self) -> &str {
        "Test key"
    }

    async fn resolve(&self, _ctx: &dyn AuthContext, _credential: Option<&ApiKeyCredential>, _signal: &AbortSignal) -> anyhow::Result<Option<AuthResult>> {
        let env: ProviderEnv = [("PROVIDER_ONLY", "provider"), ("SHARED", "provider")].iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        Ok(Some(AuthResult { auth: ModelAuth { api_key: Some("provider-key".into()), ..Default::default() }, env: Some(env), source: None }))
    }
}

fn provider(id: &str, models: Vec<ImagesModel>, auth: ProviderAuth, api: Arc<Recorder>) -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions { id: id.into(), name: None, auth, models, refresh_models: None, api })
}

fn context() -> ImagesContext {
    ImagesContext { input: vec![ContentBlock::text("a red circle")] }
}

#[test]
fn registers_providers_and_reads_models_synchronously() {
    let title = "registers providers and reads models synchronously";
    let models = create_images_models(None);
    let api = Arc::new(Recorder::default());
    models.set_provider(provider("p1", vec![test_image_model("p1", "m1"), test_image_model("p1", "m2")], env_auth(None), api.clone()));
    models.set_provider(provider("p2", vec![test_image_model("p2", "m3")], env_auth(None), api));
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
    let models = create_images_models(fake_auth_context(&[("TEST_KEY", "env-key")]));
    let api = Arc::new(Recorder::default());
    models.set_provider(provider("p1", vec![test_image_model("p1", "model-a")], env_auth(Some("TEST_KEY")), api.clone()));
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
    let models = create_images_models(None);
    let api = Arc::new(Recorder::default());
    let auth = ProviderAuth { api_key: Some(Arc::new(FixedAuth)), oauth: None };
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
    let models = create_images_models(fake_auth_context(&[]));
    let ghost = models.generate_images(&test_image_model("ghost", "m"), &context(), None).await;
    assert_eq!(ghost.stop_reason, ImagesStopReason::Error, "{title}");
    assert!(ghost.error_message.as_deref().is_some_and(|m| m.contains("Unknown provider: ghost")), "{title}");
    assert!(ghost.output.is_empty(), "{title}");

    let api = Arc::new(Recorder::default());
    models.set_provider(provider("p1", vec![test_image_model("p1", "model-a")], env_auth(Some("MISSING")), api.clone()));
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
    let models = create_images_models(None);
    models.set_provider(create_images_provider(CreateImagesProviderOptions {
        id: "dyn".into(),
        name: None,
        auth: env_auth(None),
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
        auth: env_auth(None),
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

#[tokio::test]
async fn builtin_images_models_registers_the_openrouter_provider_with_its_catalog() {
    let title = "builtinImagesModels registers the openrouter provider with its catalog";
    let models = crate::providers::all::builtin_images_models(fake_auth_context(&[("OPENROUTER_API_KEY", "or-key")]));
    assert_eq!(models.get_providers().iter().map(|p| p.id().to_owned()).collect::<Vec<_>>(), ["openrouter", "openai"], "{title}");
    let list = models.get_models(Some("openrouter"));
    assert!(!list.is_empty(), "{title}");
    assert!(list.iter().all(|m| m.api == "openrouter-images"), "{title}");
    let auth = models.get_auth(&list[0].provider, &AuthResolutionOverrides::default()).await.expect("auth");
    assert_eq!(auth.and_then(|r| r.auth.api_key), Some("or-key".into()), "{title}");
}

#[tokio::test]
async fn stored_openrouter_oauth_credential_resolves_for_images() {
    let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
    let expires = crate::utils::diagnostics::now_ms() as f64 + 3_600_000.0;
    credentials
        .modify(
            "openrouter",
            Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(OAuthCredential::new("oauth-access", "oauth-refresh", expires)))) })),
            None,
        )
        .await
        .expect("seed credential");
    let options = CreateImagesModelsOptions { credentials: Some(credentials), auth_context: fake_auth_context(&[("OPENROUTER_API_KEY", "or-key")]).and_then(|o| o.auth_context) };
    let models = crate::providers::all::builtin_images_models(Some(options));
    let auth = models.get_auth("openrouter", &AuthResolutionOverrides::default()).await.expect("auth");
    assert_eq!(auth.and_then(|r| r.auth.api_key), Some("oauth-access".into()));
}
