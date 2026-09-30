//! Port of senpi packages/ai/src/providers/cloudflare-ai-gateway.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::{builtin_api_streams, cloudflare_stream::cloudflare_streams};
use crate::types::Model;
use indexmap::IndexMap;
use std::sync::Arc;

const WORKERS_AI_GATEWAY_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat";

fn gateway_models() -> Vec<Model> {
    let mut models = super::cloudflare_ai_gateway_models::cloudflare_ai_gateway_models();
    models.extend(super::cloudflare_workers_ai_models::cloudflare_workers_ai_models().into_iter().map(|model| Model {
        provider: "cloudflare-ai-gateway".into(),
        id: format!("workers-ai/{}", model.id),
        base_url: WORKERS_AI_GATEWAY_BASE_URL.to_owned(),
        ..model
    }));
    models
}

pub fn cloudflare_ai_gateway_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    for id in ["anthropic-messages", "openai-completions", "openai-responses"] {
        api.insert(id.to_owned(), cloudflare_streams(builtin_api_streams(id)));
    }
    create_provider(CreateProviderOptions {
        id: "cloudflare-ai-gateway".into(),
        name: Some("Cloudflare AI Gateway".into()),
        base_url: None,
        headers: None,
        models: gateway_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(api),
    })
}
