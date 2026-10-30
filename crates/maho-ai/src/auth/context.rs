//! Port of senpi packages/ai/src/auth/context.ts.

use crate::auth::types::{AuthContext, Credential, CredentialStore};
use crate::types::ProviderEnv;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

pub struct DefaultAuthContext;

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[async_trait]
impl AuthContext for DefaultAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        non_blank(process_env(name))
    }

    async fn file_exists(&self, path: &str) -> bool {
        let resolved = resolve_home(path);
        tokio::fs::metadata(&resolved).await.is_ok()
    }
}

fn resolve_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~')
        && let Some(home) = dirs::home_dir()
    {
        return PathBuf::from(format!("{}{}", home.display(), rest));
    }
    PathBuf::from(path)
}

pub fn default_provider_auth_context() -> DefaultAuthContext {
    DefaultAuthContext
}

/// The `StoredProviderEnvReader` senpi's model runtime hands ambient auth: a copy of another
/// provider's stored api-key credential `env`, never its key. A failed or empty read is `None`.
pub async fn read_stored_provider_env(
    credentials: &dyn CredentialStore,
    provider_id: &str,
) -> Option<ProviderEnv> {
    match credentials.read(provider_id, None).await {
        Ok(Some(Credential::ApiKey(credential))) => credential.env,
        _ => None,
    }
}

/// An auth context whose `stored_provider_env` reads `credentials`; env and file checks defer to
/// `base`. Ambient auth resolution receives it so routing metadata such as `GOOGLE_CLOUD_PROJECT`
/// can come from a sibling provider's stored credential.
pub struct StoredEnvAuthContext {
    base: Arc<dyn AuthContext>,
    credentials: Arc<dyn CredentialStore>,
}

impl StoredEnvAuthContext {
    pub fn new(base: Arc<dyn AuthContext>, credentials: Arc<dyn CredentialStore>) -> Self {
        Self { base, credentials }
    }
}

#[async_trait]
impl AuthContext for StoredEnvAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.base.env(name).await
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }

    async fn stored_provider_env(&self, provider_id: &str) -> Option<ProviderEnv> {
        read_stored_provider_env(self.credentials.as_ref(), provider_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{ApiKeyCredential, OAuthCredential};

    #[tokio::test]
    async fn stored_provider_env_reads_only_api_key_env_of_the_named_provider() {
        let store = Arc::new(InMemoryCredentialStore::new());
        let env: ProviderEnv = [(
            "GOOGLE_CLOUD_PROJECT".to_string(),
            "profile-project".to_string(),
        )]
        .into();
        let api_key = Credential::ApiKey(ApiKeyCredential {
            key: Some("secret-key".into()),
            env: Some(env.clone()),
            ..Default::default()
        });
        store
            .modify(
                "google-vertex",
                Box::new(move |_| Box::pin(async move { Ok(Some(api_key)) })),
                None,
            )
            .await
            .expect("seed api key");
        let oauth = Credential::OAuth(OAuthCredential::new("access", "refresh", 0.0));
        store
            .modify(
                "anthropic",
                Box::new(move |_| Box::pin(async move { Ok(Some(oauth)) })),
                None,
            )
            .await
            .expect("seed oauth");
        let ctx = StoredEnvAuthContext::new(Arc::new(DefaultAuthContext), store);
        assert_eq!(ctx.stored_provider_env("google-vertex").await, Some(env));
        assert_eq!(ctx.stored_provider_env("anthropic").await, None);
        assert_eq!(ctx.stored_provider_env("missing").await, None);
        assert_eq!(
            DefaultAuthContext
                .stored_provider_env("google-vertex")
                .await,
            None
        );
    }

    #[tokio::test]
    async fn env_returns_none_for_blank_or_unset() {
        assert_eq!(non_blank(Some("  ".into())), None);
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(non_blank(Some("value".into())), Some("value".into()));
        assert_eq!(non_blank(None), None);

        let ctx = DefaultAuthContext;
        assert_eq!(ctx.env("MAHO_AI_TEST_CONTEXT_ENV_MISSING").await, None);
        assert_eq!(ctx.env("PATH").await, std::env::var("PATH").ok());
    }

    #[tokio::test]
    async fn file_exists_resolves_home_and_reports_missing() {
        let ctx = DefaultAuthContext;
        let temp = tempfile::NamedTempFile::new().expect("tempfile");
        assert!(
            ctx.file_exists(temp.path().to_str().expect("utf8 path"))
                .await
        );
        assert!(!ctx.file_exists("/nonexistent/path/for/maho-ai-tests").await);
    }
}
