//! The Google cases of senpi packages/ai/test/fetch-option.test.ts and
//! packages/ai/test/pre-generation-error.test.ts.
//!
//! Both TS files are cross-adapter suites; only the cases naming a Google adapter belong to this
//! lane. The custom-fetch case ports fully. The `globalThis.fetch` case cannot: the Rust
//! `ProviderRequestOptions::fetch` is an explicit `reqwest::Client` with no ambient-client identity
//! to compare against, so any client a caller hands over is by construction a custom one. The
//! missing-auth case asserts the error text the ported adapter produces.

mod google_fixtures;

use google_fixtures::model_with;
use maho_ai::api::google_generative_ai::{stream as stream_google, stream_simple as stream_simple_google};
use maho_ai::api::google_vertex::stream as stream_vertex;
use maho_ai::types::{Context, Message, ProviderRequestOptions, StreamOptions};

fn test_model(api: &str) -> maho_ai::types::Model {
    let mut model = model_with(api, "test-provider", "test-model");
    model.base_url = "https://example.invalid".into();
    model.reasoning = false;
    model
}

fn options(fetch: Option<reqwest::Client>) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions {
            api_key: Some("test-key".into()),
            fetch,
            ..ProviderRequestOptions::default()
        },
        ..StreamOptions::default()
    }
}

fn empty_context() -> Context {
    Context { system_prompt: None, messages: Vec::<Message>::new(), tools: None }
}

#[tokio::test]
async fn rejects_custom_fetch_for_google_adapters_instead_of_silently_bypassing_it() {
    let custom = reqwest::Client::new();

    let google = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream_google(&test_model("google-generative-ai"), &empty_context(), Some(options(Some(custom.clone()))))
            .result(),
    )
    .await
    .expect("bounded wait")
    .expect("result");
    let vertex = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream_vertex(&test_model("google-vertex"), &empty_context(), Some(options(Some(custom)))).result(),
    )
    .await
    .expect("bounded wait")
    .expect("result");

    assert_eq!(
        google.error_message.as_deref(),
        Some("Custom fetch is not supported by the Google Generative AI adapter")
    );
    assert_eq!(vertex.error_message.as_deref(), Some("Custom fetch is not supported by the Google Vertex adapter"));
}

#[tokio::test]
async fn throws_when_auth_is_missing_for_the_google_generative_ai_adapter() {
    let stream = stream_simple_google(
        &test_model("google-generative-ai"),
        &empty_context(),
        Some(maho_ai::types::SimpleStreamOptions::default()),
    );
    let message = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result");
    assert_eq!(message.error_message.as_deref(), Some("No API key for provider: test-provider"));
}
