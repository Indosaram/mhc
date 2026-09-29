//! Port of senpi packages/ai/src/providers/nvidia.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn nvidia_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "nvidia".into(),
        name: Some("NVIDIA".into()),
        base_url: Some("https://integrate.api.nvidia.com/v1".into()),
        headers: None,
        models: super::nvidia::nvidia_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
