#[path = "support.rs"]
mod support;

use maho_ai::node::provider_scope::{run_with_provider_scope_async, ProviderScope};
use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_ext_imagegen::state::set_image_gen_registry_override;
use maho_test_support::faux::{FauxResponse, FauxScript};
use maho_test_support::faux_session::{FauxSession, NativeSession};
use serde_json::{json, Value};
use std::sync::Arc;

fn script() -> FauxScript {
    FauxScript { name: "imagegen-extension".into(), prompt: "draw".into(), responses: vec![FauxResponse { content: "ok".into(), stop_reason: "stop".into() }] }
}

fn model(api: &str, provider: &str, base_url: &str) -> maho_ai::types::Model {
    serde_json::from_value(json!({"id":"gpt-5.5","name":"GPT-5.5","api":api,"provider":provider,"baseUrl":base_url,"reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":128000,"maxTokens":16384})).expect("model")
}

fn compat_model(api: &str, provider: &str, base_url: &str, supports: bool) -> maho_ai::types::Model {
    let mut value = json!({"id":"gpt-5.5","name":"GPT-5.5","api":api,"provider":provider,"baseUrl":base_url,"reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":128000,"maxTokens":16384});
    value["compat"] = json!({"supportsImageGeneration": supports});
    serde_json::from_value(value).expect("model")
}

fn official() -> maho_ai::types::Model {
    model("openai-responses", "openai", "https://api.openai.com/v1")
}
fn proxied() -> maho_ai::types::Model {
    model("openai-responses", "quotio-openai", "https://gateway.example/openai/v1")
}
fn completions() -> maho_ai::types::Model {
    model("openai-completions", "openai", "https://api.openai.com/v1")
}

fn credentialed() -> Arc<dyn maho_ext_imagegen::auth::ImageGenAuthRegistry> {
    Arc::new(support::FixtureRegistry { stored_api_key: false, provider_api_key: Some("gateway-secret".into()), provider_headers: None, models: vec![support::gateway_model()] })
}

fn uncredentialed() -> Arc<dyn maho_ext_imagegen::auth::ImageGenAuthRegistry> {
    Arc::new(support::FixtureRegistry { stored_api_key: false, provider_api_key: None, provider_headers: None, models: Vec::new() })
}

async fn boot() -> NativeSession {
    FauxSession::new(script())
        .with_native_extension(NativeExtensionFactory { path: "imagegen".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_imagegen::ImageGen::default()) })
        .with_native_extension(NativeExtensionFactory { path: "openai-image-gen".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_openai_image_gen::OpenAiImageGen) })
        .run_native_handle()
        .await
        .expect("native session")
}

fn request_payload() -> Value {
    json!({"model":"gpt-5.5","tools":[{"type":"function","name":"generate_image","parameters":{"type":"object"}},{"type":"function","name":"read","parameters":{"type":"object"}}]})
}

fn tool_names(payload: &Value) -> Vec<String> {
    payload["tools"].as_array().map(|tools| tools.iter().filter_map(|tool| tool["name"].as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn native_tools(payload: &Value) -> usize {
    payload["tools"].as_array().map(|tools| tools.iter().filter(|tool| tool["type"] == "image_generation").count()).unwrap_or(0)
}

async fn payload(session: &NativeSession, model: maho_ai::types::Model) -> Value {
    session.emit_before_provider_request(request_payload(), Some(model)).await.expect("payload")
}

async fn execute(session: &NativeSession, stub: Arc<support::StubImages>) -> Value {
    let scope = ProviderScope::new();
    let outcome = run_with_provider_scope_async(&scope, async {
        maho_ai::images_api_registry::register_images_api_provider("openai-images", stub.clone(), Some("session-stub"))?;
        let result = session.execute_tool("generate_image", json!({"prompt":"a fox"})).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result)
    })
    .await;
    scope.close();
    outcome.expect("scope").expect("execute").details
}

#[tokio::test]
async fn proxied_responses_with_credentials_exposes_only_the_client_function_tool() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = payload(&session, proxied()).await;
    assert!(tool_names(&payload).contains(&"generate_image".to_owned()));
    assert_eq!(native_tools(&payload), 0);
}

#[tokio::test]
async fn official_responses_replaces_the_function_tool_with_one_server_tool_and_bypasses() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = payload(&session, official()).await;
    assert_eq!(native_tools(&payload), 1);
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
    assert!(tool_names(&payload).contains(&"read".to_owned()));
    let details = execute(&session, Arc::new(support::StubImages::one())).await;
    assert_eq!(details["reason"], "provider_native_bypass");
}

#[tokio::test]
async fn proxied_responses_without_credentials_exposes_neither_tool() {
    let _guard = support::GlobalStateGuard::acquire(Some(uncredentialed())).await;
    let session = boot().await;
    let payload = payload(&session, proxied()).await;
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
    assert_eq!(native_tools(&payload), 0);
}

#[tokio::test]
async fn switching_to_the_official_endpoint_hands_over_to_the_server_tool() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = payload(&session, proxied()).await;
    assert_eq!(native_tools(&payload), 0);
    session.set_model(official()).await.expect("model");
    let payload = payload(&session, official()).await;
    assert_eq!(native_tools(&payload), 1);
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[tokio::test]
async fn switching_to_a_proxied_endpoint_hands_back_to_the_client_tool() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = payload(&session, official()).await;
    assert_eq!(native_tools(&payload), 1);
    session.set_model(proxied()).await.expect("model");
    let payload = payload(&session, proxied()).await;
    assert_eq!(native_tools(&payload), 0);
    assert!(tool_names(&payload).contains(&"generate_image".to_owned()));
    let details = execute(&session, Arc::new(support::StubImages::one())).await;
    assert_ne!(details["reason"], "provider_native_bypass");
}

#[tokio::test]
async fn a_native_session_without_credentials_loses_both_tools_on_a_proxied_endpoint() {
    let _guard = support::GlobalStateGuard::acquire(Some(uncredentialed())).await;
    let session = boot().await;
    let payload = payload(&session, official()).await;
    assert_eq!(native_tools(&payload), 1);
    session.set_model(proxied()).await.expect("model");
    let payload = payload(&session, proxied()).await;
    assert_eq!(native_tools(&payload), 0);
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[tokio::test]
async fn an_unavailable_session_gains_the_server_tool_on_the_official_endpoint() {
    let _guard = support::GlobalStateGuard::acquire(Some(uncredentialed())).await;
    let session = boot().await;
    let payload = payload(&session, proxied()).await;
    assert_eq!(native_tools(&payload), 0);
    session.set_model(official()).await.expect("model");
    let payload = payload(&session, official()).await;
    assert_eq!(native_tools(&payload), 1);
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[test]
fn proxied_and_azure_responses_default_to_the_client_tool() {
    use maho_ext_openai_image_gen::gate::{supports_native_image_generation, NativeImageGenModel};
    let proxied = NativeImageGenModel { id: "gpt-5.5", provider: "quotio-openai", api: "openai-responses", base_url: "https://gateway.example/openai/v1", compat: None };
    let azure = NativeImageGenModel { id: "gpt-5.5", provider: "azure", api: "azure-openai-responses", base_url: "https://contoso.openai.azure.com/openai/v1", compat: None };
    assert!(!supports_native_image_generation(Some(&proxied)));
    assert!(!supports_native_image_generation(Some(&azure)));
}

#[tokio::test]
async fn a_proxied_endpoint_with_compat_opt_in_injects_the_server_tool() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = payload(&session, compat_model("openai-responses", "quotio-openai", "https://gateway.example/openai/v1", true)).await;
    assert_eq!(native_tools(&payload), 1);
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[tokio::test]
async fn the_official_endpoint_with_compat_disabled_keeps_the_client_tool() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = payload(&session, compat_model("openai-responses", "openai", "https://api.openai.com/v1", false)).await;
    assert_eq!(native_tools(&payload), 0);
    assert!(tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[tokio::test]
async fn a_non_responses_api_strips_a_preexisting_native_tool_defensively() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let payload = session
        .emit_before_provider_request(json!({"model":"gpt-5.5","tools":[{"type":"image_generation"},{"type":"function","name":"generate_image","parameters":{"type":"object"}}]}), Some(completions()))
        .await
        .expect("payload");
    assert_eq!(native_tools(&payload), 0);
    assert!(tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[tokio::test]
async fn credentials_resolving_after_start_are_used_at_execution() {
    let _guard = support::GlobalStateGuard::acquire(Some(uncredentialed())).await;
    let session = boot().await;
    let blocked = execute(&session, Arc::new(support::StubImages::one())).await;
    assert_eq!(blocked["reason"], "missing_config");

    let stub = Arc::new(support::StubImages::one());
    set_image_gen_registry_override(Some(credentialed()));
    let details = execute(&session, stub.clone()).await;
    assert!(details["reason"].is_null());
    assert_eq!(details["paths"].as_array().map(Vec::len), Some(1));
    assert_eq!(stub.call_count(), 1);
}

#[tokio::test]
async fn a_cached_client_model_refreshes_when_the_request_model_differs() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let _ = payload(&session, proxied()).await;
    let payload = session.emit_before_provider_request(request_payload(), Some(official())).await.expect("payload");
    assert_eq!(native_tools(&payload), 1);
    assert!(!tool_names(&payload).contains(&"generate_image".to_owned()));
}

#[test]
fn the_enable_env_off_keeps_the_client_tool_active() {
    use maho_ext_openai_image_gen::gate::is_enabled;
    assert!(!is_enabled(Some("0")));
    assert!(!is_enabled(Some("off")));
    assert!(is_enabled(None));
}

#[tokio::test]
async fn an_unchanged_client_payload_is_returned_unchanged() {
    let _guard = support::GlobalStateGuard::acquire(Some(credentialed())).await;
    let session = boot().await;
    let input = json!({"model":"gpt-5.5","tools":[{"type":"function","name":"read","parameters":{}}]});
    let result = session.emit_before_provider_request(input.clone(), Some(proxied())).await.expect("payload");
    assert_eq!(result, input);
}
