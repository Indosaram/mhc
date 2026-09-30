//! Port of senpi packages/ai/src/providers/google-vertex.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};
use std::sync::Arc;

pub fn stream_google_vertex(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    builtin_api_streams("google-vertex").stream(model, context, options)
}

pub fn stream_simple_google_vertex(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("google-vertex").stream_simple(model, context, options)
}

pub fn google_vertex_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "google-vertex".into(),
        name: Some("Google Vertex AI".into()),
        base_url: None,
        headers: None,
        models: super::google_vertex_models::google_vertex_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("google-vertex")),
    })
}
