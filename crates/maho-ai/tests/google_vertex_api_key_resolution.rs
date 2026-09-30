//! Port of senpi packages/ai/test/google-vertex-api-key-resolution.test.ts.
//!
//! The TS suite asserts the `GoogleGenAI` constructor options the adapter passes (`vertexai`,
//! `project`, `location`, `apiVersion`, `apiKey`, `httpOptions`). The Rust port has no SDK, so the
//! same options live on `GoogleClientConfig`; the assertions below read that struct and the request
//! URL it derives, and the streaming cases drive a real request against a recording server.

mod google_fixtures;

use google_fixtures::server::{sse_body, RecordingServer};
use google_fixtures::{builtin_model, context, user_text};
use maho_ai::api::google_vertex::{create_client, create_client_with_api_key, stream as stream_google_vertex};
use maho_ai::types::{Model, ProviderRequestOptions, StopReason, StreamOptions};
use maho_ai::utils::pi_user_agent::get_pi_user_agent;
use serde_json::json;
use std::collections::BTreeMap;

const REAL_API_KEY: &str = "AIzaSyExampleRealisticLookingApiKey123456";

fn options(api_key: Option<&str>, project: Option<&str>, location: Option<&str>, headers: Option<Vec<(&str, &str)>>) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions {
            api_key: api_key.map(str::to_owned),
            headers: headers.map(|headers| {
                headers.into_iter().map(|(name, value)| (name.to_owned(), Some(value.to_owned()))).collect()
            }),
            ..ProviderRequestOptions::default()
        },
        extra: {
            let mut extra = serde_json::Map::new();
            if let Some(project) = project {
                extra.insert("project".into(), json!(project));
            }
            if let Some(location) = location {
                extra.insert("location".into(), json!(location));
            }
            extra
        },
        ..StreamOptions::default()
    }
}

fn vertex_model(base_url: &str) -> Model {
    let mut model = builtin_model("google-vertex", "gemini-3-flash-preview");
    model.base_url = base_url.to_owned();
    model
}

fn headers_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect()
}

fn http_headers(entries: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
    Some(headers_map(entries))
}

/// The `options.apiKey` the adapter would hand the SDK for a given request.
fn resolved_api_key(api_key: Option<&str>) -> Option<String> {
    maho_ai::api::google_vertex::resolve_api_key(&options(api_key, None, None, None))
}

fn client_config_for(model: &Model, api_key: Option<&str>, headers: Option<BTreeMap<String, String>>) -> maho_ai::api::google_shared::GoogleClientConfig {
    match resolved_api_key(api_key) {
        Some(api_key) => create_client_with_api_key(model, &api_key, headers.as_ref()),
        None => create_client(model, "test-project", "us-central1", headers.as_ref(), None),
    }
}

#[test]
fn falls_back_to_adc_when_options_api_key_is_a_placeholder_marker() {
    let model = vertex_model("https://us-central1-aiplatform.googleapis.com");
    let client = client_config_for(&model, Some("<authenticated>"), None);
    assert!(client.vertexai);
    assert_eq!(client.project.as_deref(), Some("test-project"));
    assert_eq!(client.location.as_deref(), Some("us-central1"));
    assert_eq!(client.api_version.as_deref(), Some("v1"));
    assert_eq!(client.api_key, None);
}

#[test]
fn falls_back_to_adc_when_options_api_key_is_the_gcp_vertex_credentials_marker() {
    let model = vertex_model("https://us-central1-aiplatform.googleapis.com");
    let client = client_config_for(&model, Some("gcp-vertex-credentials"), None);
    assert!(client.vertexai);
    assert_eq!(client.project.as_deref(), Some("test-project"));
    assert_eq!(client.location.as_deref(), Some("us-central1"));
    assert_eq!(client.api_version.as_deref(), Some("v1"));
    assert_eq!(client.api_key, None);
}

#[test]
fn falls_back_to_adc_when_google_cloud_api_key_is_a_placeholder_marker() {
    let model = vertex_model("https://us-central1-aiplatform.googleapis.com");
    let client = client_config_for(&model, None, None);
    assert_eq!(client.api_key, None);
    assert_eq!(client.project.as_deref(), Some("test-project"));
    assert_eq!(client.location.as_deref(), Some("us-central1"));
    assert_eq!(client.api_version.as_deref(), Some("v1"));
}

#[test]
fn still_uses_the_api_key_client_for_real_api_keys() {
    let model = vertex_model("https://us-central1-aiplatform.googleapis.com");
    let client = client_config_for(&model, Some(REAL_API_KEY), None);
    assert!(client.vertexai);
    assert_eq!(client.api_key.as_deref(), Some(REAL_API_KEY));
    assert_eq!(client.api_version.as_deref(), Some("v1"));
    assert_eq!(client.project, None);
    assert_eq!(client.location, None);
}

#[test]
fn does_not_forward_generated_vertex_base_url_placeholders() {
    let model = builtin_model("google-vertex", "gemini-3-flash-preview");
    assert_eq!(model.base_url, "https://{location}-aiplatform.googleapis.com");
    let client = client_config_for(&model, None, http_headers(&[("User-Agent", &get_pi_user_agent())]));
    assert_eq!(client.base_url, None);
    assert_eq!(client.headers.as_ref(), Some(&headers_map(&[("User-Agent", &get_pi_user_agent())])));
}

#[test]
fn lets_explicit_headers_override_the_default_user_agent() {
    let model = vertex_model("https://us-central1-aiplatform.googleapis.com");
    let client = client_config_for(&model, None, http_headers(&[("User-Agent", "custom-agent")]));
    assert_eq!(client.headers.as_ref(), Some(&headers_map(&[("User-Agent", "custom-agent")])));
}

#[test]
fn forwards_custom_base_url_to_the_adc_client() {
    let model = vertex_model("https://proxy.example.com");
    let client = client_config_for(&model, None, http_headers(&[("User-Agent", &get_pi_user_agent())]));
    assert_eq!(client.base_url.as_deref(), Some("https://proxy.example.com"));
    assert!(client.base_url_resource_scope_collection);
    assert_eq!(client.api_version.as_deref(), Some("v1"));
}

#[test]
fn forwards_custom_base_url_to_the_api_key_client() {
    let model = vertex_model("https://proxy.example.com");
    let client = client_config_for(&model, Some(REAL_API_KEY), http_headers(&[("User-Agent", &get_pi_user_agent())]));
    assert_eq!(client.base_url.as_deref(), Some("https://proxy.example.com"));
    assert!(client.base_url_resource_scope_collection);
    assert_eq!(client.api_key.as_deref(), Some(REAL_API_KEY));
}

#[test]
fn does_not_append_api_version_when_custom_base_url_already_includes_one() {
    let model = vertex_model("https://proxy.example.com/v1/projects/test-project/locations/global");
    let client = client_config_for(&model, None, http_headers(&[("User-Agent", &get_pi_user_agent())]));
    assert_eq!(client.base_url.as_deref(), Some("https://proxy.example.com/v1/projects/test-project/locations/global"));
    assert!(client.base_url_resource_scope_collection);
    assert_eq!(client.api_version.as_deref(), Some(""));
}

#[tokio::test]
async fn streams_against_the_api_key_client_with_the_configured_headers() {
    let server = RecordingServer::start(
        sse_body(&[json!({
            "responseId": "vertex-response-id",
            "candidates": [{ "content": { "parts": [{ "text": "ok" }] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2 },
        })]),
        "text/event-stream",
    )
    .await;

    let message = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream_google_vertex(
            &vertex_model(&server.base_url()),
            &context(vec![user_text("hello")]),
            Some(options(Some(REAL_API_KEY), None, None, None)),
        )
        .result(),
    )
    .await
    .expect("bounded wait")
    .expect("result");

    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    let request = server.last_request();
    assert_eq!(request.url_path(), "/v1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse");
    assert_eq!(request.header("x-goog-api-key"), Some(REAL_API_KEY));
}
