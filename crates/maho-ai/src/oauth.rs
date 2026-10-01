//! Port of senpi packages/ai/src/oauth.ts.

pub use crate::auth::oauth::load::{load_anthropic_oauth, register_bundled_oauth_flow_loaders};
pub use crate::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectOption, OAuthSelectPrompt,
};
pub use crate::auth::types::OAuthCredential as OAuthCredentials;
