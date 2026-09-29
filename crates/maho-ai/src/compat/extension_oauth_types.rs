//! Port of senpi packages/ai/src/compat/extension-oauth-types.ts.

use crate::types::BoxFuture;
use crate::utils::abort::AbortSignal;

pub type OAuthProviderId = String;

#[derive(Debug, Clone, Default)]
pub struct OAuthPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub allow_empty: Option<bool>,
    pub signal: Option<AbortSignal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAuthInfo {
    pub url: String,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDeviceCodeInfo {
    pub user_code: String,
    pub verification_uri: String,
    pub interval_seconds: Option<u64>,
    pub expires_in_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSelectOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OAuthSelectPrompt {
    pub message: String,
    pub options: Vec<OAuthSelectOption>,
    pub signal: Option<AbortSignal>,
}

pub trait OAuthLoginCallbacks: Send + Sync {
    fn on_auth(&self, info: OAuthAuthInfo);
    fn on_device_code(&self, info: OAuthDeviceCodeInfo);
    fn on_prompt(&self, prompt: OAuthPrompt) -> BoxFuture<'_, String>;
    fn on_progress(&self, _message: &str) {}
    fn on_manual_code_input(&self) -> Option<BoxFuture<'_, String>> {
        None
    }
    fn on_select(&self, prompt: OAuthSelectPrompt) -> BoxFuture<'_, Option<String>>;
    fn signal(&self) -> Option<AbortSignal> {
        None
    }
}
