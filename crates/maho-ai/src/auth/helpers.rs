//! Port of senpi packages/ai/src/auth/helpers.ts.

use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthPrompt, AuthPromptKind, AuthResult, ModelAuth, OAuthAuth,
    OAuthCredential, ProviderAuthInteraction,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub struct EnvApiKeyAuth {
    name: String,
    env_vars: Vec<String>,
}

pub fn env_api_key_auth(name: impl Into<String>, env_vars: &[&str]) -> EnvApiKeyAuth {
    EnvApiKeyAuth { name: name.into(), env_vars: env_vars.iter().map(|s| s.to_string()).collect() }
}

#[async_trait]
impl ApiKeyAuth for EnvApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        interaction.signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
        let key = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret { message: format!("Enter {}", self.name), placeholder: None },
                signal: None,
            })
            .await?;
        interaction.signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
        Ok(ApiKeyCredential { key: Some(key), env: None })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
        if let Some(credential) = credential
            && let Some(key) = &credential.key {
                return Ok(Some(AuthResult {
                    auth: ModelAuth { api_key: Some(key.clone()), ..Default::default() },
                    env: credential.env.clone(),
                    source: Some("stored credential".into()),
                }));
            }
        for env_var in &self.env_vars {
            let value = ctx.env(env_var).await;
            signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
            if let Some(value) = value
                && !value.is_empty() {
                    return Ok(Some(AuthResult {
                        auth: ModelAuth { api_key: Some(value), ..Default::default() },
                        env: None,
                        source: Some(env_var.clone()),
                    }));
                }
        }
        Ok(None)
    }
}

type LoadFuture = Pin<Box<dyn Future<Output = anyhow::Result<Arc<dyn OAuthAuth>>> + Send>>;

pub struct LazyOAuth {
    name: String,
    is_subscription: bool,
    login_label: Option<String>,
    load: Arc<dyn Fn() -> LoadFuture + Send + Sync>,
    loaded: Mutex<Option<Arc<dyn OAuthAuth>>>,
}

pub fn lazy_oauth(
    name: impl Into<String>,
    is_subscription: bool,
    login_label: Option<String>,
    load: impl Fn() -> LoadFuture + Send + Sync + 'static,
) -> LazyOAuth {
    LazyOAuth { name: name.into(), is_subscription, login_label, load: Arc::new(load), loaded: Mutex::new(None) }
}

impl LazyOAuth {
    async fn loaded(&self) -> anyhow::Result<Arc<dyn OAuthAuth>> {
        if let Some(existing) = self.loaded.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return Ok(existing);
        }
        let loaded = (self.load)().await?;
        *self.loaded.lock().unwrap_or_else(|p| p.into_inner()) = Some(loaded.clone());
        Ok(loaded)
    }
}

#[async_trait]
impl OAuthAuth for LazyOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_subscription(&self) -> bool {
        self.is_subscription
    }

    fn login_label(&self) -> Option<&str> {
        self.login_label.as_deref()
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.loaded().await?.login(interaction).await
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        self.loaded().await?.refresh(credential, signal).await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        self.loaded().await?.to_auth(credential).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{AccountLoginReceipt, AuthContext, AuthEvent, AuthInteraction};
    use crate::utils::abort::AbortController;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FauxAuthContext {
        values: std::collections::HashMap<String, String>,
    }

    #[async_trait]
    impl AuthContext for FauxAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.values.get(name).cloned()
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    struct FauxInteraction {
        signal: AbortSignal,
        answer: String,
    }

    #[async_trait]
    impl AuthInteraction for FauxInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }
        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            Ok(self.answer.clone())
        }
        fn notify(&self, _event: AuthEvent) {}
    }

    #[tokio::test]
    async fn resolve_prefers_stored_credential_over_env() {
        let auth = env_api_key_auth("Anthropic API key", &["ANTHROPIC_API_KEY"]);
        let ctx = FauxAuthContext { values: [("ANTHROPIC_API_KEY".to_string(), "env-value".to_string())].into() };
        let signal = AbortController::new().signal();
        let stored = ApiKeyCredential { key: Some("stored".into()), env: None };
        let result = auth.resolve(&ctx, Some(&stored), &signal).await.unwrap().unwrap();
        assert_eq!(result.auth.api_key, Some("stored".into()));
        assert_eq!(result.source, Some("stored credential".into()));
    }

    #[tokio::test]
    async fn resolve_falls_back_to_first_set_env_var() {
        let auth = env_api_key_auth("Anthropic API key", &["MISSING_ONE", "ANTHROPIC_API_KEY"]);
        let ctx = FauxAuthContext { values: [("ANTHROPIC_API_KEY".to_string(), "env-value".to_string())].into() };
        let signal = AbortController::new().signal();
        let result = auth.resolve(&ctx, None, &signal).await.unwrap().unwrap();
        assert_eq!(result.auth.api_key, Some("env-value".into()));
        assert_eq!(result.source, Some("ANTHROPIC_API_KEY".into()));
    }

    #[tokio::test]
    async fn resolve_returns_none_when_nothing_configured() {
        let auth = env_api_key_auth("x", &["MISSING"]);
        let ctx = FauxAuthContext { values: Default::default() };
        let signal = AbortController::new().signal();
        assert!(auth.resolve(&ctx, None, &signal).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn login_prompts_for_secret_and_returns_api_key_credential() {
        let auth = env_api_key_auth("Anthropic API key", &[]);
        let signal = AbortController::new().signal();
        let interaction = ProviderAuthInteraction::new(
            signal,
            Arc::new(FauxInteraction { signal: AbortController::new().signal(), answer: "sk-typed".into() }),
        );
        let credential = auth.login(&interaction).await.unwrap();
        assert_eq!(credential.key, Some("sk-typed".into()));
    }

    #[tokio::test]
    async fn lazy_oauth_loads_once_and_delegates() {
        let load_count = Arc::new(AtomicUsize::new(0));
        let counter = load_count.clone();
        struct FauxOAuth;
        #[async_trait]
        impl OAuthAuth for FauxOAuth {
            fn name(&self) -> &str {
                "faux"
            }
            async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
                Ok(OAuthCredential::new("a", "r", 1.0))
            }
            async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
                Ok(credential.clone())
            }
            async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
                Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
            }
        }
        let lazy = lazy_oauth("Faux", true, Some("Sign in with Faux".into()), move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Arc::new(FauxOAuth) as Arc<dyn OAuthAuth>) })
        });
        let credential = OAuthCredential::new("access", "refresh", 1.0);
        let auth = lazy.to_auth(&credential).await.unwrap();
        assert_eq!(auth.api_key, Some("access".into()));
        let _ = lazy.to_auth(&credential).await.unwrap();
        assert_eq!(load_count.load(Ordering::SeqCst), 1);
        assert_eq!(lazy.name(), "Faux");
        assert!(lazy.is_subscription());
        assert_eq!(lazy.login_label(), Some("Sign in with Faux"));
    }
}
