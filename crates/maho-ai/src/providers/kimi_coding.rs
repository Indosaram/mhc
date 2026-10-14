//! Port of senpi packages/ai/src/providers/kimi-coding.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::lazy_oauth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider_with_retry_policy};
use crate::providers::builtin_api_streams;
use crate::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE;
use std::sync::Arc;

pub fn kimi_coding_provider() -> Arc<dyn Provider> {
    create_provider_with_retry_policy(
        CreateProviderOptions {
            id: "kimi-coding".into(),
            name: Some("Kimi For Coding".into()),
            base_url: Some("https://api.kimi.com/coding".into()),
            headers: None,
            auth: Some(ProviderAuth { api_key: Some(Arc::new(super::kimi_coding_auth::kimi_code_api_key_auth())), oauth: Some(Arc::new(lazy_oauth("Kimi Code (subscription)", true, Some("Sign in with Kimi Code".into()), || Box::pin(crate::auth::oauth::load::load_kimi_coding_oauth())))) }),
            models: super::kimi_coding_models::kimi_coding_models(),
            fetch_models: None,
            restore_models: None,
            filter_models: None,
            api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
        },
        // Same upstream service (api.kimi.com/coding, protocol anthropic) as the
        // kimi-code CLI, so its own retry policy applies verbatim.
        Some(KIMI_CODE_RETRY_PROFILE.clone()),
    )
}
