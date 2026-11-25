//! Port of senpi packages/coding-agent/src/core/provider-base-url-override.ts.

use maho_ai::types::AnyModel;

use crate::model_config_schema::ModelsJsonProvider;

fn user_base_url<'a>(model: &AnyModel, config: Option<&'a ModelsJsonProvider>) -> Option<&'a str> {
    let config = config?;
    let per_model = match model {
        AnyModel::Chat(chat) => config
            .models
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|entry| entry.id == chat.id)
            .and_then(|entry| entry.base_url.as_deref()),
        AnyModel::Image(_) | AnyModel::Classifier(_) => None,
    };
    per_model.or(config.base_url.as_deref())
}

/// The last baseUrl-affecting composition step: a models.json `models[i].baseUrl`, else the
/// provider `baseUrl`, replaces whatever the extension, native provider, or `oauth.modifyModels`
/// set, for every model type.
pub fn apply_user_base_url(
    models: Vec<AnyModel>,
    config: Option<&ModelsJsonProvider>,
) -> Vec<AnyModel> {
    models
        .into_iter()
        .map(|mut model| {
            let Some(base_url) = user_base_url(&model, config) else {
                return model;
            };
            match &mut model {
                AnyModel::Chat(chat) if chat.base_url != base_url => {
                    chat.base_url = base_url.to_owned();
                }
                AnyModel::Image(img) if img.base_url != base_url => {
                    img.base_url = base_url.to_owned();
                }
                AnyModel::Classifier(cls) if cls.base_url != base_url => {
                    cls.base_url = base_url.to_owned();
                }
                _ => {}
            }
            model
        })
        .collect()
}
