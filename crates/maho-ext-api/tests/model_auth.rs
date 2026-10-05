use maho_ext_api::{ExtensionFuture, Model, ModelRegistry, ResolvedRequestAuth};

fn ready<T>(mut future: ExtensionFuture<'_, T>) -> Result<T, maho_ext_api::ExtensionFailure> {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(result) => result,
        std::task::Poll::Pending => panic!("immediate fixture future unexpectedly pending"),
    }
}

struct LegacyRegistry;
impl ModelRegistry for LegacyRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(Some("provider-only-fixture".into())) }) }
}

fn model(id: &str) -> Model {
    serde_json::from_value(serde_json::json!({
        "id":id,"name":id,"api":"openai-responses","provider":"synthetic",
        "baseUrl":"https://fixture.invalid","reasoning":false,"input":["text"],
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},
        "contextWindow":1000,"maxTokens":100
    })).expect("model fixture")
}

#[test]
fn legacy_registry_auth_is_explicitly_unsupported_despite_provider_key() {
    let registry: &dyn ModelRegistry = &LegacyRegistry;
    assert!(ready(registry.get_api_key_for_provider("synthetic")).unwrap().is_some());
    assert!(ready(registry.get_api_key_and_headers(&model("first"))).is_err());
    assert!(ready(registry.get_provider_auth("synthetic")).is_err());
    assert!(registry.get_stored_credential_type("synthetic").is_err());
    assert!(registry.stream_simple(&model("first"), &maho_ai::types::Context::default(), None).is_err());
}

struct ScopedRegistry;
impl ModelRegistry for ScopedRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { true }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { panic!("model auth must not use provider-only lookup") }) }
    fn get_provider_auth<'a>(&'a self, provider: &'a str) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        Box::pin(async move {
            match provider {
                "missing" => Ok(None),
                "failed" => Err("provider auth rejected".into()),
                _ => Ok(Some(maho_ai::models::AuthResolution {
                    auth: maho_ai::models::ProviderAuthResult { api_key: Some("synthetic-key".into()), headers: Some([("remove".into(), None)].into()), base_url: None },
                    env: Some([("FIXTURE".into(), provider.into())].into()),
                })),
            }
        })
    }
    fn get_stored_credential_type(&self, provider: &str) -> Result<Option<maho_ai::auth::types::CredentialType>, maho_ext_api::ExtensionFailure> {
        Ok(match provider { "key" => Some(maho_ai::auth::types::CredentialType::ApiKey), "oauth" => Some(maho_ai::auth::types::CredentialType::OAuth), _ => None })
    }
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        Box::pin(async move {
            if model.id == "failed" { return Err("model auth rejected".into()); }
            Ok(ResolvedRequestAuth {
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some("synthetic-key".into()),
                    headers: Some([("x-model".into(),Some(model.id.clone())),("remove".into(),None)].into()),
                    base_url: Some(format!("https://{}.invalid",model.id)),
                },
                extra_body: Some([("route".into(), serde_json::Value::String(model.id.clone()))].into_iter().collect()),
                upstream_model_id: Some(format!("upstream-{}",model.id)),
                service_tier: Some(maho_ai::types::ServiceTierPreference::Flex),
                env: Some([("FIXTURE".into(),model.id.clone())].into()),
            })
        })
    }
}

#[test]
fn provider_auth_preserves_absence_failure_and_nonsecret_credential_kind() {
    let registry: &dyn ModelRegistry = &ScopedRegistry;
    let auth = ready(registry.get_provider_auth("key")).unwrap().unwrap();
    assert_eq!(auth.auth.headers.unwrap()["remove"], None);
    assert_eq!(auth.env.unwrap()["FIXTURE"], "key");
    assert!(ready(registry.get_provider_auth("missing")).unwrap().is_none());
    assert!(ready(registry.get_provider_auth("failed")).is_err());
    assert_eq!(registry.get_stored_credential_type("key").unwrap(), Some(maho_ai::auth::types::CredentialType::ApiKey));
    assert_eq!(registry.get_stored_credential_type("oauth").unwrap(), Some(maho_ai::auth::types::CredentialType::OAuth));
    assert_eq!(registry.get_stored_credential_type("missing").unwrap(), None);
}

#[test]
fn model_scoped_result_preserves_routing_headers_environment_and_failure() {
    let registry: &dyn ModelRegistry = &ScopedRegistry;
    for id in ["first","second"] {
        let result = ready(registry.get_api_key_and_headers(&model(id))).unwrap();
        assert_eq!(result.auth.api_key.as_deref(),Some("synthetic-key"));
        let headers = result.auth.headers.unwrap();
        assert_eq!(headers["x-model"].as_deref(),Some(id));
        assert_eq!(headers["remove"],None);
        assert_eq!(result.auth.base_url,Some(format!("https://{id}.invalid")));
        assert_eq!(result.extra_body.unwrap()["route"],id);
        assert_eq!(result.upstream_model_id,Some(format!("upstream-{id}")));
        assert_eq!(result.service_tier,Some(maho_ai::types::ServiceTierPreference::Flex));
        assert_eq!(result.env.unwrap()["FIXTURE"],id);
    }
    assert!(ready(registry.get_api_key_and_headers(&model("failed"))).is_err());
}
