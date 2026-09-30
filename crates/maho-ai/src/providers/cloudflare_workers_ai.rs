//! Port of senpi packages/ai/src/providers/cloudflare-workers-ai.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::{builtin_api_streams, cloudflare_stream::cloudflare_streams};
use std::sync::Arc;

pub fn cloudflare_workers_ai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "cloudflare-workers-ai".into(),
        name: Some("Cloudflare Workers AI".into()),
        base_url: None,
        headers: None,
        models: super::cloudflare_workers_ai_models::cloudflare_workers_ai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(cloudflare_streams(builtin_api_streams("openai-completions"))),
    })
}
