//! Port of senpi packages/ai/src/auth/oauth/openai-chatgpt.ts.

use super::oauth_page::{oauth_error_html, oauth_success_html};
use super::openai_id_token::{DISCOVERY_URL, OpenAIIdTokenVerifier, https_endpoint};
use super::pkce::generate_pkce;
use super::transport::{HttpRequest, HttpResponse, OAuthTransport};
use crate::auth::types::{
    AuthCheck, AuthContext, AuthEvent, AuthPrompt, AuthPromptKind, AuthType, LoginOptions,
    LogoutOptions, ModelAuth, OAuthAuth, OAuthCredential, OAuthRevocationResult,
    ProviderAuthInteraction,
};
use crate::utils::abort::{AbortController, AbortSignal, operation_signal, race_with_abort_signal};
use crate::utils::oauth_refresh_error::{OAuthRefreshFailureClass, classify_oauth_refresh_failure};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use url::Url;

const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const CALLBACK_PATH: &str = "/auth/callback";
const DIRECT_TOKEN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const EXPIRY_MARGIN_MS: f64 = 180_000.0;

#[derive(Debug, PartialEq)]
struct AuthorizationResult {
    code: String,
    client_id: String,
}

struct CallbackServer {
    task: JoinHandle<()>,
    result: mpsc::UnboundedReceiver<anyhow::Result<AuthorizationResult>>,
    port: u16,
}

#[derive(Clone)]
enum KnownClient {
    Reauthorize(String),
    Consent(String),
}

impl KnownClient {
    fn id(&self) -> &str {
        match self {
            Self::Reauthorize(id) | Self::Consent(id) => id,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenAIReauthorizationError {
    #[error("The selected ChatGPT account needs an issued client ID and verified subject")]
    InvalidAccount,
    #[error("ChatGPT reauthorization returned a different client ID")]
    ClientMismatch,
    #[error("ChatGPT reauthorization returned a different account")]
    SubjectMismatch,
}

#[derive(Debug, thiserror::Error)]
#[error("ChatGPT consent declined")]
pub struct OpenAIConsentDeclinedError;

#[derive(Debug, thiserror::Error)]
#[error("ChatGPT plan usage is not enabled. Enable ChatGPT plan usage or use an API key.")]
pub struct OpenAIPlanUsageNotEnabledError;

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct ManualPromptCancellation(AbortController);

impl Drop for ManualPromptCancellation {
    fn drop(&mut self) {
        self.0.abort(None);
    }
}

fn authorization_result_from_callback(
    url: &Url,
    expected_state: &str,
    known_client: Option<&KnownClient>,
) -> anyhow::Result<AuthorizationResult> {
    let params = url.query_pairs().collect::<Vec<_>>();
    let get = |key| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
    };
    let state = get("state")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing OAuth state"))?;
    if state != expected_state {
        anyhow::bail!("OAuth state mismatch");
    }
    if let Some(error) = get("error").filter(|s| !s.is_empty()) {
        if error == "access_denied" {
            return Err(OpenAIConsentDeclinedError.into());
        }
        anyhow::bail!("ChatGPT authorization failed: {error}");
    }
    let code = get("code")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing authorization code"))?;
    let supplied = get("client_id");
    if let Some(KnownClient::Reauthorize(client)) = known_client
        && supplied.is_some_and(|supplied| supplied != client)
    {
        return Err(OpenAIReauthorizationError::ClientMismatch.into());
    }
    let callback_client = supplied.map(str::trim).filter(|s| !s.is_empty());
    if let Some(KnownClient::Consent(client)) = known_client
        && callback_client.is_some_and(|supplied| supplied != client)
    {
        anyhow::bail!("ChatGPT consent callback client ID mismatch");
    }
    let client_id = known_client
        .map(KnownClient::id)
        .or(callback_client)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "OpenAI OAuth registration callback did not contain an issued client ID"
            )
        })?;
    Ok(AuthorizationResult {
        code: code.into(),
        client_id: client_id.into(),
    })
}

fn authorization_result_from_manual_input(
    input: &str,
    state: &str,
    redirect_uri: &str,
    known_client: Option<&KnownClient>,
) -> anyhow::Result<AuthorizationResult> {
    let url = Url::parse(input.trim())
        .map_err(|_| anyhow::anyhow!("Paste the full callback URL from the browser"))?;
    let expected = Url::parse(redirect_uri)?;
    if url.origin() != expected.origin() || url.path() != expected.path() {
        anyhow::bail!("The pasted callback URL must start with {redirect_uri}");
    }
    authorization_result_from_callback(&url, state, known_client)
}

fn callback_response(
    url: &Url,
    state: &str,
    known_client: Option<&KnownClient>,
) -> (u16, String, Option<anyhow::Result<AuthorizationResult>>) {
    if url.path() != CALLBACK_PATH {
        return (
            404,
            oauth_error_html("Callback route not found.", None),
            None,
        );
    }
    match authorization_result_from_callback(url, state, known_client) {
        Ok(result) => (
            200,
            oauth_success_html("ChatGPT authentication completed. You can close this window."),
            Some(Ok(result)),
        ),
        Err(error) => {
            let matching_state = url
                .query_pairs()
                .find(|(key, _)| key == "state")
                .is_some_and(|(_, value)| value == state);
            let terminal = error.is::<OpenAIReauthorizationError>()
                || (matching_state && url.query_pairs().any(|(key, _)| key == "error"));
            (
                400,
                oauth_error_html(&error.to_string(), None),
                terminal.then_some(Err(error)),
            )
        }
    }
}

async fn start_callback_server(
    state: String,
    host: &str,
    port: u16,
    known_client: Option<KnownClient>,
) -> anyhow::Result<CallbackServer> {
    let listener = match TcpListener::bind((host, port)).await {
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            TcpListener::bind((host, 0)).await?
        }
        result => result?,
    };
    let port = listener.local_addr()?.port();
    let (sender, result) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        // Dropping the owner aborts every connection, including browser preconnections.
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = match accepted {
                        Ok(value) => value,
                        Err(error) => { if sender.send(Err(error.into())).is_err() { return; } return; }
                    };
                    let sender = sender.clone();
                    let state = state.clone();
                    let known_client = known_client.clone();
                    connections.spawn(async move {
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 1024];
                        loop {
                            let count = match stream.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(count) => count,
                            };
                            buffer.extend_from_slice(&chunk[..count]);
                            if buffer.windows(4).any(|part| part == b"\r\n\r\n") { break; }
                            if buffer.len() > 16 * 1024 { return; }
                        }
                        let head = String::from_utf8_lossy(&buffer);
                        let target = head.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("/");
                        let (status, body, outcome) = match Url::parse(REDIRECT_URI).and_then(|base| base.join(target)) {
                            Ok(url) => callback_response(&url, &state, known_client.as_ref()),
                            Err(_) => (500, oauth_error_html("Internal error while processing the callback.", None), None),
                        };
                        let reason = match status { 200 => "OK", 400 => "Bad Request", 404 => "Not Found", _ => "Internal Server Error" };
                        let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        if let Err(error) = stream.write_all(response.as_bytes()).await {
                            eprintln!("OAuth callback response write failed: {error}");
                        }
                        if let Some(outcome) = outcome && sender.send(outcome).is_err() {
                            eprintln!("OAuth callback receiver closed before delivery");
                        }
                    });
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = completed
                        && sender.send(Err(error.into())).is_err() { return; }
                }
            }
        }
    });
    Ok(CallbackServer { task, result, port })
}

fn callback_uri(host: &str, port: u16) -> anyhow::Result<String> {
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let url = Url::parse(&format!("http://{host}:{port}{CALLBACK_PATH}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.port_or_known_default() != Some(port)
        || url.path() != CALLBACK_PATH
    {
        anyhow::bail!("Invalid OAuth callback host");
    }
    Ok(url.into())
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI OAuth token request failed ({status}): {body}")]
pub struct OAuthTokenEndpointError {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI OAuth refresh requires reauthorization ({code})")]
pub struct OAuthReauthorizationRequiredError {
    pub code: String,
    pub status: u16,
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI OAuth client configuration is invalid")]
pub struct OAuthClientConfigurationError {
    pub status: u16,
}

fn token_error_code(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    object
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| object.get("code").and_then(Value::as_str))
        .or_else(|| object.get("error").and_then(token_error_code))
        .or_else(|| object.get("type").and_then(Value::as_str))
}

async fn request_token(
    transport: &dyn OAuthTransport,
    fields: &[(&str, &str)],
    signal: &AbortSignal,
) -> anyhow::Result<Value> {
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.extend_pairs(fields.iter().copied());
        serializer.finish()
    };
    let response = transport
        .execute(
            HttpRequest {
                method: "POST".into(),
                url: TOKEN_URL.into(),
                headers: vec![
                    ("accept".into(), "application/json".into()),
                    (
                        "content-type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ),
                ],
                body: Some(body),
                timeout_ms: None,
            },
            signal,
        )
        .await?;
    if !response.ok() {
        let body = if response.body.is_empty() {
            reqwest::StatusCode::from_u16(response.status)
                .ok()
                .and_then(|s| s.canonical_reason())
                .unwrap_or_default()
                .to_owned()
        } else {
            response.body
        };
        if fields.contains(&("grant_type", "refresh_token"))
            && classify_oauth_refresh_failure(
                &crate::utils::oauth_refresh_error::OAuthTokenEndpointError::new(
                    "OpenAI OAuth refresh failed",
                    response.status,
                ),
            ) == OAuthRefreshFailureClass::Permanent
            && let Ok(parsed) = serde_json::from_str::<Value>(&body)
        {
            match token_error_code(&parsed) {
                Some(
                    code @ ("invalid_grant"
                    | "invalid_refresh_token"
                    | "token_expired"
                    | "refresh_token_expired"
                    | "refresh_token_invalidated"
                    | "refresh_token_reused"),
                ) => {
                    return Err(OAuthReauthorizationRequiredError {
                        code: code.into(),
                        status: response.status,
                    }
                    .into());
                }
                Some("invalid_client") => {
                    return Err(OAuthClientConfigurationError {
                        status: response.status,
                    }
                    .into());
                }
                _ => {}
            }
        }
        if fields.contains(&("grant_type", "refresh_token")) {
            return Err(
                crate::utils::oauth_refresh_error::OAuthTokenEndpointError::new(
                    format!("OpenAI OAuth token request failed ({})", response.status),
                    response.status,
                )
                .into(),
            );
        }
        return Err(OAuthTokenEndpointError {
            status: response.status,
            body,
        }
        .into());
    }
    let token = response.json()?;
    if !token.is_object() {
        anyhow::bail!("OpenAI OAuth token response must be an object");
    }
    Ok(token)
}

fn require_token_string<'a>(token: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    token[field]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("OpenAI OAuth token response has invalid {field}"))
}

fn credential_from_token_response(
    token: &Value,
    client_id: &str,
    now_ms: f64,
) -> anyhow::Result<OAuthCredential> {
    let access = require_token_string(token, "access_token")?;
    let refresh = require_token_string(token, "refresh_token")?;
    let scope = require_token_string(token, "scope")?;
    let expires_in = token["expires_in"]
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.0)
        .ok_or_else(|| anyhow::anyhow!("OpenAI OAuth token response has invalid expires_in"))?;
    let scopes: Vec<&str> = scope.split_whitespace().collect();
    Ok(OAuthCredential::new(
        access,
        refresh,
        now_ms + expires_in * 1000.0 - EXPIRY_MARGIN_MS,
    )
    .with_extra("clientId", json!(client_id))
    .with_extra(
        "planUsageEnabled",
        json!(scopes.contains(&DIRECT_TOKEN_SCOPE)),
    )
    .with_extra("scopes", json!(scopes)))
}

fn agent_host_id(device_id: Option<&str>) -> anyhow::Result<String> {
    let Some(device_id) = device_id.filter(|s| s.len() == 36) else {
        anyhow::bail!("Sign in with ChatGPT requires a device ID (UUID) for this installation");
    };
    if !device_id.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        14 => b == b'4',
        19 => matches!(b.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
        _ => b.is_ascii_hexdigit(),
    }) {
        anyhow::bail!("Sign in with ChatGPT requires a device ID (UUID) for this installation");
    }
    Ok(format!("urn:uuid:{}", device_id.to_ascii_lowercase()))
}

fn authorization_url(
    host_id: &str,
    challenge: &str,
    state: &str,
    nonce: &str,
    redirect_uri: &str,
    options: Option<&LoginOptions>,
    known_client: Option<&KnownClient>,
) -> anyhow::Result<Url> {
    let mut url = Url::parse(AUTHORIZE_URL)?;
    let mut query = url.query_pairs_mut();
    query.extend_pairs([
        (
            "client_id",
            known_client.map_or(DYNAMIC_CLIENT_ID, KnownClient::id),
        ),
        ("ext_agent_host_id", host_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("resource", RESOURCE),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("nonce", nonce),
    ]);
    if let Some(existing) = options.and_then(|options| options.reauthorize.as_ref()) {
        for (field, hint) in [("idToken", "id_token_hint"), ("email", "login_hint")] {
            if let Some(value) = existing.get_extra_str(field).filter(|s| !s.is_empty()) {
                query.append_pair(hint, value);
            }
        }
    } else if let Some(name) = options.and_then(|options| options.app_name.as_deref()) {
        query.append_pair("agent_name_hint", name);
    } else {
        query.append_pair(
            "agent_name_hint",
            &crate::wire_identity::get_wire_identity(),
        );
    }
    if options.is_some_and(|options| options.plan_usage_consent.is_some()) {
        query.append_pair("prompt", "consent");
    }
    drop(query);
    Ok(url)
}

pub struct OpenAIChatGPTOAuth {
    transport: Arc<dyn OAuthTransport>,
    verifier: OpenAIIdTokenVerifier,
    preferred_callback_port: u16,
}
impl OpenAIChatGPTOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self {
            transport,
            verifier: OpenAIIdTokenVerifier::default(),
            preferred_callback_port: 1455,
        }
    }

    #[cfg(test)]
    fn with_callback_port(mut self, port: u16) -> Self {
        self.preferred_callback_port = port;
        self
    }

    async fn exchange_authorization_code(
        &self,
        result: AuthorizationResult,
        verifier: &str,
        nonce: &str,
        redirect_uri: &str,
        signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        let token = request_token(
            self.transport.as_ref(),
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &result.client_id),
                ("code", &result.code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
                ("resource", RESOURCE),
            ],
            signal,
        )
        .await?;
        let id_token = token["id_token"].as_str();
        let identity = self
            .verifier
            .verify(
                self.transport.as_ref(),
                id_token,
                &result.client_id,
                nonce,
                signal,
            )
            .await?;
        let mut credential =
            credential_from_token_response(&token, &result.client_id, self.transport.now_ms())?
                .with_extra("subject", json!(identity.subject))
                .with_extra("issuer", json!(identity.issuer))
                .with_extra("idToken", json!(id_token));
        if let Some(email) = identity.email {
            credential.extra.insert("email".into(), json!(email));
        }
        Ok(credential)
    }
}

#[async_trait]
impl OAuthAuth for OpenAIChatGPTOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT subscription)"
    }
    fn is_subscription(&self) -> bool {
        true
    }
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with ChatGPT")
    }
    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        self.login_with_options(interaction, None).await
    }

    async fn login_with_options(
        &self,
        interaction: &ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> anyhow::Result<OAuthCredential> {
        let existing = options
            .as_ref()
            .and_then(|options| options.reauthorize.as_ref());
        let existing_identity = existing
            .map(|existing| {
                let client = existing
                    .get_extra_str("clientId")
                    .filter(|id| !id.trim().is_empty() && *id != DYNAMIC_CLIENT_ID)
                    .ok_or(OpenAIReauthorizationError::InvalidAccount)?;
                let subject = existing
                    .get_extra_str("subject")
                    .filter(|subject| !subject.trim().is_empty())
                    .ok_or(OpenAIReauthorizationError::InvalidAccount)?;
                Ok::<_, OpenAIReauthorizationError>((client, subject))
            })
            .transpose()?;
        let host_id = agent_host_id(interaction.device_id())?;
        let consent_client = options
            .as_ref()
            .and_then(|options| options.plan_usage_consent.as_ref());
        if consent_client.is_some_and(|consent| {
            consent.client_id.trim().is_empty() || consent.client_id == DYNAMIC_CLIENT_ID
        }) {
            anyhow::bail!("Enabling ChatGPT plan usage requires the saved issued client ID");
        }
        if let (Some((client, _)), Some(consent)) = (existing_identity, consent_client)
            && client != consent.client_id
        {
            return Err(OpenAIReauthorizationError::ClientMismatch.into());
        }
        let known_client = existing_identity
            .map(|(client, _)| KnownClient::Reauthorize(client.into()))
            .or_else(|| {
                consent_client.map(|consent| KnownClient::Consent(consent.client_id.clone()))
            });
        let pkce = generate_pkce();
        let state = generate_pkce().verifier;
        let nonce = generate_pkce().verifier;
        let callback_host = super::loopback::callback_host();
        let fallback_uri = callback_uri(&callback_host, self.preferred_callback_port)?;
        let mut callback = match start_callback_server(
            state.clone(),
            &callback_host,
            self.preferred_callback_port,
            known_client.clone(),
        )
        .await
        {
            Ok(callback) => Some(callback),
            Err(error) => {
                interaction.notify(AuthEvent::Info { message: format!("Could not listen on {fallback_uri}; paste the final redirect URL to continue. {error}"), links: None });
                None
            }
        };
        let redirect_uri = match &callback {
            Some(server) => callback_uri(&callback_host, server.port)?,
            None => fallback_uri,
        };
        interaction.notify(AuthEvent::AuthUrl { url: authorization_url(
            &host_id, &pkce.challenge, &state, &nonce, &redirect_uri,
            options.as_ref(), known_client.as_ref(),
        )?.into(),
            instructions: Some("Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here.".into()) });
        let manual_abort = ManualPromptCancellation(AbortController::new());
        let manual = async {
            let input = interaction
                .prompt(AuthPrompt {
                    kind: AuthPromptKind::ManualCode {
                        message:
                            "Complete login in your browser, or paste the final redirect URL here:"
                                .into(),
                        placeholder: Some(redirect_uri.clone()),
                    },
                    signal: Some(manual_abort.0.signal()),
                })
                .await?;
            authorization_result_from_manual_input(
                &input,
                &state,
                &redirect_uri,
                known_client.as_ref(),
            )
        };
        let result = async {
            let authorization = tokio::select! {
                biased;
                () = interaction.signal.cancelled() => anyhow::bail!("Login cancelled"),
                result = manual => result?,
                result = async {
                    match callback.as_mut() {
                        Some(server) => server.result.recv().await.ok_or_else(|| anyhow::anyhow!("OAuth callback server closed"))?,
                        None => std::future::pending().await,
                    }
                } => result?,
            };
            interaction.notify(AuthEvent::Progress { message: "Exchanging authorization code for tokens...".into() });
            let credential = tokio::select! {
                biased;
                () = interaction.signal.cancelled() => anyhow::bail!("Login cancelled"),
                result = self.exchange_authorization_code(
                    authorization, &pkce.verifier, &nonce, &redirect_uri, &interaction.signal,
                ) => result?,
            };
            if let Some((_, subject)) = existing_identity
                && credential.get_extra_str("subject") != Some(subject)
            {
                return Err(OpenAIReauthorizationError::SubjectMismatch.into());
            }
            Ok(credential)
        }.await;
        drop(manual_abort);
        drop(callback);
        if interaction.signal.aborted() {
            anyhow::bail!("Login cancelled");
        }
        result
    }
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        let client_id = credential.get_extra_str("clientId").filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Stored OpenAI OAuth credential does not contain an issued client ID; reconnect ChatGPT"))?;
        let token = request_token(
            self.transport.as_ref(),
            &[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", &credential.refresh),
                ("resource", RESOURCE),
            ],
            signal,
        )
        .await?;
        let mut refreshed =
            credential_from_token_response(&token, client_id, self.transport.now_ms())?;
        for (key, value) in &credential.extra {
            refreshed
                .extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        Ok(refreshed)
    }

    fn has_revoke(&self) -> bool {
        true
    }

    async fn revoke(
        &self,
        credential: &OAuthCredential,
        options: Option<LogoutOptions>,
    ) -> anyhow::Result<OAuthRevocationResult> {
        Ok(revoke_session(self.transport.as_ref(), credential, options).await)
    }

    async fn check(
        &self,
        _ctx: &dyn AuthContext,
        credential: Option<&OAuthCredential>,
        _signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        Ok(credential
            .filter(|credential| {
                !credential.access.is_empty()
                    && !credential.refresh.is_empty()
                    && !credential.is_signed_out()
            })
            .map(|_| AuthCheck {
                auth_type: AuthType::OAuth,
                source: Some("OAuth".into()),
                ambient: false,
            }))
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        if credential
            .get_extra("planUsageEnabled")
            .and_then(Value::as_bool)
            == Some(false)
            || !credential
                .get_extra("scopes")
                .and_then(Value::as_array)
                .is_some_and(|scopes| scopes.iter().any(|scope| scope == DIRECT_TOKEN_SCOPE))
        {
            return Err(OpenAIPlanUsageNotEnabledError.into());
        }
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..Default::default()
        })
    }
}

async fn revoke_session(
    transport: &dyn OAuthTransport,
    credential: &OAuthCredential,
    options: Option<LogoutOptions>,
) -> OAuthRevocationResult {
    use OAuthRevocationResult::{RemoteRevocationUnconfirmed, Revoked};

    let Some(client_id) = credential
        .get_extra_str("clientId")
        .filter(|s| !s.trim().is_empty())
    else {
        return RemoteRevocationUnconfirmed;
    };
    if credential.refresh.is_empty() {
        return RemoteRevocationUnconfirmed;
    }
    let LogoutOptions { signal, sleep, .. } = options.unwrap_or_default();
    let signal = operation_signal(signal);
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.extend_pairs([
            ("token", credential.refresh.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
        ]);
        serializer.finish()
    };
    let operation = async {
        let mut endpoint: Option<Url> = None;
        for delay in [0, 250, 1000] {
            if delay > 0 {
                match &sleep {
                    Some(sleep) => sleep(delay).await,
                    None => tokio::time::sleep(Duration::from_millis(delay)).await,
                }
            }
            if endpoint.is_none() {
                let Ok(response) = transport
                    .execute(
                        HttpRequest {
                            method: "GET".into(),
                            url: DISCOVERY_URL.into(),
                            headers: Vec::new(),
                            body: None,
                            timeout_ms: None,
                        },
                        &signal,
                    )
                    .await
                else {
                    continue;
                };
                if response.status >= 500 {
                    continue;
                }
                if !response.ok() {
                    return RemoteRevocationUnconfirmed;
                }
                let Ok(metadata) = response.json() else {
                    continue;
                };
                endpoint = metadata["revocation_endpoint"]
                    .as_str()
                    .and_then(https_endpoint);
            }
            let Some(endpoint) = &endpoint else {
                return RemoteRevocationUnconfirmed;
            };
            if signal.aborted() {
                return RemoteRevocationUnconfirmed;
            }
            let Ok(response) = transport
                .execute(
                    HttpRequest {
                        method: "POST".into(),
                        url: endpoint.as_str().into(),
                        headers: vec![(
                            "content-type".into(),
                            "application/x-www-form-urlencoded".into(),
                        )],
                        body: Some(body.clone()),
                        timeout_ms: None,
                    },
                    &signal,
                )
                .await
            else {
                continue;
            };
            match response.status {
                200 => return Revoked,
                status if status < 500 => return RemoteRevocationUnconfirmed,
                _ => {}
            }
        }
        RemoteRevocationUnconfirmed
    };
    tokio::select! {
        biased;
        () = signal.cancelled() => RemoteRevocationUnconfirmed,
        () = tokio::time::sleep(Duration::from_secs(15)) => RemoteRevocationUnconfirmed,
        result = operation => result,
    }
}

// An HTTP redirect must never forward a refresh token (notably on 307/308).
struct NoRedirectTransport(Result<reqwest::Client, reqwest::Error>);

impl Default for NoRedirectTransport {
    fn default() -> Self {
        Self(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build(),
        )
    }
}

#[async_trait]
impl OAuthTransport for NoRedirectTransport {
    async fn execute(
        &self,
        request: HttpRequest,
        signal: &AbortSignal,
    ) -> anyhow::Result<HttpResponse> {
        let client = self
            .0
            .as_ref()
            .map_err(|_| anyhow::anyhow!("Could not initialize OpenAI OAuth transport"))?;
        race_with_abort_signal(
            async {
                let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
                let mut builder = client.request(method, request.url);
                for (name, value) in request.headers {
                    builder = builder.header(name, value);
                }
                if let Some(body) = request.body {
                    builder = builder.body(body);
                }
                if let Some(timeout_ms) = request.timeout_ms {
                    builder = builder.timeout(Duration::from_millis(timeout_ms));
                }
                let response = builder.send().await?;
                let status = response.status().as_u16();
                let headers = response
                    .headers()
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.to_string(),
                            value.to_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect();
                Ok(HttpResponse {
                    status,
                    headers,
                    body: response.text().await?,
                })
            },
            signal,
        )
        .await?
    }
}

pub fn openai_chatgpt_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAIChatGPTOAuth::new(Arc::new(
        NoRedirectTransport::default(),
    )))
}

#[cfg(test)]
#[path = "openai_chatgpt_logout_tests.rs"]
mod logout_tests;

#[cfg(test)]
#[path = "openai_chatgpt_oauth_tests.rs"]
mod oauth_tests;

#[cfg(test)]
mod tests {
    use super::super::transport::{ScriptedResponse, ScriptedTransport};
    use super::*;

    pub(super) fn token() -> Value {
        json!({"access_token":"access", "refresh_token":"refresh", "expires_in":3600,
            "id_token":"id", "scope":"openid chatgpt.tokens.use.direct"})
    }

    #[test]
    fn dynamic_registration_binds_host_pkce_nonce_and_resource() -> anyhow::Result<()> {
        let host = agent_host_id(Some("ABCDEF00-1234-4678-90AB-ABCDEF123456"))?;
        let url = authorization_url(
            &host,
            "challenge",
            "state",
            "nonce",
            REDIRECT_URI,
            None,
            None,
        )?;
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query["client_id"], DYNAMIC_CLIENT_ID);
        assert_eq!(
            query["ext_agent_host_id"],
            "urn:uuid:abcdef00-1234-4678-90ab-abcdef123456"
        );
        assert_eq!(query["resource"], RESOURCE);
        assert_eq!(query["code_challenge"], "challenge");
        assert_eq!(query["nonce"], "nonce");
        assert!(
            query["scope"]
                .split_whitespace()
                .any(|s| s == DIRECT_TOKEN_SCOPE)
        );
        assert!(agent_host_id(None).is_err());
        assert!(agent_host_id(Some("abcdef001234567890ababcdef123456")).is_err());
        Ok(())
    }

    #[test]
    fn manual_callback_requires_exact_origin_state_and_issued_client() -> anyhow::Result<()> {
        let result = authorization_result_from_manual_input(
            &format!("{REDIRECT_URI}?code=c&state=s&client_id=issued"),
            "s",
            REDIRECT_URI,
            None,
        )?;
        assert_eq!(
            result,
            AuthorizationResult {
                code: "c".into(),
                client_id: "issued".into()
            }
        );
        for url in [
            format!("{REDIRECT_URI}?code=c&state=wrong&client_id=issued"),
            format!("{REDIRECT_URI}?code=c&state=s"),
            "http://localhost:1455/auth/callback?code=c&state=s&client_id=issued".into(),
            format!("{REDIRECT_URI}?error=access_denied"),
            "c".into(),
        ] {
            assert!(authorization_result_from_manual_input(&url, "s", REDIRECT_URI, None).is_err());
        }
        Ok(())
    }

    #[test]
    fn grants_preserve_plan_permission_and_require_valid_tokens() -> anyhow::Result<()> {
        let credential = credential_from_token_response(&token(), "issued", 1000.0)?;
        assert_eq!(credential.expires, 3_421_000.0);
        assert_eq!(credential.get_extra_str("clientId"), Some("issued"));
        let mut no_plan = token();
        no_plan["scope"] = json!("openid resource.invoke");
        let credential = credential_from_token_response(&no_plan, "issued", 1000.0)?;
        assert_eq!(
            credential.get_extra("planUsageEnabled"),
            Some(&json!(false))
        );
        for (field, value) in [
            ("scope", json!(" ")),
            ("access_token", json!(" ")),
            ("refresh_token", Value::Null),
            ("expires_in", json!(0)),
            ("expires_in", json!("3600")),
        ] {
            let mut invalid = token();
            invalid[field] = value;
            assert!(credential_from_token_response(&invalid, "issued", 1000.0).is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn exchange_and_refresh_use_issued_client_and_rotate_tokens() -> anyhow::Result<()> {
        let mut granted = token();
        granted["id_token"] = json!(super::super::openai_id_token::test_support::signed_token(
            super::super::openai_id_token::test_support::claims("issued", "nonce"),
            json!({"alg":"RS256","kid":"test-key"}),
        ));
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                None,
                ScriptedResponse::Json {
                    status: 200,
                    body: granted,
                },
            ),
            (
                Some("openid-configuration"),
                super::super::openai_id_token::test_support::discovery(),
            ),
            (
                Some("/jwks"),
                super::super::openai_id_token::test_support::jwks("test-key"),
            ),
            (
                None,
                ScriptedResponse::Json {
                    status: 200,
                    body: json!({"access_token":"next", "refresh_token":"rotated", "expires_in":3600, "scope":DIRECT_TOKEN_SCOPE}),
                },
            ),
        ]));
        transport.set_now_ms(1000.0);
        let signal = AbortController::new().signal();
        let flow = OpenAIChatGPTOAuth::new(transport.clone());
        let first = flow
            .exchange_authorization_code(
                AuthorizationResult {
                    code: "code".into(),
                    client_id: "issued".into(),
                },
                "verifier",
                "nonce",
                REDIRECT_URI,
                &signal,
            )
            .await?;
        let next = flow.refresh(&first, &signal).await?;
        assert_eq!(next.access, "next");
        assert_eq!(next.refresh, "rotated");
        assert_eq!(next.get_extra_str("subject"), Some("test-subject"));
        assert_eq!(
            next.get_extra_str("idToken"),
            first.get_extra_str("idToken")
        );
        for request in transport
            .requests()
            .into_iter()
            .filter(|request| request.url == TOKEN_URL)
        {
            let fields =
                url::form_urlencoded::parse(request.body.as_deref().unwrap_or_default().as_bytes())
                    .into_owned()
                    .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(fields["client_id"], "issued");
            assert_eq!(fields["resource"], RESOURCE);
        }
        Ok(())
    }

    #[tokio::test]
    async fn token_endpoint_errors_preserve_http_status() {
        let transport = ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Text {
                status: 401,
                body: "denied".into(),
            },
        )]);
        let error = request_token(&transport, &[], &AbortController::new().signal())
            .await
            .expect_err("endpoint rejects");
        assert_eq!(
            error
                .downcast_ref::<OAuthTokenEndpointError>()
                .expect("typed HTTP error")
                .status,
            401
        );
    }

    #[tokio::test]
    async fn callback_server_ignores_invalid_state_then_delivers_issued_client()
    -> anyhow::Result<()> {
        let mut server = start_callback_server("expected".into(), "127.0.0.1", 0, None).await?;
        let base = format!("http://127.0.0.1:{}{CALLBACK_PATH}", server.port);
        let client = reqwest::Client::new();
        let exercise = async {
            let invalid = client
                .get(format!("{base}?code=c&state=wrong&client_id=issued"))
                .send()
                .await?;
            assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
            assert!(matches!(
                server.result.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            let (response, result) = tokio::join!(
                client
                    .get(format!("{base}?code=c&state=expected&client_id=issued"))
                    .send(),
                server.result.recv(),
            );
            assert_eq!(response?.status(), reqwest::StatusCode::OK);
            let result = result.ok_or_else(|| anyhow::anyhow!("callback channel closed"))??;
            assert_eq!(result.client_id, "issued");
            assert_eq!(result.code, "c");
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), exercise).await??;
        Ok(())
    }

    #[test]
    fn callback_uri_uses_the_requested_host() {
        assert_eq!(
            callback_uri("localhost", 1455).unwrap(),
            "http://localhost:1455/auth/callback"
        );
        assert_eq!(
            callback_uri("::1", 1455).unwrap(),
            "http://[::1]:1455/auth/callback"
        );
        assert!(callback_uri("user@localhost", 1455).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn callback_server_binds_the_requested_loopback_address() -> anyhow::Result<()> {
        let mut server = start_callback_server("expected".into(), "127.0.0.2", 0, None).await?;
        let uri = callback_uri("127.0.0.2", server.port)?;
        let client = reqwest::Client::builder().no_proxy().build()?;
        let (response, result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                client
                    .get(format!("{uri}?code=c&state=expected&client_id=issued"))
                    .send(),
                server.result.recv(),
            )
        })
        .await?;
        assert_eq!(response?.status(), reqwest::StatusCode::OK);
        assert_eq!(
            result
                .ok_or_else(|| anyhow::anyhow!("callback closed"))??
                .code,
            "c"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_guard_aborts_prompt_when_login_future_is_dropped() {
        let guard = ManualPromptCancellation(AbortController::new());
        let signal = guard.0.signal();
        let cancelled = signal.cancelled();
        tokio::pin!(cancelled);
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(1), cancelled)
            .await
            .expect("prompt cancelled");
    }

    #[tokio::test]
    async fn exchange_rejects_missing_id_token_and_refresh_rejects_missing_client() {
        let mut response = token();
        response["id_token"] = Value::Null;
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json {
                status: 200,
                body: response,
            },
        )]));
        let signal = AbortController::new().signal();
        assert!(
            OpenAIChatGPTOAuth::new(transport.clone())
                .exchange_authorization_code(
                    AuthorizationResult {
                        code: "c".into(),
                        client_id: "issued".into()
                    },
                    "v",
                    "n",
                    REDIRECT_URI,
                    &signal
                )
                .await
                .is_err()
        );
        assert!(
            OpenAIChatGPTOAuth::new(transport.clone())
                .refresh(&OAuthCredential::new("a", "r", 0.0), &signal)
                .await
                .is_err()
        );
        assert_eq!(transport.requests().len(), 1);
    }
}
