//! Port of senpi packages/ai/src/providers/github-copilot.ts.

use crate::models::{CreateProviderOptions, Credential, FilterModels, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::Model;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

fn github_copilot_filter_models(models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
    let Some(credential) = credential else { return models };
    if credential.get("type").and_then(Value::as_str) != Some("oauth") {
        return models;
    }
    let Some(available_model_ids) = credential.get("availableModelIds").and_then(Value::as_array) else {
        return models;
    };
    let mut available = HashSet::with_capacity(available_model_ids.len());
    for id in available_model_ids {
        match id.as_str() {
            Some(id) => available.insert(id.to_owned()),
            None => return models,
        };
    }
    models.into_iter().filter(|model| available.contains(&model.id)).collect()
}

pub fn github_copilot_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert("anthropic-messages".to_owned(), builtin_api_streams("anthropic-messages"));
    api.insert("openai-completions".to_owned(), builtin_api_streams("openai-completions"));
    api.insert("openai-responses".to_owned(), builtin_api_streams("openai-responses"));
    let filter: FilterModels = Arc::new(github_copilot_filter_models);
    create_provider(CreateProviderOptions {
        id: "github-copilot".into(),
        name: Some("GitHub Copilot".into()),
        base_url: Some("https://api.individual.githubcopilot.com".into()),
        headers: None,
        models: super::github_copilot_models::github_copilot_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: Some(filter),
        api: ProviderApi::ByApi(api),
    })
}
