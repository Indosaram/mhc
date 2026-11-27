//! Provider-routing cases from senpi packages/ai/test/chatgpt-subscription-placement.test.ts.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use maho_ai::api::openai_codex_responses;
use maho_ai::auth::context::default_provider_auth_context;
use maho_ai::auth::credential_store::InMemoryCredentialStore;
use maho_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use maho_ai::models::{
    CreateModelsOptions, Models, ModelsRequestTransforms, ProviderModelsAuth, create_models,
};
use maho_ai::providers::chatgpt_subscription::chatgpt_subscription_provider;
use maho_ai::types::{
    Context, Model, ProviderRequestOptions, SimpleStreamOptions, StopReason, StreamOptions,
    Transport,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

#[path = "support/siwc_wire.rs"]
mod siwc_wire;
use siwc_wire::wire_client;

#[path = "openai_responses_siwc_tests.rs"]
mod siwc_contract;

async fn models(siwc: bool, id: &str) -> (Models, Model, String) {
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "synthetic-account"}
        })
        .to_string(),
    );
    let access = format!("synthetic.{payload}.signature");
    let mut credential =
        OAuthCredential::new(access.clone(), "synthetic-refresh", 4_102_444_800_000.0);
    if siwc {
        credential = credential
            .with_extra("route", json!("siwc"))
            .with_extra("subject", json!("synthetic-subject"))
            .with_extra("clientId", json!("oaiapp_synthetic"))
            .with_extra("scopes", json!(["openid", "chatgpt.tokens.use.direct"]))
            .with_extra("planUsageEnabled", json!(true));
    }
    let provider = chatgpt_subscription_provider().expect("ChatGPT subscription");
    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify(
            provider.id(),
            Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
            None,
        )
        .await
        .expect("seed synthetic account");
    let models = create_models(Some(CreateModelsOptions {
        auth: Some(Arc::new(ProviderModelsAuth::new(
            store,
            Arc::new(default_provider_auth_context()),
        ))),
        ..Default::default()
    }));
    let model = provider.get_model(id).expect("tagged model fixture");
    models.set_provider(provider);
    (models, model, access)
}

fn context() -> Context {
    serde_json::from_value(json!({
        "systemPrompt": "fixture instructions",
        "messages": [{"role":"user", "content":"fixture message", "timestamp":1}]
    }))
    .expect("context")
}

fn options(client: reqwest::Client) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions {
            fetch: Some(client),
            max_retries: Some(0),
            ..Default::default()
        },
        transport: Some(Transport::Sse),
        session_id: Some("fixture-session".into()),
        ..Default::default()
    }
}

async fn assert_siwc_placement(simple: bool) {
    let (models, model, access) = models(true, "gpt-6.1-sol-fast").await;
    let (client, wire) = wire_client("api.openai.com", 1).await;
    let context = context();
    let mut options = options(client);
    options.temperature = Some(0.5);
    options.max_tokens = Some(128);
    let transform = ModelsRequestTransforms::default();
    let stream = if simple {
        models.stream_simple(
            &model,
            &context,
            Some(SimpleStreamOptions {
                stream: options,
                ..Default::default()
            }),
            transform,
        )
    } else {
        models.stream(&model, &context, Some(options), transform)
    };
    let result = stream.result().await.expect("terminal message");
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    let requests = tokio::time::timeout(Duration::from_secs(5), wire)
        .await
        .expect("wire deadline")
        .expect("fixture requests");
    let request = &requests[0];
    assert_eq!(request.line, "POST /v1/responses HTTP/1.1");
    assert_eq!(request.headers["authorization"], format!("Bearer {access}"));
    assert!(!request.headers.contains_key("chatgpt-account-id"));
    let body: Value = serde_json::from_slice(&request.body).expect("request JSON");
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_output_tokens").is_none());
    assert!(body["input"].as_array().expect("input").contains(&json!({
        "role":"developer", "content":"fixture instructions"
    })));
    assert_eq!(model.api, "openai-codex-responses");
    assert_eq!(model.base_url, "https://chatgpt.com/backend-api");
}

#[tokio::test]
async fn sends_siwc_through_public_responses_in_models_stream() {
    assert_siwc_placement(false).await;
}

#[tokio::test]
async fn sends_siwc_through_public_responses_in_models_stream_simple() {
    assert_siwc_placement(true).await;
}

#[tokio::test]
async fn keeps_legacy_request_url_headers_and_serialized_body_identical_to_the_codex_adapter() {
    let (models, model, access) = models(false, "gpt-6.1-sol").await;
    let (client, wire) = wire_client("chatgpt.com", 2).await;
    let context = context();
    let options = options(client);
    let mut direct = options.clone();
    direct.request.api_key = Some(access);
    let before = openai_codex_responses::stream(&model, &context, Some(direct))
        .result()
        .await
        .expect("direct terminal");
    let after = models
        .stream(
            &model,
            &context,
            Some(options),
            ModelsRequestTransforms::default(),
        )
        .result()
        .await
        .expect("wrapped terminal");
    assert_eq!(
        before.stop_reason,
        StopReason::Stop,
        "{:?}",
        before.error_message
    );
    assert_eq!(
        after.stop_reason,
        StopReason::Stop,
        "{:?}",
        after.error_message
    );
    let requests = tokio::time::timeout(Duration::from_secs(5), wire)
        .await
        .expect("wire deadline")
        .expect("fixture requests");
    assert_eq!(
        requests[0].line,
        "POST /backend-api/codex/responses HTTP/1.1"
    );
    assert_eq!(requests[0], requests[1]);
}
