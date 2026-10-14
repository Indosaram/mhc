//! Port of senpi packages/ai/src/providers/cloudflare-auth.ts.

use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthPrompt, AuthPromptKind, AuthResult, ModelAuth,
    ProviderAuthInteraction,
};
use crate::types::{ProviderEnv, ProviderHeaders};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;

pub const CLOUDFLARE_API_KEY: &str = "CLOUDFLARE_API_KEY";
pub const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
pub const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloudflareAuthKind {
    WorkersAi,
    AiGateway,
}

async fn resolve_value(
    name: &str,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortSignal,
) -> anyhow::Result<Option<String>> {
    let from_credential = match credential {
        Some(cred) => {
            if name == CLOUDFLARE_API_KEY {
                cred.key.clone()
            } else {
                cred.env.as_ref().and_then(|env| env.get(name)).cloned()
            }
        }
        None => None,
    };
    if let Some(value) = from_credential {
        return Ok(Some(value));
    }
    signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
    let value = ctx.env(name).await;
    signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
    Ok(value.filter(|v| !v.is_empty()))
}

struct ResolvedCloudflareEnv {
    api_key: String,
    env: ProviderEnv,
    source: String,
}

async fn resolve_cloudflare_env(
    kind: CloudflareAuthKind,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortSignal,
) -> anyhow::Result<Option<ResolvedCloudflareEnv>> {
    let api_key = resolve_value(CLOUDFLARE_API_KEY, ctx, credential, signal).await?;
    let account_id = resolve_value(CLOUDFLARE_ACCOUNT_ID, ctx, credential, signal).await?;
    let gateway_id = if kind == CloudflareAuthKind::AiGateway {
        resolve_value(CLOUDFLARE_GATEWAY_ID, ctx, credential, signal).await?
    } else {
        None
    };

    let api_key = api_key.filter(|v| !v.is_empty());
    let account_id = account_id.filter(|v| !v.is_empty());
    let gateway_id = gateway_id.filter(|v| !v.is_empty());

    let (Some(api_key), Some(account_id)) = (api_key, account_id) else {
        return Ok(None);
    };
    if kind == CloudflareAuthKind::AiGateway && gateway_id.is_none() {
        return Ok(None);
    }

    let mut env = ProviderEnv::new();
    env.insert(CLOUDFLARE_ACCOUNT_ID.to_string(), account_id);
    if let Some(gateway_id) = gateway_id {
        env.insert(CLOUDFLARE_GATEWAY_ID.to_string(), gateway_id);
    }

    let source = if credential.is_some() {
        "stored credential".to_string()
    } else {
        CLOUDFLARE_API_KEY.to_string()
    };

    Ok(Some(ResolvedCloudflareEnv { api_key, env, source }))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CloudflareWorkersAIAuth;

#[async_trait]
impl ApiKeyAuth for CloudflareWorkersAIAuth {
    fn name(&self) -> &str {
        "Cloudflare API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        let key = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret { message: "Enter Cloudflare API key".into(), placeholder: None },
                signal: None,
            })
            .await?;
        let account_id = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Text { message: "Enter Cloudflare account ID".into(), placeholder: None },
                signal: None,
            })
            .await?;
        let mut env = ProviderEnv::new();
        env.insert(CLOUDFLARE_ACCOUNT_ID.to_string(), account_id);
        Ok(ApiKeyCredential { key: Some(key), env: Some(env) })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        let resolved = resolve_cloudflare_env(CloudflareAuthKind::WorkersAi, ctx, credential, signal).await?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        Ok(Some(AuthResult {
            auth: ModelAuth { api_key: Some(resolved.api_key), headers: None, base_url: None },
            env: Some(resolved.env),
            source: Some(resolved.source),
        }))
    }
}

pub fn cloudflare_workers_ai_auth() -> CloudflareWorkersAIAuth {
    CloudflareWorkersAIAuth
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CloudflareAIGatewayAuth;

#[async_trait]
impl ApiKeyAuth for CloudflareAIGatewayAuth {
    fn name(&self) -> &str {
        "Cloudflare API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        let key = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret { message: "Enter Cloudflare API key".into(), placeholder: None },
                signal: None,
            })
            .await?;
        let account_id = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Text { message: "Enter Cloudflare account ID".into(), placeholder: None },
                signal: None,
            })
            .await?;
        let gateway_id = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Text { message: "Enter Cloudflare AI Gateway ID".into(), placeholder: None },
                signal: None,
            })
            .await?;
        let mut env = ProviderEnv::new();
        env.insert(CLOUDFLARE_ACCOUNT_ID.to_string(), account_id);
        env.insert(CLOUDFLARE_GATEWAY_ID.to_string(), gateway_id);
        Ok(ApiKeyCredential { key: Some(key), env: Some(env) })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        let resolved = resolve_cloudflare_env(CloudflareAuthKind::AiGateway, ctx, credential, signal).await?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        let mut headers = ProviderHeaders::new();
        headers.insert("cf-aig-authorization".to_string(), Some(format!("Bearer {}", resolved.api_key)));
        headers.insert("Authorization".to_string(), None);
        headers.insert("x-api-key".to_string(), None);
        Ok(Some(AuthResult {
            auth: ModelAuth { api_key: None, headers: Some(headers), base_url: None },
            env: Some(resolved.env),
            source: Some(resolved.source),
        }))
    }
}

pub fn cloudflare_ai_gateway_auth() -> CloudflareAIGatewayAuth {
    CloudflareAIGatewayAuth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::AuthEvent;
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
    async fn workers_ai_auth_resolves_from_ambient_env() {
        let mut env_map = HashMap::new();
        env_map.insert("CLOUDFLARE_API_KEY".to_string(), "cf-ambient-key".to_string());
        env_map.insert("CLOUDFLARE_ACCOUNT_ID".to_string(), "acct-123".to_string());
        let ctx = TestAuthContext { env_map };
        let signal = crate::utils::abort::AbortController::new().signal();

        let auth = cloudflare_workers_ai_auth();
        let res = auth.resolve(&ctx, None, &signal).await.unwrap().expect("resolved");
        assert_eq!(res.auth.api_key.as_deref(), Some("cf-ambient-key"));
        assert_eq!(res.source.as_deref(), Some("CLOUDFLARE_API_KEY"));
        let env = res.env.expect("env");
        assert_eq!(env.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str), Some("acct-123"));
        assert!(!env.contains_key("CLOUDFLARE_GATEWAY_ID"));
    }

    #[tokio::test]
    async fn ai_gateway_auth_resolves_headers_and_prefers_credential_over_env() {
        let mut env_map = HashMap::new();
        env_map.insert("CLOUDFLARE_API_KEY".to_string(), "env-key".to_string());
        env_map.insert("CLOUDFLARE_ACCOUNT_ID".to_string(), "env-acct".to_string());
        env_map.insert("CLOUDFLARE_GATEWAY_ID".to_string(), "env-gw".to_string());
        let ctx = TestAuthContext { env_map };
        let signal = crate::utils::abort::AbortController::new().signal();

        let mut cred_env = ProviderEnv::new();
        cred_env.insert("CLOUDFLARE_ACCOUNT_ID".to_string(), "cred-acct".to_string());
        let credential = ApiKeyCredential {
            key: Some("cred-key".to_string()),
            env: Some(cred_env),
        };

        let auth = cloudflare_ai_gateway_auth();
        let res = auth.resolve(&ctx, Some(&credential), &signal).await.unwrap().expect("resolved");
        assert_eq!(res.source.as_deref(), Some("stored credential"));
        assert!(res.auth.api_key.is_none());
        let headers = res.auth.headers.expect("headers");
        assert_eq!(headers.get("cf-aig-authorization").and_then(|v| v.as_deref()), Some("Bearer cred-key"));
        assert_eq!(headers.get("Authorization"), Some(&None));
        assert_eq!(headers.get("x-api-key"), Some(&None));

        let env = res.env.expect("env");
        assert_eq!(env.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str), Some("cred-acct"));
        assert_eq!(env.get("CLOUDFLARE_GATEWAY_ID").map(String::as_str), Some("env-gw"));
    }

    #[tokio::test]
    async fn workers_ai_auth_returns_none_if_missing_key_or_account() {
        let ctx = TestAuthContext { env_map: HashMap::new() };
        let signal = crate::utils::abort::AbortController::new().signal();
        let auth = cloudflare_workers_ai_auth();
        assert!(auth.resolve(&ctx, None, &signal).await.unwrap().is_none());

        let mut env_map = HashMap::new();
        env_map.insert("CLOUDFLARE_API_KEY".to_string(), "cf-key".to_string());
        let ctx = TestAuthContext { env_map };
        assert!(auth.resolve(&ctx, None, &signal).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ai_gateway_auth_returns_none_if_missing_gateway() {
        let mut env_map = HashMap::new();
        env_map.insert("CLOUDFLARE_API_KEY".to_string(), "cf-key".to_string());
        env_map.insert("CLOUDFLARE_ACCOUNT_ID".to_string(), "acct-123".to_string());
        let ctx = TestAuthContext { env_map };
        let signal = crate::utils::abort::AbortController::new().signal();
        let auth = cloudflare_ai_gateway_auth();
        assert!(auth.resolve(&ctx, None, &signal).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn login_prompts_workers_ai() {
        let interaction = ProviderAuthInteraction::new(
            crate::utils::abort::AbortController::new().signal(),
            Arc::new(MockInteraction {
                responses: std::sync::Mutex::new(vec!["my-key".into(), "my-acct".into()]),
            }),
        );
        let auth = cloudflare_workers_ai_auth();
        let cred = auth.login(&interaction).await.unwrap();
        assert_eq!(cred.key.as_deref(), Some("my-key"));
        let env = cred.env.expect("env");
        assert_eq!(env.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str), Some("my-acct"));
    }

    #[tokio::test]
    async fn login_prompts_ai_gateway() {
        let interaction = ProviderAuthInteraction::new(
            crate::utils::abort::AbortController::new().signal(),
            Arc::new(MockInteraction {
                responses: std::sync::Mutex::new(vec!["my-key".into(), "my-acct".into(), "my-gw".into()]),
            }),
        );
        let auth = cloudflare_ai_gateway_auth();
        let cred = auth.login(&interaction).await.unwrap();
        assert_eq!(cred.key.as_deref(), Some("my-key"));
        let env = cred.env.expect("env");
        assert_eq!(env.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str), Some("my-acct"));
        assert_eq!(env.get("CLOUDFLARE_GATEWAY_ID").map(String::as_str), Some("my-gw"));
    }
}
