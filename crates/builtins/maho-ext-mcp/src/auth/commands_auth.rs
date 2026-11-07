use super::{
    oauth::{
        OAuthRequestError, begin_authorization, begin_authorization_forced,
        client_credentials_grant, complete_authorization, logout,
    },
    oauth_errors::{OAuthFailureKind, OAuthFlowError},
    oauth_provider::McpOAuthProvider,
    token_store::McpTokenStore,
};
use crate::config_schema::{Auth, McpServerConfig, Transport};
use std::{collections::BTreeMap, path::Path};
pub fn build_provider(
    name: &str,
    config: &McpServerConfig,
    agent_dir: &Path,
    callback_url: Option<&str>,
) -> Result<McpOAuthProvider, OAuthRequestError> {
    if config.transport != Some(Transport::Http)
        || config.auth == Some(Auth::Disabled(false))
        || config.url.is_none()
    {
        return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(
            OAuthFailureKind::Headless,
            format!("MCP server {name} is not an OAuth HTTP server."),
        ))));
    }
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(
        agent_dir,
        name,
        config.url.as_deref().unwrap_or(""),
    ));
    provider.redirect_url = Some(callback_url.unwrap_or("http://127.0.0.1:0/callback").into());
    if let Some(oauth) = &config.oauth {
        provider.client_id = oauth.client_id.clone();
        provider.client_metadata_url = oauth.client_metadata_url.clone();
        provider.scopes = oauth.scopes.clone();
    }
    Ok(provider)
}
pub async fn run_auth_start(
    name: &str,
    mut provider: McpOAuthProvider,
    pending: &mut BTreeMap<String, McpOAuthProvider>,
    client: &reqwest::Client,
) -> Result<String, OAuthRequestError> {
    let begin = begin_authorization(&mut provider, client).await?;
    let url = begin.authorization_url.ok_or_else(|| {
        OAuthRequestError::Flow(Box::new(OAuthFlowError::new(
            OAuthFailureKind::NeedsAuth,
            format!("MCP server {name} did not produce an authorization URL."),
        )))
    })?;
    pending.insert(name.into(), provider);
    Ok(url.to_string())
}
pub async fn run_auth_complete(
    name: &str,
    redirect: &str,
    pending: &mut BTreeMap<String, McpOAuthProvider>,
    client: &reqwest::Client,
) -> Result<(), OAuthRequestError> {
    let provider = pending.get_mut(name).ok_or_else(|| {
        OAuthRequestError::Flow(Box::new(OAuthFlowError::new(
            OAuthFailureKind::StateMismatch,
            format!(
                "MCP server {name} authorization state did not match; restart with /mcp auth-start."
            ),
        )))
    })?;
    complete_authorization(provider, redirect, client).await?;
    pending.remove(name);
    Ok(())
}
pub async fn run_client_credentials_auth(
    provider: &McpOAuthProvider,
    client: &reqwest::Client,
) -> Result<(), OAuthRequestError> {
    client_credentials_grant(provider, client).await
}
pub async fn run_loopback_auth<F, Fut>(
    provider: McpOAuthProvider,
    port: Option<u16>,
    client: &reqwest::Client,
    on_authorization: F,
) -> Result<(), OAuthRequestError>
where
    F: FnOnce(url::Url) -> Fut,
    Fut: std::future::Future<Output = Result<(), OAuthRequestError>>,
{
    loopback_auth(provider, port, client, on_authorization, false).await
}
/// Like `run_loopback_auth` but runs the flow even when a fresh token is already stored.
pub async fn run_loopback_auth_forced<F, Fut>(
    provider: McpOAuthProvider,
    port: Option<u16>,
    client: &reqwest::Client,
    on_authorization: F,
) -> Result<(), OAuthRequestError>
where
    F: FnOnce(url::Url) -> Fut,
    Fut: std::future::Future<Output = Result<(), OAuthRequestError>>,
{
    loopback_auth(provider, port, client, on_authorization, true).await
}
async fn loopback_auth<F, Fut>(
    mut provider: McpOAuthProvider,
    port: Option<u16>,
    client: &reqwest::Client,
    on_authorization: F,
    force: bool,
) -> Result<(), OAuthRequestError>
where
    F: FnOnce(url::Url) -> Fut,
    Fut: std::future::Future<Output = Result<(), OAuthRequestError>>,
{
    let shared = std::sync::Arc::new(std::sync::Mutex::new(None::<McpOAuthProvider>));
    let state = shared.clone();
    let mut channel = super::callback::open_callback_channel(
        super::callback::CallbackServerOptions {
            server_name: provider.store.server_name.clone(),
            port,
            host: None,
            path: None,
            timeout: None,
            validate_state: std::sync::Arc::new(move |candidate| {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_mut()
                    .is_some_and(|provider| provider.consume_state(candidate))
            }),
        },
        None,
    )
    .await
    .map_err(|error| OAuthRequestError::Flow(Box::new(error)))?;
    provider.redirect_url = Some(channel.redirect_url.clone());
    let begin = if force {
        begin_authorization_forced(&mut provider, client).await?
    } else {
        begin_authorization(&mut provider, client).await?
    };
    if begin.authorized {
        channel.close().await;
        return Ok(());
    }
    let authorization = begin.authorization_url.ok_or_else(|| {
        OAuthRequestError::Invalid("OAuth flow produced no authorization URL".into())
    })?;
    *shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(provider);
    on_authorization(authorization).await?;
    let callback = channel
        .wait_for_code()
        .await
        .map_err(|error| OAuthRequestError::Flow(Box::new(error)))?;
    let provider = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("loopback provider initialized before callback");
    super::oauth::finish_authorization(&provider, &callback.code, callback.iss.as_deref(), client)
        .await
}
pub fn run_logout(
    name: &str,
    provider: &McpOAuthProvider,
    pending: &mut BTreeMap<String, McpOAuthProvider>,
) -> Result<(), OAuthRequestError> {
    logout(provider)?;
    pending.remove(name);
    Ok(())
}

/// Upstream `Notify`: a message and its kind.
pub type McpAuthNotify = std::sync::Arc<dyn Fn(&str, maho_ext_api::NotificationType) + Send + Sync>;
/// Upstream `onReconnect`: reconnect the server and reattach the session; failures are expected and ignored.
pub type McpAuthReconnect = std::sync::Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;
/// Upstream `interactiveGuard`: the same server may authenticate in another session.
#[derive(Clone)]
pub struct McpInteractiveAuthGuard {
    pub begin: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub end: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
}
/// Upstream `AuthCommandDeps`.
#[derive(Clone)]
pub struct AuthCommandDeps {
    pub server_name: String,
    pub config: McpServerConfig,
    pub agent_dir: std::path::PathBuf,
    pub has_ui: bool,
    pub notify: McpAuthNotify,
    /// settings.oauthCallbackUrl override: when set, no local listener is opened.
    pub callback_url: Option<String>,
    pub open_browser: Option<super::challenge_login::BrowserOpener>,
    pub on_reconnect: McpAuthReconnect,
    /// Persists the provider between auth-start and auth-complete so the in-memory CSRF state survives.
    pub pending: std::sync::Arc<std::sync::Mutex<BTreeMap<String, McpOAuthProvider>>>,
    pub interactive_guard: Option<McpInteractiveAuthGuard>,
    pub client: reqwest::Client,
}
const PASTE_REDIRECT: &str = "http://127.0.0.1:0/callback";
fn active_interactive_auth_servers() -> &'static std::sync::Mutex<std::collections::BTreeSet<String>>
{
    static ACTIVE: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}
fn flow_error(kind: OAuthFailureKind, message: String) -> OAuthRequestError {
    OAuthRequestError::Flow(Box::new(OAuthFlowError::new(kind, message)))
}
fn deps_provider(
    deps: &AuthCommandDeps,
    redirect: &str,
) -> Result<McpOAuthProvider, OAuthRequestError> {
    build_provider(
        &deps.server_name,
        &deps.config,
        &deps.agent_dir,
        Some(redirect),
    )
}
fn pending(
    deps: &AuthCommandDeps,
) -> std::sync::MutexGuard<'_, BTreeMap<String, McpOAuthProvider>> {
    deps.pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// Upstream `runAuth` (`/mcp auth <server>`): client_credentials M2M, or the interactive flow. A caller
/// without UI gets an actionable headless hint and no browser.
pub async fn run_auth(deps: &AuthCommandDeps) -> Result<(), OAuthRequestError> {
    let name = &deps.server_name;
    if deps
        .config
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.flow.as_ref())
        == Some(&crate::config_schema::OAuthFlow::ClientCredentials)
    {
        let provider = deps_provider(deps, deps.callback_url.as_deref().unwrap_or(PASTE_REDIRECT))?;
        client_credentials_grant(&provider, &deps.client).await?;
        (deps.on_reconnect)().await;
        (deps.notify)(
            &format!("MCP server {name} authenticated (client_credentials)"),
            maho_ext_api::NotificationType::Info,
        );
        return Ok(());
    }
    // ensureOAuth precedes every flow.
    build_provider(name, &deps.config, &deps.agent_dir, None)?;
    if !deps.has_ui {
        (deps.notify)(
            &format!(
                "MCP server {name} needs interactive OAuth. Run senpi in a terminal, then: /mcp auth-start {name}  and  /mcp auth-complete {name} <redirect-url>"
            ),
            maho_ext_api::NotificationType::Error,
        );
        return Ok(());
    }
    run_interactive(deps).await
}
async fn run_interactive(deps: &AuthCommandDeps) -> Result<(), OAuthRequestError> {
    let name = deps.server_name.clone();
    let began = match &deps.interactive_guard {
        Some(guard) => (guard.begin)(&name),
        None => active_interactive_auth_servers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.clone()),
    };
    if !began {
        return Err(flow_error(
            OAuthFailureKind::NeedsAuth,
            format!(
                "MCP server {name} authorization is already in progress; complete the existing browser flow or retry after it finishes."
            ),
        ));
    }
    let result = interactive_flow(deps).await;
    match &deps.interactive_guard {
        Some(guard) => (guard.end)(&name),
        None => {
            active_interactive_auth_servers()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&name);
        }
    }
    result
}
async fn announce(deps: &AuthCommandDeps, url: &url::Url, uses_loopback: bool) {
    let name = &deps.server_name;
    let instruction = if uses_loopback {
        String::new()
    } else {
        format!("\nThen run /mcp auth-complete {name} <redirect-url> with the final redirect URL.")
    };
    let announcement = format!("Open this URL to authorize {name}:\n{url}{instruction}");
    (deps.notify)(&announcement, maho_ext_api::NotificationType::Info);
    if let Some(open) = &deps.open_browser
        && let Err(message) = open(url)
    {
        (deps.notify)(
            &format!("Browser launch failed: {message}\n{announcement}"),
            maho_ext_api::NotificationType::Warning,
        );
    }
}
async fn interactive_flow(deps: &AuthCommandDeps) -> Result<(), OAuthRequestError> {
    let name = &deps.server_name;
    if let Some(callback_url) = deps.callback_url.as_deref().filter(|url| !url.is_empty()) {
        let mut provider = deps_provider(deps, callback_url)?;
        let begin = begin_authorization(&mut provider, &deps.client).await?;
        let url = begin.authorization_url.ok_or_else(|| {
            flow_error(
                OAuthFailureKind::NeedsAuth,
                format!("MCP server {name} did not produce an authorization URL."),
            )
        })?;
        pending(deps).insert(name.clone(), provider);
        announce(deps, &url, false).await;
        return Ok(());
    }
    let oauth = deps.config.oauth.as_ref();
    let port = oauth
        .filter(|oauth| oauth.client_id.is_some() && oauth.client_metadata_url.is_none())
        .and_then(|oauth| oauth.callback_port);
    let provider = deps_provider(deps, PASTE_REDIRECT)?;
    run_loopback_auth(provider, port, &deps.client, |url| async move {
        announce(deps, &url, true).await;
        Ok(())
    })
    .await?;
    (deps.on_reconnect)().await;
    (deps.notify)(
        &format!("MCP server {name} authorized"),
        maho_ext_api::NotificationType::Info,
    );
    Ok(())
}
/// Upstream `runAuthStart` (`/mcp auth-start <server>`): print the authorize URL for a browser-less paste.
pub async fn run_auth_start_command(deps: &AuthCommandDeps) -> Result<String, OAuthRequestError> {
    let name = &deps.server_name;
    let mut provider = deps_provider(deps, deps.callback_url.as_deref().unwrap_or(PASTE_REDIRECT))?;
    let begin = begin_authorization(&mut provider, &deps.client).await?;
    let url = begin.authorization_url.ok_or_else(|| {
        flow_error(
            OAuthFailureKind::NeedsAuth,
            format!("MCP server {name} did not produce an authorization URL."),
        )
    })?;
    pending(deps).insert(name.clone(), provider);
    (deps.notify)(
        &format!(
            "Open this URL, approve, then run /mcp auth-complete {name} <redirect-url>:\n{url}"
        ),
        maho_ext_api::NotificationType::Info,
    );
    Ok(url.to_string())
}
/// Upstream `runAuthComplete` (`/mcp auth-complete <server> <redirect-url>`): finish the paste flow. Without a
/// pending provider a fresh one rejects the redirect's state.
pub async fn run_auth_complete_command(
    deps: &AuthCommandDeps,
    redirect: &str,
) -> Result<(), OAuthRequestError> {
    let name = &deps.server_name;
    let pending_provider = pending(deps).remove(name);
    let mut provider = match pending_provider {
        Some(provider) => provider,
        None => deps_provider(deps, deps.callback_url.as_deref().unwrap_or(PASTE_REDIRECT))?,
    };
    if let Err(error) = complete_authorization(&mut provider, redirect, &deps.client).await {
        // A failed completion keeps the pending flow, as upstream deletes it only on success.
        pending(deps).insert(name.clone(), provider);
        return Err(error);
    }
    (deps.on_reconnect)().await;
    (deps.notify)(
        &format!("MCP server {name} authorized"),
        maho_ext_api::NotificationType::Info,
    );
    Ok(())
}
/// Upstream `runLogout` (`/mcp logout <server>`): clear stored credentials.
pub async fn run_logout_command(deps: &AuthCommandDeps) -> Result<(), OAuthRequestError> {
    let name = &deps.server_name;
    let provider = McpOAuthProvider::new(McpTokenStore::new(
        &deps.agent_dir,
        name,
        deps.config.url.as_deref().unwrap_or(""),
    ));
    tokio::task::spawn_blocking(move || logout(&provider))
        .await
        .map_err(|error| OAuthRequestError::Invalid(error.to_string()))??;
    pending(deps).remove(name);
    (deps.on_reconnect)().await;
    (deps.notify)(
        &format!("MCP server {name} logged out"),
        maho_ext_api::NotificationType::Info,
    );
    Ok(())
}
