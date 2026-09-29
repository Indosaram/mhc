/// A split `provider/model` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFormat {
    pub provider_id: String,
    pub model_id: String,
}

/// `string | { providerID; modelID }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelFormatInput {
    String(String),
    Pair(ModelFormat),
}

/// Splits `provider/model` strings on the first slash; pairs pass through unchanged.
#[must_use]
pub fn normalize_model_format(model: Option<&ModelFormatInput>) -> Option<ModelFormat> {
    match model? {
        ModelFormatInput::Pair(pair) => Some(pair.clone()),
        ModelFormatInput::String(model) => {
            let (provider_id, model_id) = model.split_once('/')?;
            Some(ModelFormat {
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
            })
        }
    }
}
