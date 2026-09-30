//! Port of senpi packages/ai/src/providers/anthropic.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};
use std::sync::Arc;

pub fn stream_anthropic(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    builtin_api_streams("anthropic-messages").stream(model, context, options)
}

pub fn stream_simple_anthropic(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("anthropic-messages").stream_simple(model, context, options)
}

pub fn anthropic_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "anthropic".into(),
        name: Some("Anthropic".into()),
        base_url: Some("https://api.anthropic.com".into()),
        headers: None,
        models: super::anthropic_models::anthropic_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
    })
}
