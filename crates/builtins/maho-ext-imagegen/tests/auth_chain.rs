#![allow(clippy::unwrap_used)]

use maho_ai::model::Model;
use maho_ext_imagegen::auth::{resolve_image_gen_auth, AuthFuture, Credentials, ImageGenAuthRegistry, ImageGenAuthResolution};
use serde_json::json;
use std::collections::BTreeMap;

struct FakeRegistry {
    stored_api_key: bool,
    models: Vec<Model>,
    openai_provider_auth: Option<Credentials>,
    gateway_auth: BTreeMap<String, Option<Credentials>>,
}

impl FakeRegistry {
    fn new() -> Self {
        Self { stored_api_key: false, models: Vec::new(), openai_provider_auth: None, gateway_auth: BTreeMap::new() }
    }
    fn stored(mut self, auth: Option<Credentials>) -> Self {
        self.stored_api_key = true;
        self.openai_provider_auth = auth;
        self
    }
    fn models(mut self, models: Vec<Model>) -> Self {
        self.models = models;
        self
    }
    fn gateway(mut self, provider: &str, auth: Option<Credentials>) -> Self {
        self.gateway_auth.insert(provider.into(), auth);
        self
    }
}

impl ImageGenAuthRegistry for FakeRegistry {
    fn stored_openai_is_api_key(&self) -> bool {
        self.stored_api_key
    }
    fn get_all(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn get_provider_auth(&self, provider: &str) -> AuthFuture<'_> {
        let auth = (provider == "openai").then(|| self.openai_provider_auth.clone()).flatten();
        Box::pin(async move { Ok(auth) })
    }
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> AuthFuture<'a> {
        let auth = self.gateway_auth.get(&model.provider).cloned().flatten();
        Box::pin(async move { Ok(auth) })
    }
}

fn model(provider: &str) -> Model {
    model_with(provider, provider, "openai-completions")
}

fn model_with(provider: &str, id: &str, api: &str) -> Model {
    model_base(provider, id, api, &format!("https://{provider}.example/v1"))
}

fn model_base(provider: &str, id: &str, api: &str, base_url: &str) -> Model {
    serde_json::from_value(json!({"provider":provider,"id":id,"name":id,"api":api,"baseUrl":base_url,"reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":4096,"maxTokens":1024})).expect("model")
}

fn creds(api_key: Option<&str>, headers: &[(&str, Option<&str>)]) -> Credentials {
    Credentials {
        api_key: api_key.map(str::to_owned),
        headers: headers.iter().map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned))).collect(),
    }
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
}

async fn resolve(registry: &FakeRegistry, env: &BTreeMap<String, String>) -> ImageGenAuthResolution {
    resolve_image_gen_auth(registry, env).await
}

fn configured(result: &ImageGenAuthResolution) -> (&'static str, String, &'static str, Option<String>) {
    match result {
        ImageGenAuthResolution::Configured { kind, api_key, provenance, provider_id, .. } => (kind, api_key.clone(), provenance, provider_id.clone()),
        ImageGenAuthResolution::None { .. } => panic!("expected configured"),
    }
}

#[tokio::test]
async fn stored_native_auth_wins_over_a_pinned_gateway() {
    let registry = FakeRegistry::new().stored(Some(creds(Some("stored-secret"), &[]))).models(vec![model("pinned-openai")]).gateway("pinned-openai", Some(creds(Some("gateway-secret"), &[])));
    let result = resolve(&registry, &env(&[("PI_IMAGE_GEN_PROVIDER", "pinned-openai")])).await;
    let (kind, key, provenance, provider) = configured(&result);
    assert_eq!((kind, key.as_str(), provenance, provider.as_deref()), ("native-openai", "stored-secret", "store", Some("openai")));
}

#[tokio::test]
async fn materialized_headers_are_threaded_through() {
    let registry = FakeRegistry::new().models(vec![model("header-openai")]).gateway("header-openai", Some(creds(Some("header-key"), &[("Authorization", Some("Bearer header-key")), ("X-Route", Some("one"))])));
    let ImageGenAuthResolution::Configured { api_key, headers, .. } = resolve(&registry, &env(&[])).await else { panic!("gateway") };
    assert_eq!(api_key, "header-key");
    assert_eq!(headers.get("Authorization").map(String::as_str), Some("Bearer header-key"));
    assert_eq!(headers.get("X-Route").map(String::as_str), Some("one"));
}

#[tokio::test]
async fn plain_keys_do_not_synthesize_authorization() {
    let registry = FakeRegistry::new().models(vec![model("plain-openai")]).gateway("plain-openai", Some(creds(Some("plain-key"), &[("X-Route", Some("two"))])));
    let ImageGenAuthResolution::Configured { api_key, headers, .. } = resolve(&registry, &env(&[])).await else { panic!("gateway") };
    assert_eq!(api_key, "plain-key");
    assert_eq!(headers.get("X-Route").map(String::as_str), Some("two"));
    assert!(headers.get("Authorization").is_none());
}

#[tokio::test]
async fn headers_only_gateway_without_a_key_is_rejected() {
    let registry = FakeRegistry::new().models(vec![model("header-only-openai")]).gateway("header-only-openai", Some(creds(None, &[("X-API-Key", Some("header-secret"))])));
    assert!(matches!(resolve(&registry, &env(&[])).await, ImageGenAuthResolution::None { .. }));
}

#[tokio::test]
async fn a_headers_only_provider_falls_through_to_a_later_keyed_gateway() {
    let registry = FakeRegistry::new().models(vec![model("headers-first"), model("keyed-second")]).gateway("headers-first", Some(creds(None, &[("X-API-Key", Some("header-secret"))]))).gateway("keyed-second", Some(creds(Some("second-key"), &[])));
    let (kind, key, _, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((kind, key.as_str(), provider.as_deref()), ("gateway", "second-key", Some("keyed-second")));
}

#[tokio::test]
async fn empty_inline_keys_are_rejected() {
    let registry = FakeRegistry::new().models(vec![model("empty-openai")]).gateway("empty-openai", Some(creds(Some("  "), &[])));
    assert!(matches!(resolve(&registry, &env(&[])).await, ImageGenAuthResolution::None { .. }));
}

#[tokio::test]
async fn an_unresolved_gateway_key_falls_through() {
    for source in ["environment", "command"] {
        let provider = format!("{source}-openai");
        let registry = FakeRegistry::new().models(vec![model(&provider)]).gateway(&provider, None);
        assert!(matches!(resolve(&registry, &env(&[])).await, ImageGenAuthResolution::None { .. }), "{source}");
    }
}

#[tokio::test]
async fn a_listed_image_model_is_not_required() {
    let registry = FakeRegistry::new().models(vec![model_with("chat-only-openai", "text-chat-model", "openai-completions")]).gateway("chat-only-openai", Some(creds(Some("chat-route-key"), &[])));
    let (kind, _, _, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((kind, provider.as_deref()), ("gateway", Some("chat-only-openai")));
}

#[tokio::test]
async fn a_stored_api_key_credential_is_used() {
    let registry = FakeRegistry::new().stored(Some(creds(Some("stored-openai-key"), &[])));
    let (kind, key, provenance, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((kind, key.as_str(), provenance, provider.as_deref()), ("native-openai", "stored-openai-key", "store", Some("openai")));
}

#[tokio::test]
async fn stored_oauth_entries_are_skipped_by_type() {
    let registry = FakeRegistry::new().models(vec![model("fallback-openai")]).gateway("fallback-openai", Some(creds(Some("fallback-key"), &[])));
    let (kind, key, _, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((kind, key.as_str(), provider.as_deref()), ("gateway", "fallback-key", Some("fallback-openai")));
}

#[tokio::test]
async fn an_empty_stored_key_falls_through_to_the_environment() {
    let registry = FakeRegistry::new().stored(Some(creds(Some("\t"), &[])));
    let (kind, key, provenance, _) = configured(&resolve(&registry, &env(&[("OPENAI_API_KEY", "env-fallback")])).await);
    assert_eq!((kind, key.as_str(), provenance), ("native-openai", "env-fallback", "env"));
}

#[tokio::test]
async fn openai_matching_provider_ids_win_with_alphabetical_tiebreaking() {
    let registry = FakeRegistry::new().models(vec![model("aaa-gateway"), model("beta-openai"), model("alpha-openai")]).gateway("aaa-gateway", Some(creds(Some("aaa-key"), &[]))).gateway("beta-openai", Some(creds(Some("beta-key"), &[]))).gateway("alpha-openai", Some(creds(Some("alpha-key"), &[])));
    let (_, key, _, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((key.as_str(), provider.as_deref()), ("alpha-key", Some("alpha-openai")));
}

#[tokio::test]
async fn a_configured_gateway_precedes_the_environment_key() {
    let registry = FakeRegistry::new().models(vec![model("gateway-openai")]).gateway("gateway-openai", Some(creds(Some("gateway-key"), &[])));
    let (kind, key, _, _) = configured(&resolve(&registry, &env(&[("OPENAI_API_KEY", "env-openai-key")])).await);
    assert_eq!((kind, key.as_str()), ("gateway", "gateway-key"));
}

#[tokio::test]
async fn setup_guidance_is_key_free_and_names_all_three_routes() {
    let registry = FakeRegistry::new().models(vec![model("broken-openai")]).gateway("broken-openai", None);
    let ImageGenAuthResolution::None { reason } = resolve(&registry, &env(&[("PI_IMAGE_GEN_PROVIDER", "broken-openai")])).await else { panic!("none") };
    assert!(reason.contains("PI_IMAGE_GEN_PROVIDER"));
    assert!(reason.contains("OPENAI_API_KEY"));
    assert!(!reason.contains("stored-secret-sentinel"));
    assert!(!reason.contains("gateway-secret-sentinel"));
}

#[tokio::test]
async fn a_placeholder_store_key_and_a_configured_gateway_select_the_gateway() {
    let registry = FakeRegistry::new().stored(Some(creds(Some("SK-SENTINEL-DO-NOT-LOG-2"), &[]))).models(vec![model("openai-quotio")]).gateway("openai-quotio", Some(creds(Some("quotio-local-real"), &[])));
    let (kind, key, _, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((kind, key.as_str(), provider.as_deref()), ("gateway", "quotio-local-real", Some("openai-quotio")));
}

#[tokio::test]
async fn only_a_placeholder_store_key_reports_not_configured() {
    let registry = FakeRegistry::new().stored(Some(creds(Some("SK-SENTINEL-DO-NOT-LOG-2"), &[])));
    assert!(matches!(resolve(&registry, &env(&[])).await, ImageGenAuthResolution::None { .. }));
}

#[tokio::test]
async fn an_explicit_provider_pin_precedes_gateway_scanning() {
    let registry = FakeRegistry::new().models(vec![model("alpha-openai"), model("pinned-gateway")]).gateway("alpha-openai", Some(creds(Some("alpha-key"), &[]))).gateway("pinned-gateway", Some(creds(Some("pinned-key"), &[])));
    let (_, key, _, provider) = configured(&resolve(&registry, &env(&[("PI_IMAGE_GEN_PROVIDER", "pinned-gateway")])).await);
    assert_eq!((key.as_str(), provider.as_deref()), ("pinned-key", Some("pinned-gateway")));
}

#[tokio::test]
async fn a_valid_gateway_is_used_when_native_auth_is_absent() {
    let registry = FakeRegistry::new().models(vec![model("quotio-openai")]).gateway("quotio-openai", Some(creds(Some("gateway-key"), &[])));
    let ImageGenAuthResolution::Configured { kind, api_key, base_url, provenance, provider_id, .. } = resolve(&registry, &env(&[])).await else { panic!("gateway") };
    assert_eq!((kind, api_key.as_str(), base_url.as_str(), provenance, provider_id.as_deref()), ("gateway", "gateway-key", "https://quotio-openai.example/v1", "provider-config", Some("quotio-openai")));
}

#[tokio::test]
async fn a_source_missing_a_base_url_is_skipped_without_mixing_keys() {
    let registry = FakeRegistry::new().models(vec![model_base("broken", "broken", "openai-completions", ""), model("fallback")]).gateway("broken", Some(creds(Some("broken-key"), &[]))).gateway("fallback", Some(creds(Some("fallback-key"), &[])));
    let (_, key, _, provider) = configured(&resolve(&registry, &env(&[("PI_IMAGE_GEN_PROVIDER", "broken")])).await);
    assert_eq!((key.as_str(), provider.as_deref()), ("fallback-key", Some("fallback")));
}

#[tokio::test]
async fn providers_with_a_wrong_chat_api_are_skipped() {
    let registry = FakeRegistry::new().models(vec![model_with("wrong", "wrong", "anthropic-messages"), model_with("valid", "valid", "openai-responses")]).gateway("wrong", Some(creds(Some("wrong-key"), &[]))).gateway("valid", Some(creds(Some("valid-key"), &[])));
    let (_, key, _, provider) = configured(&resolve(&registry, &env(&[])).await);
    assert_eq!((key.as_str(), provider.as_deref()), ("valid-key", Some("valid")));
}

#[tokio::test]
async fn the_environment_key_is_used_when_it_is_the_only_source() {
    let registry = FakeRegistry::new();
    let ImageGenAuthResolution::Configured { kind, api_key, base_url, provenance, provider_id, .. } = resolve(&registry, &env(&[("OPENAI_API_KEY", "env-openai-key")])).await else { panic!("env") };
    assert_eq!((kind, api_key.as_str(), base_url.as_str(), provenance, provider_id), ("native-openai", "env-openai-key", "https://api.openai.com/v1", "env", None));
}
