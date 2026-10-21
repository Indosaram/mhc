//! Port of senpi packages/ai/src/providers/openrouter.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider_with_classifiers};
use crate::providers::builtin_api_streams;
use indexmap::IndexMap;
use std::sync::Arc;

pub fn openrouter_provider() -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    let mut api = IndexMap::new();
    api.insert("anthropic-messages".to_owned(), builtin_api_streams("anthropic-messages"));
    api.insert("openai-completions".to_owned(), builtin_api_streams("openai-completions"));
    let other_models = crate::image_models::get_image_models("openrouter")
        .into_iter().cloned().map(crate::types::AnyModel::Image)
        .chain(crate::models_generated::CLASSIFIER_MODELS.get("openrouter")
            .into_iter().flat_map(|models| models.values().cloned())
            .map(crate::types::AnyModel::Classifier))
        .collect();
    let classifier: Arc<dyn crate::types::ProviderClassifier> = Arc::new(crate::models::BuiltinClassifier);
    create_provider_with_classifiers(CreateProviderOptions {
        id: "openrouter".into(),
        name: Some("OpenRouter".into()),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("OpenRouter API key", &["OPENROUTER_API_KEY"]))), oauth: Some(Arc::new(lazy_oauth("OpenRouter OAuth", false, Some("Sign in with OpenRouter".into()), || Box::pin(crate::auth::oauth::load::load_open_router_oauth())))) }),
        models: super::openrouter_models::openrouter_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        images: [("openrouter-images".into(), super::images::register_builtins::generate_images_openrouter())].into_iter().collect(),
        api: ProviderApi::ByApi(api),
    }, None, other_models, [("typesafe-system-one".into(), classifier)].into_iter().collect())
}
