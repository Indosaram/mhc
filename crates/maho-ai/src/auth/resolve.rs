//! Port of senpi packages/ai/src/auth/resolve.ts.

use crate::auth::oauth_refresh::{refresh_oauth_credential, OAuthRefreshRequest, RefreshError};
use crate::auth::pool::slots::{project_slot, PooledCredential};
use crate::auth::types::{
    ApiKeyAuth, AuthContext, AuthResult, Credential, CredentialStore, ModelAuth, OAuthAuth, ProviderAuth,
};
use crate::types::ProviderEnv;
use crate::utils::abort::AbortSignal;
use std::sync::Arc;

pub type ModelsErrorCode = &'static str;

pub const PROVIDER_NOT_CONFIGURED_PREFIX: &str = "Provider is not configured: ";

pub fn provider_not_configured_message(provider_id: &str) -> String {
    format!("{PROVIDER_NOT_CONFIGURED_PREFIX}{provider_id}")
}

#[derive(Debug, Clone)]
pub struct AuthResolutionOverrides {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub headers: Option<crate::types::ProviderHeaders>,
}

/// The auth-layer public error, matching senpi's `ModelsError`: a stable `code` for programmatic
/// handling plus a human `message`. `cause_detail` carries a formatted diagnostic appended to
/// `message` when present.
#[derive(Debug, Clone)]
pub struct ModelsError {
    pub code: ModelsErrorCode,
    pub message: String,
}

impl std::fmt::Display for ModelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelsError {}

impl ModelsError {
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// Appends a formatted diagnostic detail for `cause` to `message`, matching senpi's
/// `withCauseDetail`.
pub fn with_cause_detail(message: &str, cause: Option<&str>) -> String {
    match cause {
        Some(cause) if !cause.is_empty() => format!("{message}: {cause}"),
        _ => message.to_string(),
    }
}

pub const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: f64 = 5.0 * 60.0 * 1_000.0;

pub fn oauth_refresh_models_error(error: &RefreshError, provider_id: &str) -> ModelsError {
    match error {
        RefreshError::Exchange(detail) => ModelsError::new(
            "oauth",
            with_cause_detail(&format!("OAuth refresh failed for {provider_id}"), Some(detail)),
        ),
        RefreshError::Store(detail) | RefreshError::Aborted(detail) => ModelsError::new(
            "auth",
            with_cause_detail(&format!("Credential store modify failed for {provider_id}"), Some(detail)),
        ),
    }
}

fn is_oauth_expiring(credential: &crate::auth::types::OAuthCredential, now: f64, minimum_validity_ms: f64) -> bool {
    credential.expires - now < minimum_validity_ms
}

/// Refreshes a stored OAuth credential when it (or its projected slot) is within
/// `minimum_validity_ms` of expiring, then re-derives request auth via `oauth.to_auth`.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_stored_oauth(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &Arc<dyn OAuthAuth>,
    stored: &Credential,
    slot_name: Option<&str>,
    now: f64,
    minimum_validity_ms: f64,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    let pooled: PooledCredential = stored.clone().into();
    let current = match slot_name {
        None => stored.as_oauth().cloned(),
        Some(name) => project_slot(Some(&pooled), name).and_then(|c| c.into_oauth()),
    };
    let Some(current) = current else { return Ok(None) };

    let credential = if is_oauth_expiring(&current, now, minimum_validity_ms) {
        let refreshed = refresh_oauth_credential(OAuthRefreshRequest {
            credentials: credentials.clone(),
            provider_id: provider_id.to_string(),
            oauth: oauth.clone(),
            stale: current.clone(),
            slot_name: slot_name.map(str::to_owned),
            is_stale: Arc::new(move |c| is_oauth_expiring(c, now, minimum_validity_ms)),
            signal: signal.clone(),
            owning: true,
        })
        .await
        .map_err(|e| oauth_refresh_models_error(&e, provider_id))?;
        match refreshed {
            Some(stored) => {
                let pooled: PooledCredential = stored.clone().into();
                match slot_name {
                    None => stored.as_oauth().cloned(),
                    Some(name) => project_slot(Some(&pooled), name).and_then(|c| c.into_oauth()),
                }
            }
            None => return Ok(None),
        }
    } else {
        Some(current)
    };
    let Some(credential) = credential else { return Ok(None) };

    let auth = oauth
        .to_auth(&credential)
        .await
        .map_err(|e| ModelsError::new("oauth", with_cause_detail(&format!("OAuth auth derivation failed for {provider_id}"), Some(&e.to_string()))))?;
    Ok(Some(AuthResult { auth, env: None, source: Some("OAuth".into()) }))
}

/// Ambient environment values applied over resolved auth, matching senpi's `overlayEnvAuthContext`:
/// stored/oauth-resolved env wins, ambient fills gaps.
pub fn overlay_env_auth_context(resolved: Option<ProviderEnv>, ambient: Option<&ProviderEnv>) -> Option<ProviderEnv> {
    match (resolved, ambient) {
        (Some(mut resolved), Some(ambient)) => {
            for (key, value) in ambient {
                resolved.entry(key.clone()).or_insert_with(|| value.clone());
            }
            Some(resolved)
        }
        (Some(resolved), None) => Some(resolved),
        (None, Some(ambient)) => Some(ambient.clone()),
        (None, None) => None,
    }
}

async fn read_credential(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    signal: &AbortSignal,
) -> Result<Option<Credential>, ModelsError> {
    credentials
        .read(provider_id, Some(crate::auth::types::AuthOperationOptions { signal: Some(signal.clone()) }))
        .await
        .map_err(|e| ModelsError::new("auth", with_cause_detail(&format!("Credential store read failed for {provider_id}"), Some(&e.to_string()))))
}

/// Cross-provider auth resolution choke point, matching senpi's `resolveProviderAuth`.
///
/// Order: an explicit `overrides.apiKey` always wins. Otherwise a stored credential is read; an
/// `oauth` credential resolves through `resolveStoredOAuth`, an `api_key` credential (or slot
/// projection) is handed to `apiKeyAuth.resolve` alongside ambient context, so a provider can
/// still fall back to environment variables when nothing is stored. `overrides.baseUrl`/`headers`
/// are applied last, layered onto whichever result was produced.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_provider_auth(
    provider_id: &str,
    provider_auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    ctx: &dyn AuthContext,
    slot_name: Option<&str>,
    overrides: Option<&AuthResolutionOverrides>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    resolve_provider_auth_with_signal(provider_id, provider_auth, credentials, ctx, slot_name, overrides, signal).await
}

#[allow(clippy::too_many_arguments)]
pub async fn resolve_provider_auth_with_signal(
    provider_id: &str,
    provider_auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    ctx: &dyn AuthContext,
    slot_name: Option<&str>,
    overrides: Option<&AuthResolutionOverrides>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    signal.throw_if_aborted().map_err(|reason| ModelsError::new("auth", reason.to_string()))?;

    if let Some(overrides) = overrides
        && let Some(api_key) = &overrides.api_key {
            let mut result = AuthResult {
                auth: ModelAuth { api_key: Some(api_key.clone()), ..Default::default() },
                env: None,
                source: Some("override".into()),
            };
            apply_overrides(&mut result, overrides);
            return Ok(Some(result));
        }

    let stored = read_credential(credentials, provider_id, signal).await?;
    let mut result = match &stored {
        Some(Credential::OAuth(_)) => {
            let Some(oauth) = &provider_auth.oauth else { return Ok(None) };
            let stored = stored.clone().expect("checked Some above");
            resolve_stored_oauth(
                credentials,
                provider_id,
                oauth,
                &stored,
                slot_name,
                crate::utils::diagnostics::now_ms() as f64,
                DEFAULT_OAUTH_MINIMUM_VALIDITY_MS,
                signal,
            )
            .await?
        }
        Some(Credential::ApiKey(api_key_credential)) => {
            let Some(api_key_auth) = &provider_auth.api_key else { return Ok(None) };
            let projected = match slot_name {
                None => Some(api_key_credential.clone()),
                Some(name) => {
                    let pooled: PooledCredential = stored.clone().expect("checked Some above").into();
                    project_slot(Some(&pooled), name).and_then(|c| c.as_api_key().cloned())
                }
            };
            api_key_auth
                .resolve(ctx, projected.as_ref(), signal)
                .await
                .map_err(|e| ModelsError::new("auth", with_cause_detail(&format!("API key auth failed for provider {provider_id}"), Some(&e.to_string()))))?
        }
        None => match &provider_auth.api_key {
            Some(api_key_auth) => api_key_auth
                .resolve(ctx, None, signal)
                .await
                .map_err(|e| ModelsError::new("auth", with_cause_detail(&format!("API key auth failed for provider {provider_id}"), Some(&e.to_string()))))?,
            None => None,
        },
    };

    if let Some(result) = &mut result
        && let Some(overrides) = overrides {
            apply_overrides(result, overrides);
        }
    Ok(result)
}

fn apply_overrides(result: &mut AuthResult, overrides: &AuthResolutionOverrides) {
    if let Some(base_url) = &overrides.base_url {
        result.auth.base_url = Some(base_url.clone());
    }
    if let Some(headers) = &overrides.headers {
        result.auth.headers = Some(headers.clone());
    }
}

/// Resolves an api-key credential ambiently, without a stored credential slot.
pub async fn resolve_api_key(
    api_key_auth: &Arc<dyn ApiKeyAuth>,
    ctx: &dyn AuthContext,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    api_key_auth.resolve(ctx, None, signal).await.map_err(|e| ModelsError::new("auth", e.to_string()))
}

/// Provider-scoped environment values from a stored credential (any type) plus ambient context.
pub fn credential_environment(credential: Option<&Credential>) -> Option<ProviderEnv> {
    match credential {
        Some(Credential::ApiKey(c)) => c.env.clone(),
        Some(Credential::OAuth(c)) => {
            c.extra.get("env").and_then(|v| v.as_object()).map(|obj| {
                let mut env = ProviderEnv::new();
                for (k, v) in obj {
                    if let Some(v) = v.as_str() {
                        env.insert(k.clone(), v.to_string());
                    }
                }
                env
            })
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{ApiKeyCredential, OAuthCredential, ProviderAuthInteraction};
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

    struct FauxApiKeyAuth {
        env_var: String,
    }
    #[async_trait]
    impl ApiKeyAuth for FauxApiKeyAuth {
        fn name(&self) -> &str {
            "faux"
        }
        async fn resolve(
            &self,
            ctx: &dyn AuthContext,
            credential: Option<&ApiKeyCredential>,
            _signal: &AbortSignal,
        ) -> anyhow::Result<Option<AuthResult>> {
            if let Some(credential) = credential
                && let Some(key) = &credential.key {
                    return Ok(Some(AuthResult {
                        auth: ModelAuth { api_key: Some(key.clone()), ..Default::default() },
                        env: credential.env.clone(),
                        source: Some("stored".into()),
                    }));
                }
            match ctx.env(&self.env_var).await {
                Some(value) => Ok(Some(AuthResult {
                    auth: ModelAuth { api_key: Some(value), ..Default::default() },
                    env: None,
                    source: Some(self.env_var.clone()),
                })),
                None => Ok(None),
            }
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
            Ok(OAuthCredential::new("refreshed-access", "refreshed-refresh", crate::utils::diagnostics::now_ms() as f64 + 3_600_000.0))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    #[test]
    fn provider_not_configured_message_matches_senpi_prefix() {
        assert_eq!(provider_not_configured_message("anthropic"), "Provider is not configured: anthropic");
    }

    #[test]
    fn with_cause_detail_appends_only_when_present() {
        assert_eq!(with_cause_detail("failed", Some("boom")), "failed: boom");
        assert_eq!(with_cause_detail("failed", None), "failed");
        assert_eq!(with_cause_detail("failed", Some("")), "failed");
    }

    #[tokio::test]
    async fn explicit_api_key_override_wins_over_everything() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: Some(Arc::new(FauxApiKeyAuth { env_var: "X".into() })), oauth: None };
        let ctx = FauxCtx { env: HashMap::new() };
        let overrides = AuthResolutionOverrides { api_key: Some("override-key".into()), base_url: None, headers: None };
        let result = resolve_provider_auth(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            Some(&overrides),
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("override-key".into()));
    }

    #[tokio::test]
    async fn falls_back_to_ambient_env_when_nothing_stored() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: Some(Arc::new(FauxApiKeyAuth { env_var: "MY_KEY".into() })), oauth: None };
        let ctx = FauxCtx { env: [("MY_KEY".to_string(), "env-key".to_string())].into() };
        let result = resolve_provider_auth(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            None,
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("env-key".into()));
    }

    #[tokio::test]
    async fn stored_oauth_within_validity_window_skips_refresh() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        credentials
            .modify(
                "p",
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new(
                            "access",
                            "refresh",
                            crate::utils::diagnostics::now_ms() as f64 + 3_600_000.0,
                        ))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(FauxOAuthAuth { refresh_calls: refresh_calls.clone() })) };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_provider_auth(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            None,
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("access".into()));
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stored_oauth_near_expiry_refreshes_before_resolving() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        credentials
            .modify(
                "p",
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new("stale-access", "stale-refresh", 1.0))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(FauxOAuthAuth { refresh_calls: refresh_calls.clone() })) };
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_provider_auth(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            None,
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("refreshed-access".into()));
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn overrides_apply_base_url_and_headers_on_top_of_resolved_result() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let provider_auth = ProviderAuth { api_key: Some(Arc::new(FauxApiKeyAuth { env_var: "MY_KEY".into() })), oauth: None };
        let ctx = FauxCtx { env: [("MY_KEY".to_string(), "env-key".to_string())].into() };
        let mut headers = crate::types::ProviderHeaders::new();
        headers.insert("x-custom".into(), Some("1".into()));
        let overrides = AuthResolutionOverrides { api_key: None, base_url: Some("https://example.test".into()), headers: Some(headers) };
        let result = resolve_provider_auth(
            "p",
            &provider_auth,
            &credentials,
            &ctx,
            None,
            Some(&overrides),
            &crate::utils::abort::AbortController::new().signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.base_url, Some("https://example.test".into()));
        assert!(result.auth.headers.is_some());
    }

    #[test]
    fn overlay_env_auth_context_fills_gaps_without_overwriting() {
        let mut resolved = ProviderEnv::new();
        resolved.insert("A".into(), "resolved".into());
        let mut ambient = ProviderEnv::new();
        ambient.insert("A".into(), "ambient".into());
        ambient.insert("B".into(), "ambient-b".into());
        let merged = overlay_env_auth_context(Some(resolved), Some(&ambient)).unwrap();
        assert_eq!(merged.get("A").map(String::as_str), Some("resolved"));
        assert_eq!(merged.get("B").map(String::as_str), Some("ambient-b"));
    }

    #[test]
    fn credential_environment_reads_api_key_env_and_oauth_extra_env() {
        let api_key_cred = Credential::ApiKey(ApiKeyCredential { key: Some("k".into()), env: Some({
            let mut env = ProviderEnv::new();
            env.insert("X".into(), "1".into());
            env
        }) });
        assert_eq!(credential_environment(Some(&api_key_cred)).unwrap().get("X").map(String::as_str), Some("1"));
        assert_eq!(credential_environment(None), None);
    }
}
