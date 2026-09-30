//! Port of senpi packages/ai/src/providers/devin.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, RestoreModels, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::Model;
use indexmap::IndexMap;
use std::sync::Arc;

pub fn devin_provider() -> Arc<dyn Provider> {
    let restore: RestoreModels = Arc::new(|models| {
        Ok(models.into_iter().map(|model| Model { reasoning: false, ..model }).collect())
    });
    let mut api = IndexMap::new();
    api.insert("devin-agent".to_owned(), builtin_api_streams("devin-agent"));
    create_provider(CreateProviderOptions {
        id: "devin".into(),
        name: Some("Devin".into()),
        base_url: Some("https://server.codeium.com".into()),
        headers: None,
        models: super::devin_models::devin_models(),
        fetch_models: None,
        restore_models: Some(restore),
        filter_models: None,
        api: ProviderApi::ByApi(api),
    })
}
