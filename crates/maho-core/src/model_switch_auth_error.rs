//! Port of senpi `packages/coding-agent/src/core/model-switch-auth-error.ts`.

/// An explicit model selection has no usable credentials for its provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("No API key for {provider}/{model_id}")]
pub struct ModelSwitchAuthError {
    pub provider: String,
    pub model_id: String,
}

impl ModelSwitchAuthError {
    pub const NAME: &'static str = "ModelSwitchAuthError";

    pub fn new(provider: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_provider_and_model() {
        assert_eq!(
            ModelSwitchAuthError::new("openai", "gpt-5").to_string(),
            "No API key for openai/gpt-5"
        );
    }
}
