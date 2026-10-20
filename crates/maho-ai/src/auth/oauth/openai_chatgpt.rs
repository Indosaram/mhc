//! Port of senpi packages/ai/src/auth/oauth/openai-chatgpt.ts.

use super::oauth_page::{oauth_error_html, oauth_success_html};
use super::pkce::generate_pkce;
use super::transport::{HttpRequest, OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};
use crate::utils::abort::{AbortController, AbortSignal};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
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
    #[cfg(test)]
    port: u16,
}

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
) -> anyhow::Result<AuthorizationResult> {
    let params = url.query_pairs().collect::<Vec<_>>();
    let get = |key| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
    };
    let code = get("code")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing authorization code"))?;
    let state = get("state")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Missing OAuth state"))?;
    if state != expected_state {
        anyhow::bail!("OAuth state mismatch");
    }
    let client_id = get("client_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
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
) -> anyhow::Result<AuthorizationResult> {
    let url = Url::parse(input.trim())
        .map_err(|_| anyhow::anyhow!("Paste the full callback URL from the browser"))?;
    let expected = Url::parse(REDIRECT_URI)?;
    if url.origin() != expected.origin() || url.path() != expected.path() {
        anyhow::bail!("The pasted callback URL must start with {REDIRECT_URI}");
    }
    if let Some((_, error)) = url
        .query_pairs()
        .find(|(key, value)| key == "error" && !value.is_empty())
    {
        anyhow::bail!("ChatGPT authorization failed: {error}");
    }
    authorization_result_from_callback(&url, state)
}

fn callback_response(
    url: &Url,
    state: &str,
) -> (u16, String, Option<anyhow::Result<AuthorizationResult>>) {
    if url.path() != CALLBACK_PATH {
        return (
            404,
            oauth_error_html("Callback route not found.", None),
            None,
        );
    }
    if let Some((_, error)) = url
        .query_pairs()
        .find(|(key, value)| key == "error" && !value.is_empty())
    {
        let message = format!("ChatGPT authorization failed: {error}");
        return (
            400,
            oauth_error_html(
                "ChatGPT was not connected.",
                Some(&format!("Error: {error}")),
            ),
            Some(Err(anyhow::anyhow!(message))),
        );
    }
    match authorization_result_from_callback(url, state) {
        Ok(result) => (
            200,
            oauth_success_html("ChatGPT authentication completed. You can close this window."),
            Some(Ok(result)),
        ),
        Err(error) => (400, oauth_error_html(&error.to_string(), None), None),
    }
}

async fn start_callback_server(state: String, port: u16) -> anyhow::Result<CallbackServer> {
    let listener = TcpListener::bind((super::loopback::callback_host().as_str(), port)).await?;
    #[cfg(test)]
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
                            Ok(url) => callback_response(&url, &state),
                            Err(_) => (500, oauth_error_html("Internal error while processing the callback.", None), None),
                        };
                        let reason = match status { 200 => "OK", 400 => "Bad Request", 404 => "Not Found", _ => "Internal Server Error" };
                        let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        if let Err(error) = stream.write_all(response.as_bytes()).await {
                            eprintln!("OAuth callback response write failed: {error}");
                        }
                        if let Some(outcome) = outcome && sender.send(outcome).is_err() { return; }
                    });
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = completed {
                        if sender.send(Err(error.into())).is_err() { return; }
                    }
                }
            }
        }
    });
    Ok(CallbackServer {
        task,
        result,
        #[cfg(test)]
        port,
    })
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI OAuth token request failed ({status}): {body}")]
pub struct OAuthTokenEndpointError {
    pub status: u16,
    pub body: String,
}

async fn request_token(
    transport: &dyn OAuthTransport,
    fields: &[(&str, &str)],
    signal: &AbortSignal,
) -> anyhow::Result<Value> {
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
                body: Some(
                    url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(fields.iter().copied())
                        .finish(),
                ),
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
    if !scopes.contains(&DIRECT_TOKEN_SCOPE) {
        anyhow::bail!("OpenAI OAuth grant did not include {DIRECT_TOKEN_SCOPE}");
    }
    Ok(OAuthCredential::new(
        access,
        refresh,
        now_ms + expires_in * 1000.0 - EXPIRY_MARGIN_MS,
    )
    .with_extra("clientId", json!(client_id))
    .with_extra("scopes", json!(scopes)))
}

async fn exchange_authorization_code(
    transport: &dyn OAuthTransport,
    result: AuthorizationResult,
    verifier: &str,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let token = request_token(
        transport,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &result.client_id),
            ("code", &result.code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
            ("resource", RESOURCE),
        ],
        signal,
    )
    .await?;
    if token["id_token"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty())
    {
        anyhow::bail!("OpenAI OAuth token response did not contain an ID token");
    }
    credential_from_token_response(&token, &result.client_id, transport.now_ms())
}

fn agent_host_id(device_id: Option<&str>) -> anyhow::Result<String> {
    let Some(device_id) = device_id.filter(|s| s.len() == 36) else {
        anyhow::bail!("Sign in with ChatGPT requires a device ID (UUID) for this installation");
    };
    if !device_id.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
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
) -> anyhow::Result<Url> {
    let mut url = Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", DYNAMIC_CLIENT_ID),
        ("agent_name_hint", "Pi"),
        ("ext_agent_host_id", host_id),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("resource", RESOURCE),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("nonce", nonce),
    ]);
    Ok(url)
}

pub struct OpenAIChatGPTOAuth {
    transport: Arc<dyn OAuthTransport>,
}
impl OpenAIChatGPTOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
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
        let host_id = agent_host_id(interaction.device_id())?;
        let pkce = generate_pkce();
        let state = generate_pkce().verifier;
        let nonce = generate_pkce().verifier;
        let mut callback = match start_callback_server(state.clone(), 1455).await {
            Ok(callback) => Some(callback),
            Err(error) => {
                interaction.notify(AuthEvent::Info { message: format!("Could not listen on {REDIRECT_URI}; paste the final redirect URL to continue. {error}"), links: None });
                None
            }
        };
        interaction.notify(AuthEvent::AuthUrl { url: authorization_url(&host_id, &pkce.challenge, &state, &nonce)?.into(),
            instructions: Some("Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here.".into()) });
        let manual_abort = ManualPromptCancellation(AbortController::new());
        let manual = async {
            let input = interaction
                .prompt(AuthPrompt {
                    kind: AuthPromptKind::ManualCode {
                        message:
                            "Complete login in your browser, or paste the final redirect URL here:"
                                .into(),
                        placeholder: Some(REDIRECT_URI.into()),
                    },
                    signal: Some(manual_abort.0.signal()),
                })
                .await?;
            authorization_result_from_manual_input(&input, &state)
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
            tokio::select! {
                biased;
                () = interaction.signal.cancelled() => anyhow::bail!("Login cancelled"),
                result = exchange_authorization_code(self.transport.as_ref(), authorization, &pkce.verifier, &interaction.signal) => result,
            }
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
        credential_from_token_response(&token, client_id, self.transport.now_ms())
    }
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..Default::default()
        })
    }
}

pub fn openai_chatgpt_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAIChatGPTOAuth::new(default_transport()))
}

#[cfg(test)]
mod tests {
    use super::super::transport::{ScriptedResponse, ScriptedTransport};
    use super::*;

    fn token() -> Value {
        json!({"access_token":"access", "refresh_token":"refresh", "expires_in":3600,
            "id_token":"id", "scope":"openid chatgpt.tokens.use.direct"})
    }

    #[test]
    fn dynamic_registration_binds_host_pkce_nonce_and_resource() -> anyhow::Result<()> {
        let host = agent_host_id(Some("ABCDEF00-1234-5678-90AB-ABCDEF123456"))?;
        let url = authorization_url(&host, "challenge", "state", "nonce")?;
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query["client_id"], DYNAMIC_CLIENT_ID);
        assert_eq!(
            query["ext_agent_host_id"],
            "urn:uuid:abcdef00-1234-5678-90ab-abcdef123456"
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
            assert!(authorization_result_from_manual_input(&url, "s").is_err());
        }
        Ok(())
    }

    #[test]
    fn grants_require_direct_scope_and_valid_tokens() -> anyhow::Result<()> {
        let credential = credential_from_token_response(&token(), "issued", 1000.0)?;
        assert_eq!(credential.expires, 3_421_000.0);
        assert_eq!(credential.get_extra_str("clientId"), Some("issued"));
        for (field, value) in [
            ("scope", json!("openid resource.invoke")),
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
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                None,
                ScriptedResponse::Json {
                    status: 200,
                    body: token(),
                },
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
        let first = exchange_authorization_code(
            transport.as_ref(),
            AuthorizationResult {
                code: "code".into(),
                client_id: "issued".into(),
            },
            "verifier",
            &signal,
        )
        .await?;
        let next = OpenAIChatGPTOAuth::new(transport.clone())
            .refresh(&first, &signal)
            .await?;
        assert_eq!(next.access, "next");
        assert_eq!(next.refresh, "rotated");
        for request in transport.requests() {
            assert_eq!(request.url, TOKEN_URL);
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
        let mut server = start_callback_server("expected".into(), 0).await?;
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
            exchange_authorization_code(
                transport.as_ref(),
                AuthorizationResult {
                    code: "c".into(),
                    client_id: "issued".into()
                },
                "v",
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
