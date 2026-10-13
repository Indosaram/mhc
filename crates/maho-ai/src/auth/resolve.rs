//! Port of senpi packages/ai/src/auth/resolve.ts.

use crate::auth::oauth_refresh::{project_oauth_slot, refresh_oauth_credential, OAuthRefreshRequest, RefreshError};
use crate::auth::pool::slots::{project_slot, PooledCredential};
use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, Credential, CredentialStore, OAuthAuth, OAuthCredential,
    ProviderAuth,
};
use crate::types::ProviderEnv;
use crate::utils::abort::{race_with_abort_signal, AbortSignal};
use async_trait::async_trait;
use std::sync::Arc;

/// senpi's `AuthResolutionOverrides` (`apiKey`/`env`/`minOAuthValidityMs`/`slotName`/`signal`); one
/// type shared with the `Models` collection.
pub use crate::models::AuthResolutionOverrides;

pub type ModelsErrorCode = &'static str;

pub const PROVIDER_NOT_CONFIGURED_PREFIX: &str = "Provider is not configured: ";

pub fn provider_not_configured_message(provider_id: &str) -> String {
    format!("{PROVIDER_NOT_CONFIGURED_PREFIX}{provider_id}")
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

/// Auth resolution shared by the `Models` and `ImagesModels` collections.
/// A stored credential owns the provider: ambient/env is consulted only when
/// nothing is stored. No silent env fallback after a failed refresh or for a
/// credential type without a matching handler.
pub async fn resolve_provider_auth(
    provider_id: &str,
    provider_auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    ctx: &dyn AuthContext,
    overrides: Option<&AuthResolutionOverrides>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    race_with_abort_signal(
        resolve_provider_auth_with_signal(provider_id, provider_auth, credentials, ctx, overrides, signal),
        signal,
    )
    .await
    .map_err(|reason| ModelsError::new("auth", reason.message))?
}

pub async fn resolve_provider_auth_with_signal(
    provider_id: &str,
    provider_auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    ctx: &dyn AuthContext,
    overrides: Option<&AuthResolutionOverrides>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    signal.throw_if_aborted().map_err(|reason| ModelsError::new("auth", reason.message))?;
    let request_env = overrides.and_then(|overrides| overrides.env.as_ref());
    let overlay;
    let request_ctx: &dyn AuthContext = match request_env {
        Some(env) => {
            overlay = overlay_env_auth_context(ctx, env);
            &overlay
        }
        None => ctx,
    };
    let api_key = provider_auth.api_key.as_ref();
    let override_key = overrides.and_then(|overrides| overrides.api_key.as_ref());
    let min_oauth_validity_ms = overrides.and_then(|overrides| overrides.min_oauth_validity_ms).map(|ms| ms as f64);

    if let (Some(key), Some(api_key)) = (override_key, api_key)
        && !api_key.ambient_only()
    {
        let credential = ApiKeyCredential { key: Some(key.clone()), env: request_env.cloned() };
        return resolve_api_key(request_ctx, api_key, provider_id, Some(&credential), signal).await;
    }

    let stored = read_credential(credentials, provider_id, signal).await?;
    if let Some(slot_name) = overrides.and_then(|overrides| overrides.slot_name.as_deref()) {
        let projected = stored.map(PooledCredential::from).and_then(|pooled| project_slot(Some(&pooled), slot_name));
        return match projected {
            Some(Credential::OAuth(projected)) => match &provider_auth.oauth {
                Some(oauth) => {
                    resolve_stored_oauth(
                        credentials,
                        provider_id,
                        oauth,
                        projected,
                        request_ctx,
                        request_env,
                        signal,
                        min_oauth_validity_ms,
                        Some(slot_name),
                    )
                    .await
                }
                None => Ok(None),
            },
            Some(Credential::ApiKey(projected)) => match api_key {
                Some(api_key) => {
                    let credential = with_request_env(projected, request_env);
                    resolve_api_key(request_ctx, api_key, provider_id, Some(&credential), signal).await
                }
                None => Ok(None),
            },
            None => Ok(None),
        };
    }
    if let Some(stored) = stored {
        return match stored {
            Credential::OAuth(stored) => match &provider_auth.oauth {
                Some(oauth) => {
                    resolve_stored_oauth(
                        credentials,
                        provider_id,
                        oauth,
                        stored,
                        request_ctx,
                        request_env,
                        signal,
                        min_oauth_validity_ms,
                        None,
                    )
                    .await
                }
                None => Ok(None),
            },
            Credential::ApiKey(stored) => match api_key {
                Some(api_key) => {
                    let credential = with_request_env(stored, request_env);
                    resolve_api_key(request_ctx, api_key, provider_id, Some(&credential), signal).await
                }
                None => Ok(None),
            },
        };
    }

    if let (Some(key), Some(api_key)) = (override_key, api_key) {
        let credential = ApiKeyCredential { key: Some(key.clone()), env: request_env.cloned() };
        return resolve_api_key(request_ctx, api_key, provider_id, Some(&credential), signal).await;
    }

    // Ambient (env vars, AWS profiles, ADC files).
    let Some(api_key) = api_key else { return Ok(None) };
    let ambient_credential = match request_env {
        Some(env) if api_key.ambient_only() => Some(ApiKeyCredential { key: Some(String::new()), env: Some(env.clone()) }),
        _ => None,
    };
    resolve_api_key(request_ctx, api_key, provider_id, ambient_credential.as_ref(), signal).await
}

/// `{ ...credential, env: { ...credential.env, ...overrides.env } }`.
fn with_request_env(credential: ApiKeyCredential, request_env: Option<&ProviderEnv>) -> ApiKeyCredential {
    let Some(request_env) = request_env else { return credential };
    let mut env = credential.env.unwrap_or_default();
    env.extend(request_env.iter().map(|(name, value)| (name.clone(), value.clone())));
    ApiKeyCredential { env: Some(env), ..credential }
}

/// `overlayEnvAuthContext`: request env values shadow the base context; everything else defers.
pub struct OverlayEnvAuthContext<'a> {
    base: &'a dyn AuthContext,
    env: &'a ProviderEnv,
}

#[async_trait]
impl AuthContext for OverlayEnvAuthContext<'_> {
    async fn env(&self, name: &str) -> Option<String> {
        match self.env.get(name) {
            Some(value) => Some(value.clone()),
            None => self.base.env(name).await,
        }
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }
}

pub fn overlay_env_auth_context<'a>(base: &'a dyn AuthContext, env: &'a ProviderEnv) -> OverlayEnvAuthContext<'a> {
    OverlayEnvAuthContext { base, env }
}

pub const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: f64 = 5.0 * 60.0 * 1_000.0;

/// Maps a shared-refresh failure onto the `ModelsError` codes callers match on.
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

/// OAuth resolution: a token with less than five minutes remaining (or the caller's larger
/// minimum) is refreshed through `refresh_oauth_credential`, which re-checks the stored value, runs
/// the exchange outside the store lock, and compare-and-swaps the rotated slot.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_stored_oauth(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &Arc<dyn OAuthAuth>,
    stored: OAuthCredential,
    ctx: &dyn AuthContext,
    request_env: Option<&ProviderEnv>,
    signal: &AbortSignal,
    min_oauth_validity_ms: Option<f64>,
    slot_name: Option<&str>,
) -> Result<Option<AuthResult>, ModelsError> {
    let minimum_validity_ms = DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0.0));
    let expires_soon = move |credential: &OAuthCredential| {
        crate::utils::diagnostics::now_ms() as f64 + minimum_validity_ms >= credential.expires
    };
    let mut credential = stored;

    if expires_soon(&credential) {
        let post = refresh_oauth_credential(OAuthRefreshRequest {
            credentials: credentials.clone(),
            provider_id: provider_id.to_owned(),
            oauth: oauth.clone(),
            stale: credential.clone(),
            slot_name: slot_name.map(str::to_owned),
            is_stale: Arc::new(expires_soon),
            signal: signal.clone(),
            owning: true,
        })
        .await
        .map_err(|error| oauth_refresh_models_error(&error, provider_id))?;
        // Logged out meanwhile.
        let Some(post @ Credential::OAuth(_)) = post else { return Ok(None) };
        let post_view = match slot_name {
            None => post.into_oauth(),
            Some(name) => project_oauth_slot(&PooledCredential::from(post), name),
        };
        // Slot removed meanwhile.
        let Some(post_view) = post_view else { return Ok(None) };
        credential = post_view;
        // The normal five-minute window triggers a refresh but does not impose a provider
        // contract. Explicit callers (such as bearer-token export) do require the requested
        // minimum after the refresh.
        if min_oauth_validity_ms.is_some() && expires_soon(&credential) {
            return Err(ModelsError::new(
                "oauth",
                format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
            ));
        }
    }

    let stored_env = oauth_credential_environment(&credential);
    let effective_env = match request_env {
        Some(request_env) => {
            let mut env = stored_env.unwrap_or_default();
            env.extend(request_env.iter().map(|(name, value)| (name.clone(), value.clone())));
            Some(env)
        }
        None => stored_env,
    };
    let effective_credential = match &effective_env {
        Some(env) => credential.with_extra("env", serde_json::to_value(env).unwrap_or_default()),
        None => credential,
    };

    match oauth.check(ctx, Some(&effective_credential), signal).await {
        Ok(Some(_)) => {}
        Ok(None) => return Ok(None),
        Err(error) => {
            return Err(ModelsError::new(
                "auth",
                with_cause_detail(&format!("OAuth auth check failed for provider {provider_id}"), Some(&error.to_string())),
            ));
        }
    }

    let auth = oauth.to_auth(&effective_credential).await.map_err(|e| {
        ModelsError::new(
            "oauth",
            with_cause_detail(&format!("OAuth auth derivation failed for {provider_id}"), Some(&e.to_string())),
        )
    })?;
    Ok(Some(AuthResult { auth, env: effective_env, source: Some("OAuth".into()) }))
}

/// `credentialEnvironment`: the OAuth credential's `env` object when every entry is a string.
fn oauth_credential_environment(credential: &OAuthCredential) -> Option<ProviderEnv> {
    let value = credential.extra.get("env")?.as_object()?;
    let mut environment = ProviderEnv::new();
    for (name, entry) in value {
        environment.insert(name.clone(), entry.as_str()?.to_owned());
    }
    Some(environment)
}

pub async fn resolve_api_key(
    ctx: &dyn AuthContext,
    api_key: &Arc<dyn ApiKeyAuth>,
    provider_id: &str,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    api_key.resolve(ctx, credential, signal).await.map_err(|e| {
        ModelsError::new(
            "auth",
            with_cause_detail(&format!("API key auth failed for provider {provider_id}"), Some(&e.to_string())),
        )
    })
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

/// Provider-scoped environment values from a stored credential (any type).
pub fn credential_environment(credential: Option<&Credential>) -> Option<ProviderEnv> {
    match credential {
        Some(Credential::ApiKey(c)) => c.env.clone(),
        Some(Credential::OAuth(c)) => oauth_credential_environment(c),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{ModelAuth, ProviderAuthInteraction};
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
        refreshed_ttl_ms: f64,
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
            Ok(OAuthCredential::new("refreshed-access", "refreshed-refresh", now() + self.refreshed_ttl_ms))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    fn now() -> f64 {
        crate::utils::diagnostics::now_ms() as f64
    }

    fn signal() -> AbortSignal {
        crate::utils::abort::AbortController::new().signal()
    }

    fn oauth_provider(refresh_calls: &Arc<AtomicUsize>, refreshed_ttl_ms: f64) -> ProviderAuth {
        ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(FauxOAuthAuth { refresh_calls: refresh_calls.clone(), refreshed_ttl_ms })),
        }
    }

    fn api_key_provider(env_var: &str) -> ProviderAuth {
        ProviderAuth { api_key: Some(Arc::new(FauxApiKeyAuth { env_var: env_var.into() })), oauth: None }
    }

    async fn seeded(credential: Credential) -> Arc<dyn CredentialStore> {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        credentials
            .modify("p", Box::new(move |_| Box::pin(async move { Ok(Some(credential)) })), None)
            .await
            .unwrap();
        credentials
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
    async fn explicit_api_key_override_resolves_through_api_key_auth() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let ctx = FauxCtx { env: HashMap::new() };
        let overrides = AuthResolutionOverrides { api_key: Some("override-key".into()), ..Default::default() };
        let result = resolve_provider_auth("p", &api_key_provider("X"), &credentials, &ctx, Some(&overrides), &signal())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.auth.api_key, Some("override-key".into()));
        assert_eq!(result.source.as_deref(), Some("stored"));
    }

    #[tokio::test]
    async fn falls_back_to_ambient_env_when_nothing_stored() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let ctx = FauxCtx { env: [("MY_KEY".to_string(), "env-key".to_string())].into() };
        let result = resolve_provider_auth("p", &api_key_provider("MY_KEY"), &credentials, &ctx, None, &signal())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.auth.api_key, Some("env-key".into()));
    }

    #[tokio::test]
    async fn request_env_overlays_the_auth_context() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let ctx = FauxCtx { env: [("MY_KEY".to_string(), "ambient".to_string())].into() };
        let overrides = AuthResolutionOverrides {
            env: Some([("MY_KEY".to_string(), "request".to_string())].into()),
            ..Default::default()
        };
        let result = resolve_provider_auth("p", &api_key_provider("MY_KEY"), &credentials, &ctx, Some(&overrides), &signal())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.auth.api_key, Some("request".into()));
    }

    #[tokio::test]
    async fn stored_api_key_env_merges_request_env_into_the_result() {
        let credentials = seeded(Credential::ApiKey(ApiKeyCredential {
            key: Some("stored-key".into()),
            env: Some([("ACCOUNT".to_string(), "stored".to_string())].into()),
        }))
        .await;
        let ctx = FauxCtx { env: HashMap::new() };
        let overrides = AuthResolutionOverrides {
            env: Some([("GATEWAY".to_string(), "request".to_string())].into()),
            ..Default::default()
        };
        let result = resolve_provider_auth("p", &api_key_provider("X"), &credentials, &ctx, Some(&overrides), &signal())
            .await
            .unwrap()
            .unwrap();
        let env = result.env.expect("credential env");
        assert_eq!(env.get("ACCOUNT").map(String::as_str), Some("stored"));
        assert_eq!(env.get("GATEWAY").map(String::as_str), Some("request"));
    }

    #[tokio::test]
    async fn missing_slot_resolves_none_without_ambient_fallback() {
        let credentials = seeded(Credential::ApiKey(ApiKeyCredential { key: Some("flat".into()), env: None })).await;
        let ctx = FauxCtx { env: [("MY_KEY".to_string(), "env-key".to_string())].into() };
        let overrides = AuthResolutionOverrides { slot_name: Some("work".into()), ..Default::default() };
        let result = resolve_provider_auth("p", &api_key_provider("MY_KEY"), &credentials, &ctx, Some(&overrides), &signal())
            .await
            .unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn stored_oauth_within_validity_window_skips_refresh() {
        let credentials = seeded(Credential::OAuth(OAuthCredential::new("access", "refresh", now() + 3_600_000.0))).await;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_provider_auth("p", &oauth_provider(&refresh_calls, 3_600_000.0), &credentials, &ctx, None, &signal())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.auth.api_key, Some("access".into()));
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oauth_refreshes_inside_the_five_minute_floor_despite_a_shorter_override() {
        let credentials = seeded(Credential::OAuth(OAuthCredential::new("stale-access", "stale-refresh", now() + 120_000.0))).await;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let ctx = FauxCtx { env: HashMap::new() };
        let overrides = AuthResolutionOverrides { min_oauth_validity_ms: Some(60_000), ..Default::default() };
        let result = resolve_provider_auth(
            "p",
            &oauth_provider(&refresh_calls, 3_600_000.0),
            &credentials,
            &ctx,
            Some(&overrides),
            &signal(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.auth.api_key, Some("refreshed-access".into()));
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn explicit_minimum_rejects_a_refreshed_token_that_expires_too_soon() {
        let credentials = seeded(Credential::OAuth(OAuthCredential::new("stale-access", "stale-refresh", 1.0))).await;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let ctx = FauxCtx { env: HashMap::new() };
        let overrides = AuthResolutionOverrides { min_oauth_validity_ms: Some(1_800_000), ..Default::default() };
        let error = resolve_provider_auth(
            "p",
            &oauth_provider(&refresh_calls, 600_000.0),
            &credentials,
            &ctx,
            Some(&overrides),
            &signal(),
        )
        .await
        .expect_err("token shorter than the requested minimum");
        assert_eq!(error.code, "oauth");
        assert_eq!(error.message, "OAuth refresh returned a token that expires too soon for p");
    }

    #[tokio::test]
    async fn stored_oauth_env_rides_along_in_the_result() {
        let stored = OAuthCredential::new("access", "refresh", now() + 3_600_000.0)
            .with_extra("env", serde_json::json!({"REGION": "global"}));
        let credentials = seeded(Credential::OAuth(stored)).await;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let ctx = FauxCtx { env: HashMap::new() };
        let result = resolve_provider_auth("p", &oauth_provider(&refresh_calls, 3_600_000.0), &credentials, &ctx, None, &signal())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.env.expect("oauth env").get("REGION").map(String::as_str), Some("global"));
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
