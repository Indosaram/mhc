//! Port of senpi packages/ai/src/providers/openai.ts.

use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider_with_classifiers};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn openai_provider() -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    let other_models = crate::image_models::get_image_models("openai")
        .into_iter().cloned().map(crate::types::AnyModel::Image)
        .chain(crate::models_generated::CLASSIFIER_MODELS.get("openai")
            .into_iter().flat_map(|models| models.values().cloned())
            .map(crate::types::AnyModel::Classifier))
        .collect();
    create_provider_with_classifiers(CreateProviderOptions {
        id: "openai".into(),
        name: Some("OpenAI".into()),
        base_url: Some("https://api.openai.com/v1".into()),
        headers: None,
        auth: Some(ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "OpenAI API key",
                &["OPENAI_API_KEY"],
            ))),
            oauth: Some(Arc::new(lazy_oauth(
                "OpenAI (ChatGPT subscription)",
                true,
                Some("Sign in with ChatGPT".into()),
                || Box::pin(crate::auth::oauth::load::load_openai_chatgpt_oauth()),
            ))),
        }),
        models: super::openai_models::openai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        images: [("openai-images".into(), super::images::register_builtins::generate_images_openai())].into_iter().collect(),
        api: ProviderApi::Single(builtin_api_streams("openai-responses")),
    }, None, other_models, indexmap::IndexMap::new())
}
