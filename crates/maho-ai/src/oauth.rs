//! Port of senpi packages/ai/src/oauth.ts.

pub use crate::auth::oauth::load::{
    BUILTIN_OAUTH_PROVIDERS, load_anthropic_oauth, load_builtin_oauth,
    register_bundled_oauth_flow_loaders,
};
pub use crate::auth::types::OAuthCredential as OAuthCredentials;
pub use crate::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectOption,
    OAuthSelectPrompt,
};
