//! Port of senpi packages/ai/src/api/cloudflare.ts.

use crate::types::{Model, ProviderEnv};
use crate::utils::provider_env::get_provider_env_value;

const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

/// Workers AI direct endpoint.
pub const CLOUDFLARE_WORKERS_AI_BASE_URL: &str =
    "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1";

/// AI Gateway Unified API. https://developers.cloudflare.com/ai-gateway/usage/unified-api/
pub const CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat";

/// AI Gateway -> [OI] passthrough. Used until /compat supports /v1/responses.
pub const CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai";

/// AI Gateway -> Anthropic passthrough.
pub const CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic";

pub fn is_cloudflare_provider(provider: &str) -> bool {
    provider == "cloudflare-workers-ai" || provider == "cloudflare-ai-gateway"
}

pub fn resolve_cloudflare_base_url(model: &Model, env: Option<&ProviderEnv>) -> String {
    let account_id = get_provider_env_value(CLOUDFLARE_ACCOUNT_ID, env).unwrap_or_default();
    let gateway_id = get_provider_env_value(CLOUDFLARE_GATEWAY_ID, env).unwrap_or_default();
    model
        .base_url
        .replace(&format!("{{{CLOUDFLARE_ACCOUNT_ID}}}"), &account_id)
        .replace(&format!("{{{CLOUDFLARE_GATEWAY_ID}}}"), &gateway_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_model() -> Model {
        crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone()
    }

    fn model(base_url: &str) -> Model {
        let mut model = any_model();
        model.base_url = base_url.into();
        model
    }

    #[test]
    fn recognizes_both_cloudflare_providers() {
        assert!(is_cloudflare_provider("cloudflare-workers-ai"));
        assert!(is_cloudflare_provider("cloudflare-ai-gateway"));
        assert!(!is_cloudflare_provider("openai"));
    }

    #[test]
    fn substitutes_account_and_gateway_placeholders_from_env() {
        let env: ProviderEnv = [
            ("CLOUDFLARE_ACCOUNT_ID".to_owned(), "acct".to_owned()),
            ("CLOUDFLARE_GATEWAY_ID".to_owned(), "gw".to_owned()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve_cloudflare_base_url(&model(CLOUDFLARE_WORKERS_AI_BASE_URL), Some(&env)),
            "https://api.cloudflare.com/client/v4/accounts/acct/ai/v1"
        );
        assert_eq!(
            resolve_cloudflare_base_url(&model(CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL), Some(&env)),
            "https://gateway.ai.cloudflare.com/v1/acct/gw/anthropic"
        );
    }

    #[test]
    fn missing_env_leaves_empty_placeholders() {
        assert_eq!(
            resolve_cloudflare_base_url(&model(CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL), None),
            "https://gateway.ai.cloudflare.com/v1///compat"
        );
    }
}
