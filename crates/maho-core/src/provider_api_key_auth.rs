//! Request-time configuration key and header resolution.
use crate::model_config_schema::ModelsJsonProvider;
use crate::resolve_config_value::{
    get_config_value_env_var_names, is_command_config_value, resolve_config_value_or_throw,
    resolve_headers_or_throw,
};
use async_trait::async_trait;
use maho_ai::auth::headers::has_credential_headers;
use maho_ai::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthContext, AuthPrompt, AuthPromptKind, AuthResult,
    AuthType, ProviderAuthInteraction,
};
use maho_ai::models::ProviderAuthResult;
use maho_ai::types::{ProviderEnv, ProviderHeaders};
use maho_ai::utils::abort::AbortSignal;
use std::collections::HashMap;
use std::sync::Arc;

pub fn configured_api_key<'a>(
    config: Option<&'a ModelsJsonProvider>,
    extension: Option<&'a ModelsJsonProvider>,
) -> Option<&'a str> {
    extension
        .and_then(|c| c.api_key.as_deref())
        .or(config.and_then(|c| c.api_key.as_deref()))
}

pub fn configured_headers(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ModelsJsonProvider>,
) -> Option<HashMap<String, String>> {
    if config.and_then(|c| c.headers.as_ref()).is_none()
        && extension.and_then(|c| c.headers.as_ref()).is_none()
    {
        return None;
    }
    let mut headers: HashMap<String, String> = config
        .and_then(|c| c.headers.as_ref())
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    headers.extend(
        extension
            .and_then(|c| c.headers.as_ref())
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    Some(headers)
}

pub fn configured_request_auth_status(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ModelsJsonProvider>,
) -> Option<crate::provider_composer::AuthStatus> {
    use crate::provider_composer::AuthStatus;
    use crate::resolve_config_value::{
        get_config_value_env_var_names, is_command_config_value, is_config_value_configured,
    };
    let key = configured_api_key(config, extension);
    let mut values: Vec<(String, &str)> = Vec::new();
    if let Some(key) = key {
        values.push((
            if extension.and_then(|c| c.api_key.as_ref()).is_some() {
                "fallback"
            } else {
                "models_json_key"
            }
            .into(),
            key,
        ));
    }
    let headers = configured_headers(config, extension);
    if key.is_none() {
        for (name, value) in headers.as_ref().into_iter().flatten() {
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "x-api-key" | "api-key" | "cookie"
            ) {
                values.push((
                    if extension
                        .and_then(|c| c.headers.as_ref())
                        .is_some_and(|h| h.contains_key(name))
                    {
                        "extension_headers"
                    } else {
                        "models_json_headers"
                    }
                    .into(),
                    value,
                ));
            }
        }
    }
    if values.is_empty() {
        return None;
    }
    for (source, value) in values {
        if is_command_config_value(value) {
            return Some(AuthStatus {
                ambient: false,
                configured: true,
                source: Some("models_json_command".into()),
                label: None,
            });
        }
        let names = get_config_value_env_var_names(value);
        if !names.is_empty() {
            if is_config_value_configured(value, None) {
                return Some(AuthStatus {
                    ambient: false,
                    configured: true,
                    source: Some("environment".into()),
                    label: Some(names.join(", ")),
                });
            }
            continue;
        }
        if !value.trim().is_empty() {
            return Some(AuthStatus {
                ambient: false,
                configured: true,
                source: Some(source),
                label: None,
            });
        }
    }
    Some(AuthStatus::default())
}

pub fn with_configured_auth(
    mut auth: ProviderAuthResult,
    headers: Option<ProviderHeaders>,
    auth_header: bool,
) -> Result<ProviderAuthResult, String> {
    if let Some(headers) = headers {
        auth.headers
            .get_or_insert_with(Default::default)
            .extend(headers);
    }
    if auth_header {
        let key = auth
            .api_key
            .as_ref()
            .filter(|k| !k.is_empty())
            .ok_or("authHeader requires a resolved API key")?;
        auth.headers
            .get_or_insert_with(Default::default)
            .insert("Authorization".into(), Some(format!("Bearer {key}")));
    }
    Ok(auth)
}

pub async fn resolve_configured_auth(
    id: &str,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ModelsJsonProvider>,
    stored_key: Option<&str>,
    env: Option<&HashMap<String, String>>,
) -> Result<Option<ProviderAuthResult>, String> {
    // senpi providers/anthropic.ts: `ANTHROPIC_AUTH_TOKEN` resolves to an `Authorization: Bearer`
    // header (never an API key) ahead of `ANTHROPIC_OAUTH_TOKEN` / `ANTHROPIC_API_KEY`.
    let mut env_auth_token = None;
    let key = if let Some(key) = stored_key {
        Some(key.to_owned())
    } else if let Some(raw) = configured_api_key(config, extension) {
        Some(
            resolve_config_value_or_throw(raw, &format!("API key for provider \"{id}\""), env)
                .await?,
        )
    } else {
        let provider_env = env.map(|v| v.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        if id == "anthropic" {
            env_auth_token = maho_ai::utils::provider_env::get_provider_env_value(
                maho_ai::env_api_keys::ANTHROPIC_AUTH_TOKEN_ENV,
                provider_env.as_ref(),
            );
        }
        if env_auth_token.is_some() {
            None
        } else {
            maho_ai::env_api_keys::get_env_api_key(id, provider_env.as_ref())
                .filter(|key| key != maho_ai::models::AMBIENT_AUTH_MARKER)
        }
    };
    let raw_headers = configured_headers(config, extension);
    let headers =
        resolve_headers_or_throw(raw_headers.as_ref(), &format!("provider \"{id}\""), env)
            .await?
            .map(|v| {
                v.into_iter()
                    .map(|(k, v)| (k, Some(v)))
                    .collect::<ProviderHeaders>()
            });
    let credential_headers = headers.as_ref().is_some_and(|h| {
        h.keys().any(|k| {
            matches!(
                k.to_ascii_lowercase().as_str(),
                "authorization" | "x-api-key" | "api-key" | "cookie"
            )
        })
    });
    if key.is_none() && env_auth_token.is_none() && !credential_headers {
        return Ok(None);
    }
    let auth_header = extension
        .and_then(|c| c.auth_header)
        .or(config.and_then(|c| c.auth_header))
        .unwrap_or(false);
    let token_headers = env_auth_token.map(|token| {
        [("Authorization".to_owned(), Some(format!("Bearer {token}")))]
            .into_iter()
            .collect::<ProviderHeaders>()
    });
    with_configured_auth(
        ProviderAuthResult {
            api_key: key,
            headers: token_headers,
            ..Default::default()
        },
        headers,
        auth_header,
    )
    .map(Some)
}

/// `ModelAuth` <-> the collection's `ProviderAuthResult` (same three fields).
pub fn model_auth_to_result(auth: maho_ai::auth::types::ModelAuth) -> ProviderAuthResult {
    ProviderAuthResult {
        api_key: auth.api_key,
        headers: auth.headers,
        base_url: auth.base_url,
    }
}

pub fn result_to_model_auth(auth: ProviderAuthResult) -> maho_ai::auth::types::ModelAuth {
    maho_ai::auth::types::ModelAuth {
        api_key: auth.api_key,
        headers: auth.headers,
        base_url: auth.base_url,
    }
}

/// `configContextEnv`: explicit values first, then the auth context for every env var the config
/// values reference.
async fn config_context_env(
    values: &[&str],
    ctx: &dyn AuthContext,
    explicit: Option<&ProviderEnv>,
) -> Option<HashMap<String, String>> {
    let mut env: HashMap<String, String> = explicit
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut names: Vec<String> = Vec::new();
    for value in values {
        for name in get_config_value_env_var_names(value) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    for name in names {
        if env.contains_key(&name) {
            continue;
        }
        if let Some(value) = ctx.env(&name).await {
            env.insert(name, value);
        }
    }
    (!env.is_empty()).then_some(env)
}

/// `composeApiKeyAuth`: the provider's own api-key auth (`inherited`) layered under the models.json
/// / extension `apiKey`, `headers` and `authHeader` overlays.
pub struct ComposedApiKeyAuth {
    provider_id: String,
    inherited: Option<Arc<dyn ApiKeyAuth>>,
    raw_key: Option<String>,
    raw_headers: Option<HashMap<String, String>>,
    header_source: Option<String>,
    auth_header: bool,
}

/// An OAuth provider with no key, no credential headers and no inherited api-key auth gets the
/// ambient-only adapter (`ambientOnlyAuth`) when its OAuth declares `resolveAmbient`, else none.
pub fn compose_api_key_auth(
    provider_id: &str,
    inherited: Option<Arc<dyn ApiKeyAuth>>,
    oauth: Option<&Arc<dyn maho_ai::auth::types::OAuthAuth>>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ModelsJsonProvider>,
) -> Option<Arc<dyn ApiKeyAuth>> {
    let raw_key = configured_api_key(config, extension).map(str::to_owned);
    let header_source = crate::provider_header_auth::header_auth_resolution_source(
        config.and_then(|c| c.headers.as_ref()),
        extension.and_then(|c| c.headers.as_ref()),
    );
    let raw_headers = configured_headers(config, extension);
    let auth_header = extension
        .and_then(|c| c.auth_header)
        .or(config.and_then(|c| c.auth_header))
        .unwrap_or(false);
    if let Some(oauth) =
        oauth.filter(|_| inherited.is_none() && raw_key.is_none() && header_source.is_none())
    {
        if !oauth.has_resolve_ambient() {
            return None;
        }
        return Some(Arc::new(AmbientOnlyAuth {
            provider_id: provider_id.to_owned(),
            oauth: oauth.clone(),
            raw_headers,
            header_source,
            auth_header,
        }));
    }
    Some(Arc::new(ComposedApiKeyAuth {
        provider_id: provider_id.to_owned(),
        inherited,
        raw_key,
        raw_headers,
        header_source,
        auth_header,
    }))
}

/// senpi `ambientOnlyAuth`: api-key adapter for OAuth providers whose credentials live outside
/// auth.json; it never outranks a stored OAuth credential (`ambient_only`).
pub struct AmbientOnlyAuth {
    provider_id: String,
    oauth: Arc<dyn maho_ai::auth::types::OAuthAuth>,
    raw_headers: Option<HashMap<String, String>>,
    header_source: Option<String>,
    auth_header: bool,
}

impl AmbientOnlyAuth {
    async fn resolve_ambient(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        let Some(result) = self
            .oauth
            .resolve_ambient(ctx, credential.and_then(|c| c.env.as_ref()), signal)
            .await?
        else {
            return Ok(None);
        };
        // Auxiliary callers replay resolved auth as an explicit key; accept only this resolver's own marker.
        if let Some(key) = credential
            .and_then(|c| c.key.as_ref())
            .filter(|key| !key.is_empty())
            && result.auth.api_key.as_ref() != Some(key)
        {
            return Ok(None);
        }
        let mut explicit_env: ProviderEnv =
            credential.and_then(|c| c.env.clone()).unwrap_or_default();
        explicit_env.extend(result.env.clone().unwrap_or_default());
        let header_values: Vec<&str> = self
            .raw_headers
            .iter()
            .flat_map(|headers| headers.values().map(String::as_str))
            .collect();
        let header_env = config_context_env(&header_values, ctx, Some(&explicit_env)).await;
        let headers = resolve_headers_or_throw(
            self.raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            header_env.as_ref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!(error))?
        .map(|values| {
            values
                .into_iter()
                .map(|(k, v)| (k, Some(v)))
                .collect::<ProviderHeaders>()
        });
        let auth =
            with_configured_auth(model_auth_to_result(result.auth), headers, self.auth_header)
                .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(AuthResult {
            ambient: result.ambient,
            auth: result_to_model_auth(auth),
            env: result.env,
            source: result.source.or_else(|| self.header_source.clone()),
        }))
    }
}

#[async_trait]
impl ApiKeyAuth for AmbientOnlyAuth {
    fn name(&self) -> &str {
        self.oauth.name()
    }

    fn ambient_only(&self) -> bool {
        true
    }

    fn has_check(&self) -> bool {
        true
    }

    async fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        Ok(self
            .resolve_ambient(ctx, credential, signal)
            .await?
            .map(|resolved| AuthCheck {
                ambient: resolved.ambient,
                source: resolved.source,
                auth_type: AuthType::OAuth,
            }))
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        self.resolve_ambient(ctx, credential, signal).await
    }
}

impl ComposedApiKeyAuth {
    /// `resolveBaseAuth`.
    async fn resolve_base_auth(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        if let Some(credential) = credential
            && let Some(key) = credential.key.as_ref().filter(|key| !key.is_empty())
        {
            return match &self.inherited {
                Some(inherited) => inherited.resolve(ctx, Some(credential), signal).await,
                None => Ok(Some(AuthResult {
                    ambient: false,
                    auth: maho_ai::auth::types::ModelAuth {
                        api_key: Some(key.clone()),
                        ..Default::default()
                    },
                    env: credential.env.clone(),
                    source: Some("stored credential".into()),
                })),
            };
        }
        let Some(raw_key) = &self.raw_key else {
            return match &self.inherited {
                Some(inherited) => inherited.resolve(ctx, credential, signal).await,
                None => Ok(None),
            };
        };
        let env = config_context_env(
            &[raw_key.as_str()],
            ctx,
            credential.and_then(|c| c.env.as_ref()),
        )
        .await;
        let key = resolve_config_value_or_throw(
            raw_key,
            &format!("API key for provider \"{}\"", self.provider_id),
            env.as_ref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
        match &self.inherited {
            Some(inherited) => {
                let credential = ApiKeyCredential {
                    key: Some(key),
                    env: None,
                    ..Default::default()
                };
                inherited.resolve(ctx, Some(&credential), signal).await
            }
            None => Ok(Some(AuthResult {
                ambient: false,
                auth: maho_ai::auth::types::ModelAuth {
                    api_key: Some(key),
                    ..Default::default()
                },
                env: None,
                source: Some("configured API key".into()),
            })),
        }
    }
}

#[async_trait]
impl ApiKeyAuth for ComposedApiKeyAuth {
    fn key_optional(&self) -> bool {
        self.inherited.as_ref().is_some_and(|auth| auth.key_optional())
    }
    fn name(&self) -> &str {
        self.inherited
            .as_ref()
            .map_or("API key", |inherited| inherited.name())
    }

    fn has_login(&self) -> bool {
        self.inherited
            .as_ref()
            .is_some_and(|inherited| inherited.has_login())
            || !(self.header_source.is_some() && self.raw_key.is_none())
    }

    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<ApiKeyCredential> {
        if let Some(inherited) = self
            .inherited
            .as_ref()
            .filter(|inherited| inherited.has_login())
        {
            return inherited.login(interaction).await;
        }
        let key = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret {
                    message: "Enter API key".into(),
                    placeholder: None,
                },
                signal: None,
            })
            .await?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
            ..Default::default()
        })
    }

    fn has_check(&self) -> bool {
        true
    }

    async fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        if credential
            .and_then(|c| c.key.as_ref())
            .is_some_and(|key| !key.is_empty())
        {
            if let Some(inherited) = self
                .inherited
                .as_ref()
                .filter(|inherited| inherited.has_check())
                && let Some(check) = inherited.check(ctx, credential, signal).await?
            {
                return Ok(Some(check));
            }
            return Ok(Some(AuthCheck {
                ambient: false,
                source: Some("stored credential".into()),
                auth_type: AuthType::ApiKey,
            }));
        }
        if let Some(raw_key) = &self.raw_key {
            if !is_command_config_value(raw_key) {
                for name in get_config_value_env_var_names(raw_key) {
                    if ctx.env(&name).await.is_none() {
                        return Ok(None);
                    }
                }
            }
            return Ok(Some(AuthCheck {
                ambient: false,
                source: Some("configured API key".into()),
                auth_type: AuthType::ApiKey,
            }));
        }
        if let Some(inherited) = &self.inherited {
            if inherited.has_check()
                && let Some(check) = inherited.check(ctx, credential, signal).await?
            {
                return Ok(Some(check));
            }
            if let Some(resolved) = inherited.resolve(ctx, credential, signal).await? {
                return Ok(Some(AuthCheck {
                    ambient: resolved.ambient,
                    source: resolved.source,
                    auth_type: AuthType::ApiKey,
                }));
            }
        }
        let raw_headers: Option<std::collections::BTreeMap<String, String>> =
            self.raw_headers.as_ref().map(|headers| {
                headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            });
        Ok(crate::provider_header_auth::check_configured_header_auth(
            raw_headers.as_ref(),
            ctx,
            self.header_source.as_deref(),
        )
        .await)
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        let result = self.resolve_base_auth(ctx, credential, signal).await?;
        let mut explicit_env: ProviderEnv =
            credential.and_then(|c| c.env.clone()).unwrap_or_default();
        explicit_env.extend(
            result
                .as_ref()
                .and_then(|r| r.env.clone())
                .unwrap_or_default(),
        );
        let header_values: Vec<&str> = self
            .raw_headers
            .iter()
            .flat_map(|headers| headers.values().map(String::as_str))
            .collect();
        let header_env = config_context_env(&header_values, ctx, Some(&explicit_env)).await;
        let headers = resolve_headers_or_throw(
            self.raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            header_env.as_ref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!(error))?
        .map(|values| {
            values
                .into_iter()
                .map(|(k, v)| (k, Some(v)))
                .collect::<ProviderHeaders>()
        });
        if result.is_none() && !has_credential_headers(headers.as_ref()) {
            return Ok(None);
        }
        let (auth, env, source, ambient) = match result {
            Some(result) => (result.auth, result.env, result.source, result.ambient),
            None => (Default::default(), None, None, false),
        };
        let auth = with_configured_auth(model_auth_to_result(auth), headers, self.auth_header)
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(AuthResult {
            ambient,
            auth: result_to_model_auth(auth),
            env,
            source: source.or_else(|| self.header_source.clone()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stored_key_wins_over_unresolvable_configuration() {
        let config = ModelsJsonProvider {
            api_key: Some("${MISSING_KEY}".into()),
            auth_header: Some(true),
            ..Default::default()
        };
        let auth = resolve_configured_auth(
            "custom",
            Some(&config),
            None,
            Some("stored"),
            Some(&HashMap::new()),
        )
        .await
        .expect("resolve")
        .expect("auth");
        assert_eq!(auth.api_key.as_deref(), Some("stored"));
        assert_eq!(
            auth.headers.expect("headers")["Authorization"].as_deref(),
            Some("Bearer stored")
        );
    }
    #[tokio::test]
    async fn anthropic_auth_token_resolves_to_a_bearer_header() {
        let env = HashMap::from([
            ("ANTHROPIC_AUTH_TOKEN".to_owned(), "token".to_owned()),
            ("ANTHROPIC_API_KEY".to_owned(), "key".to_owned()),
        ]);
        let auth = resolve_configured_auth("anthropic", None, None, None, Some(&env))
            .await
            .expect("resolve")
            .expect("auth");
        assert_eq!(auth.api_key, None);
        assert_eq!(
            auth.headers.expect("headers")["Authorization"].as_deref(),
            Some("Bearer token")
        );
    }
    #[tokio::test]
    async fn ambient_marker_is_never_a_configured_key() {
        let env = HashMap::from([("AWS_PROFILE".to_owned(), "work".to_owned())]);
        assert!(
            resolve_configured_auth("amazon-bedrock", None, None, None, Some(&env))
                .await
                .expect("resolve")
                .is_none()
        );
    }
    #[test]
    fn bearer_auth_requires_a_key() {
        assert!(with_configured_auth(Default::default(), None, true).is_err());
    }

    struct Env(HashMap<String, String>);
    #[async_trait]
    impl AuthContext for Env {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn composed_auth_layers_configured_key_and_bearer_header_over_inherited_auth() {
        let inherited: Arc<dyn ApiKeyAuth> = Arc::new(maho_ai::auth::helpers::env_api_key_auth(
            "Fixture key",
            &["FIXTURE_KEY"],
        ));
        let config = ModelsJsonProvider {
            api_key: Some("configured".into()),
            auth_header: Some(true),
            ..Default::default()
        };
        let auth = compose_api_key_auth("custom", Some(inherited), None, Some(&config), None)
            .expect("composed");
        let signal = maho_ai::utils::abort::AbortController::new().signal();
        let result = auth
            .resolve(&Env(HashMap::new()), None, &signal)
            .await
            .expect("resolve")
            .expect("auth");
        assert_eq!(result.auth.api_key.as_deref(), Some("configured"));
        assert_eq!(
            result.auth.headers.expect("headers")["Authorization"].as_deref(),
            Some("Bearer configured")
        );
    }

    #[tokio::test]
    async fn composed_check_does_not_run_configured_commands() {
        let config = ModelsJsonProvider {
            api_key: Some("!exit 1".into()),
            ..Default::default()
        };
        let auth =
            compose_api_key_auth("custom", None, None, Some(&config), None).expect("composed");
        let signal = maho_ai::utils::abort::AbortController::new().signal();
        let check = auth
            .check(&Env(HashMap::new()), None, &signal)
            .await
            .expect("check")
            .expect("configured");
        assert_eq!(check.source.as_deref(), Some("configured API key"));
    }
}

#[cfg(test)]
mod optionality_tests {
    use super::*;
    struct OptionalAuth;
    #[async_trait]
    impl ApiKeyAuth for OptionalAuth {
        fn name(&self)->&str { "optional" }
        fn key_optional(&self)->bool { true }
        async fn resolve(&self,_:&dyn AuthContext,_:Option<&ApiKeyCredential>,_:&AbortSignal)->anyhow::Result<Option<AuthResult>> { Ok(None) }
    }
    #[test]
    fn inherits_optional_key_semantics() {
        let config=ModelsJsonProvider { base_url:Some("http://localhost".into()), ..Default::default() };
        let auth=compose_api_key_auth("optional",Some(Arc::new(OptionalAuth)),None,Some(&config),None).expect("composed"); assert!(auth.key_optional());
    }
}
