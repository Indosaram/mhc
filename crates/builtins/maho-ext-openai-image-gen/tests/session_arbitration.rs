#[path = "../../maho-ext-imagegen/tests/support.rs"]
mod imagegen_support;

use maho_ai::node::provider_scope::{run_with_provider_scope_async, ProviderScope};
use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_ext_imagegen::auth::ImageGenAuthRegistry;
use maho_test_support::faux::{FauxResponse, FauxScript};
use maho_test_support::faux_session::{FauxSession, NativeSession};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Creds {
    None,
    Gateway,
    Native,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behavior {
    Bypass,
    MissingConfig,
    Live,
}

#[derive(Clone, Copy)]
enum ModelKind {
    Official,
    OfficialCompatOff,
    Proxied,
    ProxiedCompatOn,
    Azure,
    Completions,
}

struct Row {
    creds: Creds,
    model: ModelKind,
    injection: usize,
    behavior: Behavior,
    skill: bool,
    client_section: bool,
    native_section: bool,
}

fn table() -> Vec<Row> {
    use Behavior::{Bypass, Live, MissingConfig};
    vec![
        Row { creds: Creds::None, model: ModelKind::Official, injection: 1, behavior: Bypass, skill: false, client_section: false, native_section: true },
        Row { creds: Creds::None, model: ModelKind::Proxied, injection: 0, behavior: MissingConfig, skill: false, client_section: false, native_section: false },
        Row { creds: Creds::None, model: ModelKind::Completions, injection: 0, behavior: MissingConfig, skill: false, client_section: false, native_section: false },
        Row { creds: Creds::None, model: ModelKind::Azure, injection: 0, behavior: MissingConfig, skill: false, client_section: false, native_section: false },
        Row { creds: Creds::None, model: ModelKind::ProxiedCompatOn, injection: 1, behavior: Bypass, skill: false, client_section: false, native_section: true },
        Row { creds: Creds::Gateway, model: ModelKind::Official, injection: 1, behavior: Bypass, skill: true, client_section: true, native_section: true },
        Row { creds: Creds::Gateway, model: ModelKind::Proxied, injection: 0, behavior: Live, skill: true, client_section: true, native_section: false },
        Row { creds: Creds::Gateway, model: ModelKind::Completions, injection: 0, behavior: Live, skill: true, client_section: true, native_section: false },
        Row { creds: Creds::Gateway, model: ModelKind::Azure, injection: 0, behavior: Live, skill: true, client_section: true, native_section: false },
        Row { creds: Creds::Gateway, model: ModelKind::OfficialCompatOff, injection: 0, behavior: Live, skill: true, client_section: true, native_section: false },
        Row { creds: Creds::Gateway, model: ModelKind::ProxiedCompatOn, injection: 1, behavior: Bypass, skill: true, client_section: true, native_section: true },
        Row { creds: Creds::Native, model: ModelKind::Official, injection: 1, behavior: Bypass, skill: true, client_section: true, native_section: true },
        Row { creds: Creds::Native, model: ModelKind::Proxied, injection: 0, behavior: Live, skill: true, client_section: true, native_section: false },
    ]
}

fn model(kind: ModelKind) -> maho_ai::types::Model {
    let (api, provider, base_url, compat) = match kind {
        ModelKind::Official => ("openai-responses", "openai", "https://api.openai.com/v1", None),
        ModelKind::OfficialCompatOff => ("openai-responses", "openai", "https://api.openai.com/v1", Some(false)),
        ModelKind::Proxied => ("openai-responses", "quotio-openai", "https://gateway.example/openai/v1", None),
        ModelKind::ProxiedCompatOn => ("openai-responses", "quotio-openai", "https://gateway.example/openai/v1", Some(true)),
        ModelKind::Azure => ("azure-openai-responses", "azure", "https://contoso.openai.azure.com/openai/v1", None),
        ModelKind::Completions => ("openai-completions", "openai", "https://api.openai.com/v1", None),
    };
    let mut value = json!({"id":"gpt-5.5","name":"GPT-5.5","api":api,"provider":provider,"baseUrl":base_url,"reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":128000,"maxTokens":16384});
    if let Some(compat) = compat {
        value["compat"] = json!({"supportsImageGeneration": compat});
    }
    serde_json::from_value(value).expect("model")
}

fn registry(creds: Creds) -> Arc<dyn ImageGenAuthRegistry> {
    match creds {
        Creds::None => Arc::new(imagegen_support::FixtureRegistry { stored_api_key: false, provider_api_key: None, provider_headers: None, models: Vec::new() }),
        Creds::Gateway => Arc::new(imagegen_support::FixtureRegistry { stored_api_key: false, provider_api_key: Some("gateway-secret".into()), provider_headers: None, models: vec![imagegen_support::gateway_model()] }),
        Creds::Native => Arc::new(imagegen_support::FixtureRegistry { stored_api_key: true, provider_api_key: Some("sk-native-test-key".into()), provider_headers: None, models: Vec::new() }),
    }
}

async fn boot() -> NativeSession {
    FauxSession::new(FauxScript { name: "arbitration".into(), prompt: "draw".into(), responses: vec![FauxResponse { content: "ok".into(), stop_reason: "stop".into() }] })
        .with_native_extension(NativeExtensionFactory { path: "imagegen".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_imagegen::ImageGen::default()) })
        .with_native_extension(NativeExtensionFactory { path: "openai-image-gen".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_openai_image_gen::OpenAiImageGen) })
        .run_native_handle()
        .await
        .expect("native session")
}

fn request_payload() -> Value {
    json!({"model":"gpt-5.5","tools":[{"type":"function","name":"generate_image","parameters":{"type":"object"}},{"type":"function","name":"read","parameters":{"type":"object"}}]})
}

fn native_tools(payload: &Value) -> usize {
    payload["tools"].as_array().map(|tools| tools.iter().filter(|tool| tool["type"] == "image_generation").count()).unwrap_or(0)
}

fn has_function(payload: &Value, name: &str) -> bool {
    payload["tools"].as_array().map(|tools| tools.iter().any(|tool| tool["name"] == name)).unwrap_or(false)
}

async fn execute(session: &NativeSession, stub: Arc<imagegen_support::StubImages>) -> Value {
    let scope = ProviderScope::new();
    let outcome = run_with_provider_scope_async(&scope, async {
        maho_ai::images_api_registry::register_images_api_provider("openai-images", stub.clone(), Some("arb-stub"))?;
        let result = session.execute_tool("generate_image", json!({"prompt":"a fox"})).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result)
    })
    .await;
    scope.close();
    outcome.expect("scope").expect("execute").details
}

#[tokio::test]
async fn truth_table_rows_hold_across_the_session() {
    for (index, row) in table().into_iter().enumerate() {
        let _guard = imagegen_support::GlobalStateGuard::acquire(Some(registry(row.creds))).await;
        let session = boot().await;
        let model = model(row.model);
        let payload = session.emit_before_provider_request(request_payload(), Some(model.clone())).await.expect("payload");
        assert_eq!(native_tools(&payload), row.injection, "row {index}: injection");
        if row.injection > 0 {
            assert!(!has_function(&payload, "generate_image"), "row {index}: function tool stripped in native mode");
        }
        let stub = Arc::new(imagegen_support::StubImages::one());
        let details = execute(&session, stub.clone()).await;
        match row.behavior {
            Behavior::Bypass => assert_eq!(details["reason"], "provider_native_bypass", "row {index}"),
            Behavior::MissingConfig => {
                assert_eq!(details["reason"], "missing_config", "row {index}");
                assert_eq!(stub.call_count(), 0, "row {index}: no provider call");
            }
            Behavior::Live => {
                assert!(details["reason"].is_null(), "row {index}: live tool has no reason");
                assert!(stub.call_count() > 0, "row {index}: live tool calls the provider");
            }
        }
        let skills = session.discover_skills().await.expect("skills");
        assert_eq!(!skills.is_empty(), row.skill, "row {index}: skill presence");
        let prompt = session.emit_before_agent_start("draw a fox", "base").await.expect("prompt");
        let system = prompt.unwrap_or_else(|| "base".into());
        assert_eq!(system.contains(maho_ext_imagegen::IMAGE_GEN_SECTION.trim()), row.client_section, "row {index}: client section");
        assert_eq!(system.contains(maho_ext_openai_image_gen::OPENAI_IMAGE_GEN_SECTION.trim()), row.native_section, "row {index}: native section");
    }
}

#[test]
fn native_injection_implies_bypass_and_native_section() {
    for row in table() {
        let native = row.injection > 0;
        assert_eq!(row.behavior == Behavior::Bypass, native, "bypass iff native injection");
        assert_eq!(row.native_section, native, "native section iff native injection");
        assert_eq!(row.client_section, row.creds != Creds::None, "client section iff creds");
    }
}

#[test]
fn skill_presence_tracks_credentials() {
    for row in table() {
        assert_eq!(row.skill, row.creds != Creds::None, "skill iff creds");
    }
}

#[test]
fn no_credentials_without_native_is_missing_config() {
    for row in table() {
        if row.creds == Creds::None && row.injection == 0 {
            assert_eq!(row.behavior, Behavior::MissingConfig);
        }
    }
}

#[test]
fn credentials_without_native_are_live() {
    for row in table() {
        if row.creds != Creds::None && row.injection == 0 {
            assert_eq!(row.behavior, Behavior::Live);
        }
    }
}

#[test]
fn gate_discriminates_official_from_proxied() {
    use maho_ext_openai_image_gen::gate::{supports_native_image_generation, NativeImageGenModel};
    let model = |provider: &'static str, api: &'static str, base_url: &'static str, compat: Option<&'static Value>| NativeImageGenModel { id: "gpt-5.5", provider, api, base_url, compat };
    let compat_on = json!({"supportsImageGeneration": true});
    let compat_off = json!({"supportsImageGeneration": false});
    assert!(supports_native_image_generation(Some(&model("openai", "openai-responses", "https://api.openai.com/v1", None))));
    assert!(!supports_native_image_generation(Some(&model("quotio-openai", "openai-responses", "https://gateway.example/openai/v1", None))));
    assert!(!supports_native_image_generation(Some(&model("azure", "azure-openai-responses", "https://contoso.openai.azure.com/openai/v1", None))));
    assert!(!supports_native_image_generation(Some(&model("openai", "openai-completions", "https://api.openai.com/v1", None))));
    assert!(!supports_native_image_generation(Some(&model("openai", "openai-responses", "https://api.openai.com/v1", Some(&compat_off)))));
    assert!(supports_native_image_generation(Some(&model("quotio-openai", "openai-responses", "https://gateway.example/openai/v1", Some(&compat_on)))));
}
