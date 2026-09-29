//! Port of senpi packages/ai/src/providers/ant-ling.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn ant_ling_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "ant-ling".into(),
        name: Some("Ant Ling".into()),
        base_url: Some("https://api.ant-ling.com/v1".into()),
        headers: None,
        models: super::ant_ling::ant_ling_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
