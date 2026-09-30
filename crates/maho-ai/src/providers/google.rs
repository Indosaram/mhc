//! Port of senpi packages/ai/src/providers/google.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};
use std::sync::Arc;

pub fn stream_google(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    builtin_api_streams("google-generative-ai").stream(model, context, options)
}

pub fn stream_simple_google(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("google-generative-ai").stream_simple(model, context, options)
}

pub fn google_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "google".into(),
        name: Some("Google".into()),
        base_url: Some("https://generativelanguage.googleapis.com/v1beta".into()),
        headers: None,
        models: super::google_models::google_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("google-generative-ai")),
    })
}
