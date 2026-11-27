//! Port of senpi packages/coding-agent/src/core/model-not-ready-error.ts.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelNotReadyCode {
    ProviderCredentialsMissing,
    ProviderCredentialsExpired,
    NoModelSelected,
}

impl ModelNotReadyCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderCredentialsMissing => "provider_credentials_missing",
            Self::ProviderCredentialsExpired => "provider_credentials_expired",
            Self::NoModelSelected => "no_model_selected",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelNotReadyError {
    pub message: String,
    pub code: ModelNotReadyCode,
    pub provider: Option<String>,
}

impl ModelNotReadyError {
    pub fn new(
        message: impl Into<String>,
        code: ModelNotReadyCode,
        provider: Option<String>,
    ) -> Self {
        Self {
            message: message.into(),
            code,
            provider,
        }
    }
}

impl fmt::Display for ModelNotReadyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelNotReadyError {}
