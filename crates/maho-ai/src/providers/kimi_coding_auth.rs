//! Port of senpi packages/ai/src/providers/kimi-coding-auth.ts.

use crate::auth::oauth::kimi_region::{
    KimiCodeEndpointInput, choose_kimi_code_login_endpoints, resolve_kimi_code_endpoints,
    KIMI_CODE_OAUTH_HOST_ENV, KIMI_CODE_REGION_ENV, KIMI_OAUTH_HOST_ENV,
};
use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthPrompt, AuthPromptKind, AuthResult, ModelAuth,
    ProviderAuthInteraction,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;

const KIMI_API_KEY_ENV: &str = "KIMI_API_KEY";

async fn env_value(
    ctx: &dyn AuthContext,
    name: &str,
    signal: &AbortSignal,
) -> anyhow::Result<Option<String>> {
    let value = ctx.env(name).await;
    signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
    Ok(value.filter(|v| !v.is_empty()))
}

/// Kimi Code api-key auth: the key resolves like `envApiKeyAuth` (stored key,
/// then `KIMI_API_KEY`), and the account's region rides along as credential env
/// so an international key is routed to kimi.ai. No client identity headers:
/// a platform key is not a subscription session (#1504).
#[derive(Debug, Clone, Copy, Default)]
pub struct KimiCodeApiKeyAuth;

#[async_trait]
impl ApiKeyAuth for KimiCodeApiKeyAuth {
    fn name(&self) -> &str {
        "Kimi API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        let endpoints = choose_kimi_code_login_endpoints(interaction).await?;
        let key = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret { message: "Enter Kimi API key".into(), placeholder: None },
                signal: None,
            })
            .await?;
        interaction.signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: endpoints.env,
        })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
        let key = match credential.and_then(|c| c.key.clone()).filter(|k| !k.is_empty()) {
            Some(k) => Some(k),
            None => env_value(ctx, KIMI_API_KEY_ENV, signal).await?,
        };
        let Some(key) = key else {
            return Ok(None);
        };

        let stored_region = credential.and_then(|c| c.env.as_ref()).and_then(|e| e.get(KIMI_CODE_REGION_ENV)).cloned();
        let stored_oauth_host = credential.and_then(|c| c.env.as_ref()).and_then(|e| e.get(KIMI_CODE_OAUTH_HOST_ENV)).cloned();
        let env_oauth_host = match env_value(ctx, KIMI_CODE_OAUTH_HOST_ENV, signal).await? {
            Some(h) => Some(h),
            None => env_value(ctx, KIMI_OAUTH_HOST_ENV, signal).await?,
        };
        let env_region = env_value(ctx, KIMI_CODE_REGION_ENV, signal).await?;

        let endpoints = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            stored_region,
            stored_oauth_host,
            env_oauth_host,
            env_region,
        });

        let source = if credential.and_then(|c| c.key.as_ref()).is_some_and(|k| !k.is_empty()) {
            "stored credential".to_string()
        } else {
            KIMI_API_KEY_ENV.to_string()
        };

        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                headers: None,
                base_url: endpoints.api_base_url,
            },
            env: credential.and_then(|c| c.env.clone()),
            source: Some(source),
        }))
    }
}

pub fn kimi_code_api_key_auth() -> KimiCodeApiKeyAuth {
    KimiCodeApiKeyAuth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::AuthEvent;
    use crate::types::ProviderEnv;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct TestAuthContext {
        env_map: HashMap<String, String>,
    }

    #[async_trait]
    impl AuthContext for TestAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.env_map.get(name).cloned()
        }

        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    struct MockInteraction {
        responses: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl crate::auth::types::AuthInteraction for MockInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            None
        }
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            let mut guard = self.responses.lock().unwrap();
            if guard.is_empty() {
                anyhow::bail!("No more mock responses");
            }
            Ok(guard.remove(0))
        }
        fn notify(&self, _event: AuthEvent) {}
    }

    #[tokio::test]
    async fn kimi_auth_resolves_stored_key_with_region() {
        let ctx = TestAuthContext { env_map: HashMap::new() };
        let signal = crate::utils::abort::AbortController::new().signal();

        let mut env = ProviderEnv::new();
        env.insert(KIMI_CODE_REGION_ENV.to_string(), "global".to_string());
        let credential = ApiKeyCredential {
            key: Some("stored-kimi-key".to_string()),
            env: Some(env),
        };

        let auth = kimi_code_api_key_auth();
        let res = auth.resolve(&ctx, Some(&credential), &signal).await.unwrap().expect("resolved");
        assert_eq!(res.auth.api_key.as_deref(), Some("stored-kimi-key"));
        assert_eq!(res.auth.base_url.as_deref(), Some("https://api.kimi.ai/coding"));
        assert_eq!(res.source.as_deref(), Some("stored credential"));
        assert_eq!(res.env.as_ref().and_then(|e| e.get(KIMI_CODE_REGION_ENV)).map(String::as_str), Some("global"));
    }

    #[tokio::test]
    async fn kimi_auth_resolves_env_key_with_default_region() {
        let mut env_map = HashMap::new();
        env_map.insert("KIMI_API_KEY".to_string(), "env-kimi-key".to_string());
        let ctx = TestAuthContext { env_map };
        let signal = crate::utils::abort::AbortController::new().signal();

        let auth = kimi_code_api_key_auth();
        let res = auth.resolve(&ctx, None, &signal).await.unwrap().expect("resolved");
        assert_eq!(res.auth.api_key.as_deref(), Some("env-kimi-key"));
        // Default mainland-cn region omits base_url override
        assert!(res.auth.base_url.is_none());
        assert_eq!(res.source.as_deref(), Some("KIMI_API_KEY"));
        assert!(res.env.is_none());
    }

    #[tokio::test]
    async fn kimi_auth_returns_none_if_no_key_found() {
        let ctx = TestAuthContext { env_map: HashMap::new() };
        let signal = crate::utils::abort::AbortController::new().signal();
        let auth = kimi_code_api_key_auth();
        assert!(auth.resolve(&ctx, None, &signal).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn kimi_auth_login_prompts_region_and_key() {
        let interaction = ProviderAuthInteraction::new(
            crate::utils::abort::AbortController::new().signal(),
            Arc::new(MockInteraction {
                responses: std::sync::Mutex::new(vec!["global".into(), "entered-key".into()]),
            }),
        );
        let auth = kimi_code_api_key_auth();
        let cred = auth.login(&interaction).await.unwrap();
        assert_eq!(cred.key.as_deref(), Some("entered-key"));
        let env = cred.env.expect("env");
        assert_eq!(env.get(KIMI_CODE_REGION_ENV).map(String::as_str), Some("global"));
    }
}
