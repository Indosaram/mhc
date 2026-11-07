use super::{
    oauth::OAuthRequestError,
    oauth_errors::{OAuthFailureKind, OAuthFlowError},
    token_store::{McpStoredAuth, McpTokenStore, TokenStoreError},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub const REFRESH_LEEWAY_MS: f64 = 300000.0;
pub type McpRedirectHandler = std::sync::Arc<
    dyn Fn(
            url::Url,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ProviderError>> + Send>>
        + Send
        + Sync,
>;
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub token_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<f64>,
}
pub fn token_expires_at(tokens: &OAuthTokens, now: f64) -> Option<f64> {
    tokens.expires_in.map(|s| now + s * 1000.0)
}
pub fn stored_auth_to_tokens(record: Option<&McpStoredAuth>, now: f64) -> Option<OAuthTokens> {
    let record = record?;
    let access = record.access_token.as_ref().filter(|s| !s.is_empty())?;
    // senpi#2940: an unattributed refresh token is withheld; it would be posted to whichever server is discovered.
    let refresh_token = record.issuer.as_ref().and(record.refresh_token.clone());
    Some(OAuthTokens {
        access_token: access.clone(),
        refresh_token,
        token_type: "Bearer".into(),
        expires_in: record
            .expires_at
            .map(|t| ((t - now) / 1000.0).ceil().max(0.0)),
    })
}
pub fn merge_tokens_into_stored_auth(
    current: Option<McpStoredAuth>,
    tokens: &OAuthTokens,
    server_url: &str,
    now: f64,
    issuer: Option<&str>,
) -> McpStoredAuth {
    let mut next = current.unwrap_or_default();
    next.access_token = Some(tokens.access_token.clone());
    if let Some(issuer) = issuer {
        next.issuer = Some(issuer.into());
    }
    next.resource
        .get_or_insert_with(|| super::oauth::normalize_resource_url(server_url));
    if let Some(refresh) = &tokens.refresh_token {
        next.refresh_token = Some(refresh.clone());
    }
    next.expires_at = token_expires_at(tokens, now);
    next
}
/// Runs a store operation on a blocking thread so the file lock waits there rather than on an async worker.
async fn spawn_store<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, TokenStoreError> + Send + 'static,
) -> Result<T, OAuthRequestError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| OAuthRequestError::Invalid(error.to_string()))?
        .map_err(OAuthRequestError::from)
}
pub struct McpOAuthProvider {
    pub store: McpTokenStore,
    pub redirect_url: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub client_id: Option<String>,
    pub client_metadata_url: Option<String>,
    pub logger: Option<std::sync::Arc<std::sync::Mutex<crate::log::McpLogger>>>,
    pub on_redirect: Option<McpRedirectHandler>,
    expected_state: Option<String>,
    pub last_authorization_url: Option<url::Url>,
    pub require_https: bool,
    escalated_scopes: std::sync::Mutex<Vec<String>>,
}
#[derive(Clone, Copy)]
pub enum CredentialScope {
    All,
    Client,
    Tokens,
    Verifier,
    Discovery,
}
impl McpOAuthProvider {
    pub fn new(store: McpTokenStore) -> Self {
        Self {
            store,
            redirect_url: None,
            scopes: None,
            client_id: None,
            client_metadata_url: None,
            logger: None,
            on_redirect: None,
            expected_state: None,
            last_authorization_url: None,
            require_https: true,
            escalated_scopes: std::sync::Mutex::new(Vec::new()),
        }
    }
    /// Merges scopes demanded by a step-up challenge into the in-memory escalated set.
    pub fn escalate_scopes(&self, scopes: &[String]) {
        let mut current = self
            .escalated_scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let merged = super::challenge::merge_scopes(&current, scopes);
        *current = merged;
    }
    /// Configured scopes followed by escalated ones; `None` when both are empty.
    pub fn effective_scopes(&self) -> Option<Vec<String>> {
        let escalated = self
            .escalated_scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if self.scopes.is_none() && escalated.is_empty() {
            return None;
        }
        Some(super::challenge::merge_scopes(
            self.scopes.as_deref().unwrap_or(&[]),
            &escalated,
        ))
    }
    pub fn client_metadata(&self) -> Value {
        let mut value = json!({"redirect_uris":self.redirect_url.iter().collect::<Vec<_>>(),"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none","client_name":"mhc"});
        if let Some(scopes) = &self.scopes {
            value["scope"] = Value::String(scopes.join(" "));
        }
        // senpi#4194: loopback or custom-scheme redirects register a native client.
        if let Some(redirect) = self
            .redirect_url
            .as_deref()
            .and_then(|redirect| url::Url::parse(redirect).ok())
        {
            let native = !["http", "https"].contains(&redirect.scheme())
                || ["127.0.0.1", "localhost", "[::1]"]
                    .contains(&redirect.host_str().unwrap_or_default());
            value["application_type"] = Value::String(if native { "native" } else { "web" }.into());
        }
        value
    }
    pub fn state(&mut self) -> Result<String, getrandom::Error> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)?;
        let state = URL_SAFE_NO_PAD.encode(bytes);
        self.expected_state = Some(state.clone());
        Ok(state)
    }
    pub fn consume_state(&mut self, candidate: Option<&str>) -> bool {
        let expected = self.expected_state.take();
        expected.as_deref().is_some_and(|s| Some(s) == candidate)
    }
    pub fn client_information(&self) -> Result<Option<Value>, TokenStoreError> {
        if let Some(id) = self
            .client_id
            .as_ref()
            .or(self.client_metadata_url.as_ref())
        {
            return Ok(Some(json!({"client_id":id})));
        }
        Ok(self.store.read()?.and_then(|r| r.client_info))
    }
    /// `client_information` on a blocking thread: the stored-record read may wait on the legacy
    /// migration lock.
    pub async fn client_information_async(&self) -> Result<Option<Value>, OAuthRequestError> {
        if let Some(id) = self
            .client_id
            .as_ref()
            .or(self.client_metadata_url.as_ref())
        {
            return Ok(Some(json!({"client_id":id})));
        }
        let store = self.store.clone();
        spawn_store(move || Ok(store.read()?.and_then(|r| r.client_info))).await
    }
    /// Persists a registration stamped with the validated issuer that registered it (senpi#4194).
    pub fn save_client_information(
        &self,
        info: Value,
        issuer: Option<&str>,
    ) -> Result<(), TokenStoreError> {
        Self::save_client_information_in(&self.store, info, issuer)
    }
    /// `save_client_information` on a blocking thread: the store update waits on the file lock.
    pub async fn save_client_information_async(
        &self,
        info: Value,
        issuer: Option<&str>,
    ) -> Result<(), OAuthRequestError> {
        let store = self.store.clone();
        let issuer = issuer.map(str::to_owned);
        spawn_store(move || Self::save_client_information_in(&store, info, issuer.as_deref())).await
    }
    fn save_client_information_in(
        store: &McpTokenStore,
        mut info: Value,
        issuer: Option<&str>,
    ) -> Result<(), TokenStoreError> {
        let issuer = issuer.map(str::to_owned).or_else(|| {
            info.get("issuer")
                .and_then(Value::as_str)
                .map(super::oauth_issuer::canonical_authorization_server_issuer)
        });
        if let (Some(issuer), Some(object)) = (issuer, info.as_object_mut()) {
            object.insert("issuer".into(), Value::String(issuer));
        }
        store.update(|r| {
            let mut next = r.unwrap_or_default();
            next.client_info = Some(info);
            Some(next)
        })?;
        Ok(())
    }
    /// Binds stored credentials to the authorization server discovery validated now (upstream `saveDiscoveryState`):
    /// credentials stamped for another issuer are dropped, and a registration that cannot be attributed is never
    /// sent to the server discovered now.
    pub fn bind_issuer(&self, issuer: &str) -> Result<(), TokenStoreError> {
        Self::bind_issuer_in(&self.store, issuer)
    }
    /// `bind_issuer` on a blocking thread: the store update waits on the file lock.
    pub async fn bind_issuer_async(&self, issuer: &str) -> Result<(), OAuthRequestError> {
        let store = self.store.clone();
        let issuer = issuer.to_owned();
        spawn_store(move || Self::bind_issuer_in(&store, &issuer)).await
    }
    fn bind_issuer_in(store: &McpTokenStore, issuer: &str) -> Result<(), TokenStoreError> {
        store.update(|current| {
            let mut next = current?;
            // senpi#2940 `freezeGrantAttribution`: discovery made now never attributes an existing grant, so an
            // unattributed grant loses its refresh token instead of being sent to the server discovered now.
            if next.issuer.is_none()
                && (next.access_token.is_some() || next.refresh_token.is_some())
            {
                next.refresh_token = None;
            }
            if !super::oauth_issuer::credentials_match_issuer(Some(&next), issuer) {
                return None;
            }
            let client_issuer = next
                .client_info
                .as_ref()
                .and_then(|info| info.get("issuer"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| next.issuer.clone());
            next.client_info = match (client_issuer, next.client_info.take()) {
                (Some(_), Some(mut info)) => {
                    if let Some(object) = info.as_object_mut() {
                        object.insert("issuer".into(), Value::String(issuer.into()));
                    }
                    Some(info)
                }
                _ => None,
            };
            Some(next)
        })?;
        Ok(())
    }
    pub fn tokens(&self, now: f64) -> Result<Option<OAuthTokens>, TokenStoreError> {
        Ok(stored_auth_to_tokens(self.store.read()?.as_ref(), now))
    }
    /// `tokens` on a blocking thread: the stored-record read may wait on the legacy migration lock.
    pub async fn tokens_async(&self, now: f64) -> Result<Option<OAuthTokens>, OAuthRequestError> {
        let store = self.store.clone();
        spawn_store(move || Ok(stored_auth_to_tokens(store.read()?.as_ref(), now))).await
    }
    pub fn save_tokens(&self, tokens: &OAuthTokens, now: f64) -> Result<(), TokenStoreError> {
        self.save_tokens_with_issuer(tokens, now, None)
    }
    /// `save_tokens` with the validated issuer stamped on the grant (senpi#4194).
    pub fn save_tokens_with_issuer(
        &self,
        tokens: &OAuthTokens,
        now: f64,
        issuer: Option<&str>,
    ) -> Result<(), TokenStoreError> {
        Self::save_tokens_with_issuer_in(&self.store, self.logger.as_deref(), tokens, now, issuer)
    }
    /// `save_tokens_with_issuer` on a blocking thread: the store update waits on the file lock.
    pub async fn save_tokens_with_issuer_async(
        &self,
        tokens: &OAuthTokens,
        now: f64,
        issuer: Option<&str>,
    ) -> Result<(), OAuthRequestError> {
        let store = self.store.clone();
        let logger = self.logger.clone();
        let tokens = tokens.clone();
        let issuer = issuer.map(str::to_owned);
        spawn_store(move || {
            Self::save_tokens_with_issuer_in(
                &store,
                logger.as_deref(),
                &tokens,
                now,
                issuer.as_deref(),
            )
        })
        .await
    }
    fn save_tokens_with_issuer_in(
        store: &McpTokenStore,
        logger: Option<&std::sync::Mutex<crate::log::McpLogger>>,
        tokens: &OAuthTokens,
        now: f64,
        issuer: Option<&str>,
    ) -> Result<(), TokenStoreError> {
        store.update(|r| {
            Some(merge_tokens_into_stored_auth(
                r,
                tokens,
                &store.server_url,
                now,
                issuer,
            ))
        })?;
        if let Some(logger) = logger {
            let _ = logger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .log(
                    "info",
                    "oauth saveTokens",
                    Some(&json!({"token_fp":crate::log::fingerprint_secret(&tokens.access_token)})),
                    None,
                );
        }
        Ok(())
    }
    pub fn save_code_verifier(&self, verifier: &str) -> Result<(), TokenStoreError> {
        self.store.update(|r| {
            let mut next = r.unwrap_or_default();
            next.code_verifier = Some(verifier.into());
            Some(next)
        })?;
        Ok(())
    }
    /// `save_code_verifier` on a blocking thread: the store update waits on the file lock.
    pub async fn save_code_verifier_async(&self, verifier: &str) -> Result<(), OAuthRequestError> {
        let store = self.store.clone();
        let verifier = verifier.to_owned();
        spawn_store(move || {
            store.update(|r| {
                let mut next = r.unwrap_or_default();
                next.code_verifier = Some(verifier);
                Some(next)
            })?;
            Ok(())
        })
        .await
    }
    pub fn code_verifier(&self) -> Result<String, ProviderError> {
        self.store
            .read()?
            .and_then(|r| r.code_verifier)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProviderError::OAuth(Box::new(OAuthFlowError::new(
                    OAuthFailureKind::NoVerifier,
                    format!(
                        "Missing PKCE code verifier for MCP server {}",
                        self.store.server_name
                    ),
                )))
            })
    }
    /// `code_verifier` on a blocking thread: the stored-record read may wait on the legacy
    /// migration lock.
    pub async fn code_verifier_async(&self) -> Result<String, OAuthRequestError> {
        let store = self.store.clone();
        let server_name = self.store.server_name.clone();
        let stored = spawn_store(move || Ok(store.read()?.and_then(|r| r.code_verifier))).await?;
        stored.filter(|s| !s.is_empty()).ok_or_else(|| {
            ProviderError::OAuth(Box::new(OAuthFlowError::new(
                OAuthFailureKind::NoVerifier,
                format!("Missing PKCE code verifier for MCP server {server_name}"),
            )))
            .into()
        })
    }
    pub async fn redirect_to_authorization(
        &mut self,
        authorization_url: url::Url,
    ) -> Result<(), ProviderError> {
        self.last_authorization_url = Some(authorization_url.clone());
        if let Some(handler) = &self.on_redirect {
            handler(authorization_url).await?;
        }
        Ok(())
    }
    pub fn validate_resource_url(
        server_url: &str,
        resource: Option<&str>,
    ) -> Result<url::Url, url::ParseError> {
        url::Url::parse(resource.unwrap_or(server_url))
    }
    pub fn invalidate_credentials(&self, scope: CredentialScope) -> Result<(), TokenStoreError> {
        Self::invalidate_credentials_in(&self.store, scope)
    }
    /// `invalidate_credentials` on a blocking thread: the store update waits on the file lock.
    pub async fn invalidate_credentials_async(
        &self,
        scope: CredentialScope,
    ) -> Result<(), OAuthRequestError> {
        let store = self.store.clone();
        spawn_store(move || Self::invalidate_credentials_in(&store, scope)).await
    }
    fn invalidate_credentials_in(
        store: &McpTokenStore,
        scope: CredentialScope,
    ) -> Result<(), TokenStoreError> {
        if matches!(scope, CredentialScope::All) {
            return store.clear();
        }
        if matches!(scope, CredentialScope::Discovery) {
            super::oauth::reset_discovery_cache();
            return Ok(());
        }
        store.update(|current| {
            current.map(|mut next| {
                match scope {
                    CredentialScope::All => (),
                    CredentialScope::Tokens => {
                        next.access_token = None;
                        next.refresh_token = None;
                        next.expires_at = None;
                    }
                    CredentialScope::Client => next.client_info = None,
                    CredentialScope::Verifier => next.code_verifier = None,
                    CredentialScope::Discovery => (),
                }
                next
            })
        })?;
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error(transparent)]
    Store(#[from] TokenStoreError),
    #[error(transparent)]
    OAuth(Box<OAuthFlowError>),
}
