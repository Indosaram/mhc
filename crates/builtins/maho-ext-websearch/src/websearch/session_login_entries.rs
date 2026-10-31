//! Port of senpi `websearch/websearch/session-login-entries.ts` (senpi 59d58cee59).
//!
//! websearch.json entries for `chatgpt-subscription` or `google` without an apiKey search with the
//! matching senpi login, sent to that login's own endpoint. Entries with no such login are dropped.

use super::hosted_routes::{HostedModelInfo, credential_headers, google_login_endpoint};
use super::native::{NativeAuth, NativeModelInfo, build_native_entry};
use super::provider_endpoints::{SearchProvider, is_allowed_provider_base_url};
use super::types::{SearchProviderEntry, WebsearchConfig};
use maho_tools::definition::{AbortSignal, ToolError};
use std::collections::BTreeMap;

/// Providers whose websearch.json entries may omit apiKey and use the matching senpi login.
pub const SESSION_LOGIN_PROVIDERS: [SearchProvider; 2] =
    [SearchProvider::ChatgptSubscription, SearchProvider::Google];

pub fn is_session_login_provider(provider: SearchProvider) -> bool {
    SESSION_LOGIN_PROVIDERS.contains(&provider)
}

fn needs_session_login(entry: &SearchProviderEntry) -> bool {
    is_session_login_provider(entry.config.provider)
        && entry.config.api_key.as_deref().is_none_or(str::is_empty)
}

/// A login candidate: the model plus its catalog headers.
#[derive(Clone, Debug)]
pub struct LoginModel {
    pub info: NativeModelInfo,
    pub headers: Option<BTreeMap<String, String>>,
}

/// The login credential the registry resolves for a model (`getApiKeyAndHeaders`).
#[derive(Clone, Debug, Default)]
pub struct LoginAuth {
    pub api_key: Option<String>,
    pub headers: Option<BTreeMap<String, Option<String>>>,
    pub base_url: Option<String>,
}

/// The session's active model followed by the available ones, same provider only; the entry's
/// requested model id goes first.
fn candidate_models(
    entry: &SearchProviderEntry,
    active: Option<&LoginModel>,
    available: &[LoginModel],
) -> Vec<LoginModel> {
    let provider = entry.config.provider.as_str();
    let models: Vec<LoginModel> = active
        .into_iter()
        .chain(available)
        .filter(|model| model.info.provider == provider)
        .cloned()
        .collect();
    let requested = entry.config.model.as_deref().map_or_else(Vec::new, |id| {
        models
            .iter()
            .filter(|model| model.info.id == id)
            .cloned()
            .collect()
    });
    requested.into_iter().chain(models).collect()
}

struct LoginCredential {
    api_key: String,
    base_url: String,
    model: String,
    headers: Option<BTreeMap<String, String>>,
}

async fn google_login<F, Fut>(model: &LoginModel, auth: &F) -> Option<LoginCredential>
where
    F: Fn(LoginModel) -> Fut,
    Fut: std::future::Future<Output = Option<LoginAuth>>,
{
    let hosted = HostedModelInfo {
        provider: &model.info.provider,
        id: &model.info.id,
        api: model.info.api.as_deref(),
    };
    let root = google_login_endpoint(&hosted, &model.info.base_url)?;
    let resolved = auth(model.clone()).await?;
    let api_key = resolved.api_key.filter(|key| !key.is_empty())?;
    let base_url = resolved
        .base_url
        .map_or(root, |url| url.trim_end_matches('/').to_owned());
    if !is_allowed_provider_base_url(&base_url) {
        return None;
    }
    Some(LoginCredential {
        api_key,
        base_url,
        model: model.info.id.clone(),
        headers: credential_headers(model.headers.as_ref(), resolved.headers.as_ref()),
    })
}

async fn subscription_login<F, Fut>(
    model: &LoginModel,
    auth: &F,
    id: &str,
    signal: Option<&AbortSignal>,
) -> Result<Option<LoginCredential>, ToolError>
where
    F: Fn(LoginModel) -> Fut,
    Fut: std::future::Future<Output = Option<LoginAuth>>,
{
    let catalog_headers = model.headers.clone();
    let native_auth = |info: NativeModelInfo| {
        let candidate = LoginModel {
            info,
            headers: catalog_headers.clone(),
        };
        let resolved = auth(candidate);
        let catalog_headers = catalog_headers.clone();
        async move {
            let resolved = resolved.await?;
            Some(NativeAuth {
                api_key: resolved.api_key?,
                headers: credential_headers(catalog_headers.as_ref(), resolved.headers.as_ref()),
                base_url: resolved.base_url,
            })
        }
    };
    let entry = build_native_entry(Some(&model.info), Some(&native_auth), Some(id), signal).await?;
    let Some(entry) =
        entry.filter(|entry| entry.config.provider == SearchProvider::ChatgptSubscription)
    else {
        return Ok(None);
    };
    let (Some(api_key), Some(base_url)) = (entry.config.api_key, entry.config.base_url) else {
        return Ok(None);
    };
    Ok(Some(LoginCredential {
        api_key,
        base_url,
        model: model.info.id.clone(),
        headers: entry.config.headers,
    }))
}

async fn resolve_session_login<F, Fut>(
    entry: &SearchProviderEntry,
    active: Option<&LoginModel>,
    available: &[LoginModel],
    auth: &F,
    signal: Option<&AbortSignal>,
) -> Result<Option<SearchProviderEntry>, ToolError>
where
    F: Fn(LoginModel) -> Fut,
    Fut: std::future::Future<Output = Option<LoginAuth>>,
{
    for candidate in candidate_models(entry, active, available) {
        if let Some(signal) = signal {
            signal.check()?;
        }
        let login = if entry.config.provider == SearchProvider::Google {
            google_login(&candidate, auth).await
        } else {
            let id = entry
                .config
                .id
                .clone()
                .unwrap_or_else(|| entry.config.provider.as_str().to_owned());
            subscription_login(&candidate, auth, &id, signal).await?
        };
        if let Some(signal) = signal {
            signal.check()?;
        }
        let Some(login) = login else { continue };
        let mut resolved = entry.clone();
        resolved.config.api_key = Some(login.api_key);
        resolved.config.base_url = Some(login.base_url);
        if login.headers.is_some() {
            resolved.config.headers = login.headers;
        }
        resolved.config.model = entry.config.model.clone().or(Some(login.model));
        return Ok(Some(resolved));
    }
    Ok(None)
}

/// senpi `resolveSessionLoginEntries`: entries that rely on a login take that login's key and
/// endpoint; with no context (`None` auth) or no matching login they are dropped.
pub async fn resolve_session_login_entries<F, Fut>(
    mut config: WebsearchConfig,
    active: Option<&LoginModel>,
    available: &[LoginModel],
    auth: Option<&F>,
    signal: Option<&AbortSignal>,
) -> Result<WebsearchConfig, ToolError>
where
    F: Fn(LoginModel) -> Fut,
    Fut: std::future::Future<Output = Option<LoginAuth>>,
{
    if !config.providers.iter().any(needs_session_login) {
        return Ok(config);
    }
    let mut providers = Vec::with_capacity(config.providers.len());
    for entry in std::mem::take(&mut config.providers) {
        if !needs_session_login(&entry) {
            providers.push(entry);
            continue;
        }
        let resolved = match auth {
            Some(auth) => resolve_session_login(&entry, active, available, auth, signal).await?,
            None => None,
        };
        providers.extend(resolved);
    }
    config.providers = providers;
    Ok(config)
}

fn login_model(model: &maho_ext_api::Model) -> LoginModel {
    LoginModel {
        info: super::native::model_info(model),
        headers: model.headers.clone(),
    }
}

/// [`resolve_session_login_entries`] over the session's model registry.
pub async fn config_with_session_logins(
    config: WebsearchConfig,
    model: Option<&maho_ext_api::Model>,
    registry: Option<&std::sync::Arc<dyn maho_ext_api::ModelRegistry>>,
    signal: Option<&AbortSignal>,
) -> Result<WebsearchConfig, ToolError> {
    if !config.providers.iter().any(needs_session_login) {
        return Ok(config);
    }
    let models: Vec<maho_ext_api::Model> = model
        .into_iter()
        .cloned()
        .chain(
            registry
                .map(|registry| registry.get_available())
                .unwrap_or_default(),
        )
        .collect();
    let active = model.map(login_model);
    let available: Vec<LoginModel> = registry
        .map(|registry| registry.get_available().iter().map(login_model).collect())
        .unwrap_or_default();
    let auth = |candidate: LoginModel| {
        let registry = registry.cloned();
        let model = models
            .iter()
            .find(|model| super::native::model_info(model) == candidate.info)
            .cloned();
        async move {
            let (registry, model) = (registry?, model?);
            let resolved = registry.get_api_key_and_headers(&model).await.ok()?;
            Some(LoginAuth {
                api_key: resolved.auth.api_key,
                headers: resolved.auth.headers,
                base_url: resolved.auth.base_url,
            })
        }
    };
    resolve_session_login_entries(
        config,
        active.as_ref(),
        &available,
        registry.map(|_| &auth),
        signal,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::websearch::types::{RoutingStrategy, SearchProviderConfig};

    fn entry(
        provider: SearchProvider,
        api_key: Option<&str>,
        model: Option<&str>,
    ) -> SearchProviderEntry {
        let mut config = SearchProviderConfig::new(provider);
        config.api_key = api_key.map(str::to_owned);
        config.model = model.map(str::to_owned);
        SearchProviderEntry {
            config,
            priority: None,
            weight: None,
            fallback_model: None,
        }
    }

    fn websearch(providers: Vec<SearchProviderEntry>) -> WebsearchConfig {
        WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto: false,
            native_model: None,
            providers,
        }
    }

    fn model(provider: &str, id: &str, api: &str, base_url: &str) -> LoginModel {
        LoginModel {
            info: NativeModelInfo {
                provider: provider.into(),
                id: id.into(),
                base_url: base_url.into(),
                api: Some(api.into()),
            },
            headers: Some(BTreeMap::from([("x-catalog".into(), "1".into())])),
        }
    }

    fn login(key: &str) -> impl Fn(LoginModel) -> std::future::Ready<Option<LoginAuth>> + '_ {
        move |_| {
            std::future::ready(Some(LoginAuth {
                api_key: Some(key.to_owned()),
                headers: Some(BTreeMap::from([("x-login".into(), Some("2".into()))])),
                base_url: None,
            }))
        }
    }

    #[tokio::test]
    async fn keyed_entries_pass_through_and_keyless_logins_without_context_drop() {
        let config = websearch(vec![
            entry(SearchProvider::Exa, Some("k"), None),
            entry(SearchProvider::Google, None, None),
            entry(SearchProvider::ChatgptSubscription, Some("own"), None),
        ]);
        let none: Option<&fn(LoginModel) -> std::future::Ready<Option<LoginAuth>>> = None;
        let resolved = resolve_session_login_entries(config, None, &[], none, None)
            .await
            .expect("resolve");
        assert_eq!(
            resolved
                .providers
                .iter()
                .map(|entry| entry.config.provider)
                .collect::<Vec<_>>(),
            [SearchProvider::Exa, SearchProvider::ChatgptSubscription]
        );
    }

    #[tokio::test]
    async fn google_entry_takes_the_google_login_endpoint_key_and_headers() {
        let gemini = model(
            "google",
            "gemini-flash-latest",
            "google-generative-ai",
            "https://generativelanguage.googleapis.com/v1beta/",
        );
        let imagen = model(
            "google",
            "gemini-2.0-flash-image",
            "google-generative-ai",
            "https://generativelanguage.googleapis.com/v1beta",
        );
        let config = websearch(vec![entry(SearchProvider::Google, None, None)]);
        let resolved = resolve_session_login_entries(
            config,
            Some(&imagen),
            &[gemini],
            Some(&login("g-key")),
            None,
        )
        .await
        .expect("resolve");
        let entry = &resolved.providers[0].config;
        assert_eq!(entry.api_key.as_deref(), Some("g-key"));
        assert_eq!(
            entry.base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta")
        );
        assert_eq!(entry.model.as_deref(), Some("gemini-flash-latest"));
        assert_eq!(
            entry.headers,
            Some(BTreeMap::from([
                ("x-catalog".into(), "1".into()),
                ("x-login".into(), "2".into())
            ]))
        );
    }

    #[tokio::test]
    async fn subscription_entry_uses_the_codex_route_and_keeps_its_requested_model() {
        let codex = model(
            "chatgpt-subscription",
            "gpt-5.5",
            "openai-codex-responses",
            "https://chatgpt.com/backend-api",
        );
        let mut wanted = model(
            "chatgpt-subscription",
            "gpt-5.6",
            "openai-codex-responses",
            "https://chatgpt.com/backend-api",
        );
        wanted.headers = None;
        let config = websearch(vec![entry(
            SearchProvider::ChatgptSubscription,
            None,
            Some("gpt-5.6"),
        )]);
        let resolved = resolve_session_login_entries(
            config,
            Some(&codex),
            &[wanted],
            Some(&login("c-key")),
            None,
        )
        .await
        .expect("resolve");
        let entry = &resolved.providers[0].config;
        assert_eq!(entry.api_key.as_deref(), Some("c-key"));
        assert_eq!(
            entry.base_url.as_deref(),
            Some("https://chatgpt.com/backend-api/codex/responses")
        );
        assert_eq!(entry.model.as_deref(), Some("gpt-5.6"));
        assert_eq!(entry.id.as_deref(), None);
    }

    #[tokio::test]
    async fn entry_without_a_matching_login_is_dropped() {
        let other = model(
            "openai",
            "gpt-5.5",
            "openai-responses",
            "https://api.openai.com/v1",
        );
        let config = websearch(vec![
            entry(SearchProvider::ChatgptSubscription, None, None),
            entry(SearchProvider::Exa, Some("k"), None),
        ]);
        let resolved =
            resolve_session_login_entries(config, Some(&other), &[], Some(&login("x")), None)
                .await
                .expect("resolve");
        assert_eq!(
            resolved
                .providers
                .iter()
                .map(|entry| entry.config.provider)
                .collect::<Vec<_>>(),
            [SearchProvider::Exa]
        );
    }

    #[test]
    fn session_login_providers_may_omit_api_key() {
        assert!(
            crate::websearch::config::validate_provider_config(&entry(
                SearchProvider::Google,
                None,
                None
            ))
            .is_ok()
        );
        assert!(
            crate::websearch::config::validate_provider_config(&entry(
                SearchProvider::ChatgptSubscription,
                None,
                None
            ))
            .is_ok()
        );
    }
}
