//! Port of senpi packages/ai/src/auth/refresh-credential.ts.
//!
//! Effective credential for a provider's catalog refresh (`Models.refresh`). OAuth credentials are
//! refreshed before network access; the exchange is joined without owning it, so superseding the
//! catalog refresh stops waiting but never aborts a token exchange another lane may be relying on.

use crate::auth::oauth_refresh::{refresh_oauth_credential, OAuthRefreshRequest};
use crate::auth::resolve::{oauth_refresh_models_error, ModelsError};
use crate::auth::types::{ApiKeyCredential, AuthContext, Credential, CredentialStore, OAuthCredential, ProviderAuth};
use crate::utils::abort::AbortSignal;
use std::sync::Arc;

pub async fn resolve_refresh_credential(
    provider_id: &str,
    provider_auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    ctx: &dyn AuthContext,
    stored: Option<&Credential>,
    signal: &AbortSignal,
) -> Result<Option<Credential>, ModelsError> {
    if let Some(Credential::OAuth(stored)) = stored {
        let Some(oauth) = &provider_auth.oauth else { return Ok(None) };
        if (crate::utils::diagnostics::now_ms() as f64) < stored.expires {
            return Ok(Some(Credential::OAuth(stored.clone())));
        }
        if signal.aborted() {
            return Ok(None);
        }
        let post = refresh_oauth_credential(OAuthRefreshRequest {
            credentials: credentials.clone(),
            provider_id: provider_id.to_owned(),
            oauth: oauth.clone(),
            stale: stored.clone(),
            slot_name: None,
            is_stale: Arc::new(|credential: &OAuthCredential| crate::utils::diagnostics::now_ms() as f64 >= credential.expires),
            signal: signal.clone(),
            owning: false,
        })
        .await
        .map_err(|error| oauth_refresh_models_error(&error, provider_id))?;
        return Ok(post.filter(|post| matches!(post, Credential::OAuth(_))));
    }

    let Some(api_key) = &provider_auth.api_key else { return Ok(None) };
    let credential = stored.and_then(Credential::as_api_key);
    let result = api_key
        .resolve(ctx, credential, signal)
        .await
        .map_err(|error| ModelsError::new("auth", error.to_string()))?;
    Ok(result.map(|result| Credential::ApiKey(ApiKeyCredential { key: result.auth.api_key, env: result.env })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{ApiKeyAuth, AuthResult, ModelAuth, OAuthAuth, ProviderAuthInteraction};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FauxCtx {
        env: HashMap<String, String>,
    }
    #[async_trait]
    impl AuthContext for FauxCtx {
        async fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    struct FauxApiKeyAuth;
    #[async_trait]
    impl ApiKeyAuth for FauxApiKeyAuth {
        fn name(&self) -> &str {
            "faux"
        }
        async fn resolve(
            &self,
            ctx: &dyn AuthContext,
            _credential: Option<&ApiKeyCredential>,
            _signal: &AbortSignal,
        ) -> anyhow::Result<Option<AuthResult>> {
            Ok(ctx.env("KEY").await.map(|value| AuthResult {
                auth: ModelAuth { api_key: Some(value), ..Default::default() },
                env: None,
                source: Some("KEY".into()),
            }))
        }
    }

    struct FauxOAuthAuth {
        refresh_calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl OAuthAuth for FauxOAuthAuth {
        fn name(&self) -> &str {
            "faux-oauth"
        }
        async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            anyhow::bail!("login is not exercised")
        }
        async fn refresh(&self, _credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            Ok(OAuthCredential::new("refreshed-access", "refreshed-token", 999_999_999_999_999.0))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    fn signal() -> AbortSignal {
        crate::utils::abort::AbortController::new().signal()
    }

    async fn seeded(credential: Credential) -> Arc<dyn CredentialStore> {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        credentials.modify("p", Box::new(move |_| Box::pin(async move { Ok(Some(credential)) })), None).await.unwrap();
        credentials
    }

    #[tokio::test]
    async fn unexpired_oauth_credential_is_returned_without_refresh() {
        let stored = Credential::OAuth(OAuthCredential::new("still-valid", "r1", 999_999_999_999_999.0));
        let credentials = seeded(stored.clone()).await;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(FauxOAuthAuth { refresh_calls: refresh_calls.clone() })) };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_refresh_credential("p", &provider_auth, &credentials, &ctx, Some(&stored), &signal()).await.unwrap();
        assert_eq!(result, Some(stored));
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_oauth_credential_is_refreshed_before_the_catalog_fetch() {
        let stored = Credential::OAuth(OAuthCredential::new("expired", "r1", 1.0));
        let credentials = seeded(stored.clone()).await;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(FauxOAuthAuth { refresh_calls: refresh_calls.clone() })) };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_refresh_credential("p", &provider_auth, &credentials, &ctx, Some(&stored), &signal())
            .await
            .unwrap()
            .and_then(Credential::into_oauth)
            .expect("refreshed oauth credential");
        assert_eq!(result.access, "refreshed-access");
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ambient_api_key_resolves_into_a_credential() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: Some(Arc::new(FauxApiKeyAuth)), oauth: None };
        let ctx = FauxCtx { env: [("KEY".to_string(), "env-value".to_string())].into() };
        let result = resolve_refresh_credential("p", &provider_auth, &credentials, &ctx, None, &signal()).await.unwrap();
        assert_eq!(result, Some(Credential::ApiKey(ApiKeyCredential { key: Some("env-value".into()), env: None })));
    }

    #[tokio::test]
    async fn returns_none_when_no_credential_and_no_provider_auth_matches() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: None, oauth: None };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_refresh_credential("p", &provider_auth, &credentials, &ctx, None, &signal()).await.unwrap();
        assert_eq!(result, None);
    }
}
