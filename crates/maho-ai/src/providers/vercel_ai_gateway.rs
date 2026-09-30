//! Port of senpi packages/ai/src/providers/vercel-ai-gateway.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn vercel_ai_gateway_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "vercel-ai-gateway".into(),
        name: Some("Vercel AI Gateway".into()),
        base_url: Some("https://ai-gateway.vercel.sh".into()),
        headers: None,
        models: super::vercel_ai_gateway_models::vercel_ai_gateway_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
    })
}
