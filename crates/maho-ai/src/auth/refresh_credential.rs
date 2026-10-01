//! Port of senpi packages/ai/src/auth/refresh-credential.ts.

use crate::auth::oauth_refresh::{refresh_oauth_credential, OAuthRefreshRequest};
use crate::auth::pool::slots::{project_slot, PooledCredential};
use crate::auth::resolve::{oauth_refresh_models_error, resolve_api_key, ModelsError};
use crate::auth::types::{AuthContext, AuthResult, Credential, CredentialStore, ProviderAuth};
use crate::utils::abort::AbortSignal;
use std::sync::Arc;

/// Force-refreshes the credential backing `Models.refresh`: an OAuth credential (or slot) is
/// unconditionally exchanged regardless of expiry, an api-key credential resolves ambiently
/// through `apiKeyAuth.resolve` (no forced refresh exists for api keys).
pub async fn resolve_refresh_credential(
    provider_id: &str,
    provider_auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    ctx: &dyn AuthContext,
    slot_name: Option<&str>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    signal.throw_if_aborted().map_err(|reason| ModelsError::new("auth", reason.to_string()))?;

    let stored = credentials
        .read(provider_id, Some(crate::auth::types::AuthOperationOptions { signal: Some(signal.clone()) }))
        .await
        .map_err(|e| ModelsError::new("auth", e.to_string()))?;

    match &stored {
        Some(Credential::OAuth(_)) => {
            let Some(oauth) = &provider_auth.oauth else { return Ok(None) };
            let stored_credential = stored.clone().expect("checked Some above");
            let pooled: PooledCredential = stored_credential.clone().into();
            let current = match slot_name {
                None => stored_credential.as_oauth().cloned(),
                Some(name) => project_slot(Some(&pooled), name).and_then(|c| c.into_oauth()),
            };
            let Some(current) = current else { return Ok(None) };

            let refreshed = refresh_oauth_credential(OAuthRefreshRequest {
                credentials: credentials.clone(),
                provider_id: provider_id.to_string(),
                oauth: oauth.clone(),
                stale: current,
                slot_name: slot_name.map(str::to_owned),
                is_stale: Arc::new(|_| true),
                signal: signal.clone(),
                owning: true,
            })
            .await
            .map_err(|e| oauth_refresh_models_error(&e, provider_id))?;

            let Some(refreshed) = refreshed else { return Ok(None) };
            let pooled: PooledCredential = refreshed.clone().into();
            let credential = match slot_name {
                None => refreshed.as_oauth().cloned(),
                Some(name) => project_slot(Some(&pooled), name).and_then(|c| c.into_oauth()),
            };
            let Some(credential) = credential else { return Ok(None) };
            let auth = oauth.to_auth(&credential).await.map_err(|e| ModelsError::new("oauth", e.to_string()))?;
            Ok(Some(AuthResult { auth, env: None, source: Some("OAuth".into()) }))
        }
        Some(Credential::ApiKey(_)) | None => match &provider_auth.api_key {
            Some(api_key_auth) => resolve_api_key(api_key_auth, ctx, signal).await,
            None => Ok(None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{ApiKeyAuth, ApiKeyCredential, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
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
            unimplemented!()
        }
        async fn refresh(&self, _credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            Ok(OAuthCredential::new("forced-refresh-access", "forced-refresh-token", 999_999_999_999.0))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    #[tokio::test]
    async fn oauth_credential_is_unconditionally_refreshed_even_when_valid() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        credentials
            .modify(
                "p",
                Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("still-valid", "r1", 999_999_999_999.0)))) })),
                None,
            )
            .await
            .unwrap();
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(FauxOAuthAuth { refresh_calls: refresh_calls.clone() })) };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_refresh_credential(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("forced-refresh-access".into()));
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1, "refresh must run unconditionally regardless of expiry");
    }

    #[tokio::test]
    async fn api_key_credential_resolves_ambiently_without_forced_refresh() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: Some(Arc::new(FauxApiKeyAuth)), oauth: None };
        let ctx = FauxCtx { env: [("KEY".to_string(), "env-value".to_string())].into() };
        let result = resolve_refresh_credential(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("env-value".into()));
    }

    #[tokio::test]
    async fn returns_none_when_no_credential_and_no_provider_auth_matches() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: None, oauth: None };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_refresh_credential(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap();
        assert_eq!(result, None);
    }
}
