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
            if provider_id.trim().is_empty() || model_id.trim().is_empty() {
                return None;
            }
            Some(ModelFormat {
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_string_segments() {
        for model in ["/glm-5-free", "opencode/", "opencode/   ", " /model"] {
            let input = ModelFormatInput::String(model.to_string());
            assert_eq!(normalize_model_format(Some(&input)), None, "{model:?}");
        }
    }

    #[test]
    fn preserves_nonblank_string_segments_and_nested_paths() {
        let input = ModelFormatInput::String(" provider / model/path ".to_string());
        assert_eq!(normalize_model_format(Some(&input)), Some(ModelFormat {
            provider_id: " provider ".to_string(),
            model_id: " model/path ".to_string(),
        }));
    }

    #[test]
    fn preserves_object_segments_even_when_blank() {
        let pair = ModelFormat { provider_id: String::new(), model_id: " ".to_string() };
        assert_eq!(normalize_model_format(Some(&ModelFormatInput::Pair(pair.clone()))), Some(pair));
    }
}
