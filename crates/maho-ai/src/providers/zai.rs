//! Port of senpi packages/ai/src/providers/zai.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn zai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "zai".into(),
        name: Some("Z.AI".into()),
        base_url: Some("https://api.z.ai/api/coding/paas/v4".into()),
        headers: None,
        models: super::zai::zai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
