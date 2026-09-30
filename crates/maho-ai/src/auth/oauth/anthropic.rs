//! Port of senpi packages/ai/src/auth/oauth/anthropic.ts.

use crate::auth::oauth::anthropic_callback_listener::{PREFERRED_CALLBACK_PORT, start_callback_listener_with_signal};
use crate::auth::oauth::authorization_input::parse_authorization_input;
use crate::auth::oauth::error_details::format_error_details;
use crate::auth::oauth::loopback::callback_host;
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::transport::{HttpRequest, OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use crate::utils::abort::{AbortController, AbortSignal};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use url::Url;

const CLIENT_ID_BASE64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const LOGIN_IDLE_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const REQUEST_TIMEOUT_MS: u64 = 30_000;
const SCOPES: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

fn client_id() -> String {
    base64::engine::general_purpose::STANDARD.decode(CLIENT_ID_BASE64).unwrap_or_default().iter().map(|byte| *byte as char).collect()
}

async fn post_json(
    transport: &dyn OAuthTransport,
    url: &str,
    body: Value,
    signal: &AbortSignal,
) -> anyhow::Result<String> {
    let request = HttpRequest {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "application/json".into()),
        ],
        body: Some(body.to_string()),
        timeout_ms: Some(REQUEST_TIMEOUT_MS),
    };
    let response = transport.execute(request, signal).await?;
    if !response.ok() {
        anyhow::bail!("HTTP request failed. status={}; url={url}; body={}", response.status, response.body);
    }
    Ok(response.body)
}

fn credential_from_token_json(body: &str, now_ms: f64) -> anyhow::Result<OAuthCredential> {
    let parsed: Value = serde_json::from_str(body)?;
    let access = parsed.get("access_token").and_then(Value::as_str).unwrap_or_default().to_string();
    let refresh = parsed.get("refresh_token").and_then(Value::as_str).unwrap_or_default().to_string();
    let expires_in = parsed.get("expires_in").and_then(Value::as_f64).unwrap_or_default();
    Ok(OAuthCredential::new(access, refresh, now_ms + expires_in * 1000.0 - 5.0 * 60.0 * 1000.0))
}

async fn exchange_authorization_code(
    transport: &dyn OAuthTransport,
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": client_id(),
        "code": code,
        "state": state,
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    });
    let response_body = post_json(transport, TOKEN_URL, body, signal).await.map_err(|error| {
        anyhow::anyhow!(
            "Token exchange request failed. url={TOKEN_URL}; redirect_uri={redirect_uri}; response_type=authorization_code; details={}",
            format_error_details(error.as_ref())
        )
    })?;

    credential_from_token_json(&response_body, transport.now_ms()).map_err(|error| {
        anyhow::anyhow!(
            "Token exchange returned invalid JSON. url={TOKEN_URL}; body={response_body}; details={}",
            format_error_details(error.as_ref())
        )
    })
}

async fn refresh_anthropic_token(
    transport: &dyn OAuthTransport,
    refresh_token: &str,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": client_id(),
        "refresh_token": refresh_token,
    });
    let response_body = post_json(transport, TOKEN_URL, body, signal).await.map_err(|error| {
        anyhow::anyhow!(
            "Anthropic token refresh request failed. url={TOKEN_URL}; details={}",
            format_error_details(error.as_ref())
        )
    })?;

    credential_from_token_json(&response_body, transport.now_ms()).map_err(|error| {
        anyhow::anyhow!(
            "Anthropic token refresh returned invalid JSON. url={TOKEN_URL}; body={response_body}; details={}",
            format_error_details(error.as_ref())
        )
    })
}

fn login_timed_out_message() -> String {
    let minutes = LOGIN_IDLE_TIMEOUT_MS / 60_000;
    format!("Anthropic login timed out after {minutes} minutes without a browser callback or a pasted redirect URL. Run the login again.")
}

fn authorize_url(client_id: &str, redirect_uri: &str, challenge: &str, verifier: &str) -> String {
    let mut url = Url::parse(AUTHORIZE_URL).expect("static authorize url");
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("code", "true");
    serializer.append_pair("client_id", client_id);
    serializer.append_pair("response_type", "code");
    serializer.append_pair("redirect_uri", redirect_uri);
    serializer.append_pair("scope", SCOPES);
    serializer.append_pair("code_challenge", challenge);
    serializer.append_pair("code_challenge_method", "S256");
    serializer.append_pair("state", verifier);
    url.set_query(Some(&serializer.finish()));
    url.to_string()
}

enum ManualOutcome {
    Input,
    Error(String),
}

fn apply_manual_input(input: &str, verifier: &str, code: &mut Option<String>, state: &mut Option<String>) -> anyhow::Result<()> {
    let parsed = parse_authorization_input(input);
    if let Some(parsed_state) = &parsed.state
        && parsed_state != verifier {
            anyhow::bail!("OAuth state mismatch");
        }
    *code = parsed.code;
    *state = Some(parsed.state.unwrap_or_else(|| verifier.to_string()));
    Ok(())
}

pub struct AnthropicOAuth {
    transport: Arc<dyn OAuthTransport>,
}

impl AnthropicOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }

    async fn login_anthropic(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let pkce = generate_pkce();
        let listener = Arc::new(
            start_callback_listener_with_signal(&pkce.verifier, &callback_host(), Some(interaction.signal.clone()))
                .await?,
        );
        let manual_abort = AbortController::new();
        // Cancelling the login must release BOTH waits: the callback listener and the manual
        // prompt. In manual-only mode the prompt is the only thing keeping the login alive, so
        // leaving it open would hang cleanup.
        let abort_manual = manual_abort.clone();
        let abort_listener_id = interaction.signal.add_abort_listener(move |_| abort_manual.abort(None));
        if interaction.signal.aborted() {
            manual_abort.abort(None);
        }
        let timed_out = Arc::new(AtomicBool::new(false));
        let manual_input: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let manual_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let idle_listener = listener.clone();
        let idle_timed_out = timed_out.clone();
        let idle_abort = manual_abort.clone();
        let idle_timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(LOGIN_IDLE_TIMEOUT_MS)).await;
            idle_timed_out.store(true, Ordering::SeqCst);
            idle_listener.cancel_wait();
            idle_abort.abort(None);
        });

        let outcome = async {
            let client_id = client_id();
            let instructions = match &listener.callback_unavailable {
                Some(code) => format!(
                    "No local OAuth callback port could be opened (port {PREFERRED_CALLBACK_PORT} and an ephemeral port both failed: {code}). Complete login in your browser, then copy the final redirect URL from the address bar and paste it here."
                ),
                None => "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here.".to_string(),
            };
            interaction.notify(AuthEvent::AuthUrl {
                url: authorize_url(&client_id, &listener.redirect_uri, &pkce.challenge, &pkce.verifier),
                instructions: Some(instructions),
            });

            let prompt = AuthPrompt {
                kind: AuthPromptKind::ManualCode {
                    message: "Complete login in your browser, or paste the authorization code / redirect URL here:"
                        .into(),
                    placeholder: Some(listener.redirect_uri.clone()),
                },
                signal: Some(manual_abort.signal()),
            };

            let manual_listener = listener.clone();
            let input_slot = manual_input.clone();
            let error_slot = manual_error.clone();
            let manual = async move {
                match interaction.prompt(prompt).await {
                    Ok(input) => {
                        *input_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(input.clone());
                        manual_listener.cancel_wait();
                        ManualOutcome::Input
                    }
                    Err(error) => {
                        let message = error.to_string();
                        *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(message.clone());
                        manual_listener.cancel_wait();
                        ManualOutcome::Error(message)
                    }
                }
            };
            tokio::pin!(manual);

            let mut manual_settled: Option<ManualOutcome> = None;
            let mut callback_code = None;
            tokio::select! {
                biased;
                code = listener.wait_for_code() => callback_code = Some(code),
                settled = &mut manual => manual_settled = Some(settled),
            }

            if let Some(callback_code) = callback_code {
                if timed_out.load(Ordering::SeqCst) {
                    anyhow::bail!("{}", login_timed_out_message());
                }
                if let Some(error) = manual_error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                    anyhow::bail!("{error}");
                }
                if let Some(callback_code) = callback_code {
                    interaction.notify(AuthEvent::Progress {
                        message: "Exchanging authorization code for tokens...".into(),
                    });
                    return exchange_authorization_code(
                        self.transport.as_ref(),
                        &callback_code.code,
                        &callback_code.state,
                        &pkce.verifier,
                        &listener.redirect_uri,
                        &interaction.signal,
                    )
                    .await;
                }
            }

            let outcome = match manual_settled {
                Some(outcome) => outcome,
                None => manual.await,
            };
            if timed_out.load(Ordering::SeqCst) {
                anyhow::bail!("{}", login_timed_out_message());
            }
            if let ManualOutcome::Error(message) = outcome {
                anyhow::bail!("{message}");
            }
            if let Some(error) = manual_error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                anyhow::bail!("{error}");
            }

            let mut code = None;
            let mut state = None;
            if let Some(input) = manual_input.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                apply_manual_input(&input, &pkce.verifier, &mut code, &mut state)?;
            }

            let Some(code) = code else {
                anyhow::bail!("Missing authorization code");
            };
            let Some(state) = state else {
                anyhow::bail!("Missing OAuth state");
            };
            interaction.notify(AuthEvent::Progress {
                message: "Exchanging authorization code for tokens...".into(),
            });
            exchange_authorization_code(
                self.transport.as_ref(),
                &code,
                &state,
                &pkce.verifier,
                &listener.redirect_uri,
                &interaction.signal,
            )
            .await
        }
        .await;

        idle_timer.abort();
        interaction.signal.remove_abort_listener(abort_listener_id);
        manual_abort.abort(None);
        listener.close();
        outcome
    }
}

pub fn anthropic_oauth() -> Arc<dyn OAuthAuth> {
    static INSTANCE: LazyLock<Arc<dyn OAuthAuth>> = LazyLock::new(|| Arc::new(AnthropicOAuth::new(default_transport())));
    INSTANCE.clone()
}

#[async_trait]
impl OAuthAuth for AnthropicOAuth {
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.login_anthropic(interaction).await
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        refresh_anthropic_token(self.transport.as_ref(), &credential.refresh, signal).await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction, AuthPrompt, AuthPromptKind};
    use serde_json::Map;
    use std::time::Duration;
    use tokio::sync::Notify;

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    fn token_response() -> ScriptedResponse {
        json(
            200,
            serde_json::json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600 }),
        )
    }

    enum Answer {
        /// Answer the manual prompt with the redirect URL built from the auth-url event.
        FromAuthUrl,
        /// Answer the manual prompt with a bare code.
        Plain(String),
        /// Never answer; the prompt stays open until its signal aborts.
        WaitForAbort,
    }

    struct TestInteraction {
        signal: AbortSignal,
        answer: Answer,
        events: Arc<Mutex<Vec<AuthEvent>>>,
        opened: Arc<Notify>,
        prompt_signal: Arc<Mutex<Option<AbortSignal>>>,
    }

    #[async_trait]
    impl AuthInteraction for TestInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }

        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}

        async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
            if !matches!(prompt.kind, AuthPromptKind::ManualCode { .. }) {
                anyhow::bail!("Unexpected prompt");
            }
            *self.prompt_signal.lock().unwrap_or_else(|p| p.into_inner()) = prompt.signal.clone();
            self.opened.notify_one();
            match &self.answer {
                Answer::FromAuthUrl => {
                    let url = self
                        .events
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .iter()
                        .find_map(|event| match event {
                            AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                            _ => None,
                        })
                        .expect("auth url");
                    let parsed = Url::parse(&url)?;
                    let mut state = None;
                    let mut redirect = None;
                    for (key, value) in parsed.query_pairs() {
                        match key.as_ref() {
                            "state" => state = Some(value.into_owned()),
                            "redirect_uri" => redirect = Some(value.into_owned()),
                            _ => {}
                        }
                    }
                    let state = state.expect("state in auth url");
                    let redirect = redirect.expect("redirect_uri in auth url");
                    Ok(format!("{redirect}?code=manual-code&state={state}"))
                }
                Answer::Plain(code) => Ok(code.clone()),
                Answer::WaitForAbort => {
                    if let Some(signal) = &prompt.signal {
                        signal.cancelled().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                    anyhow::bail!("prompt aborted")
                }
            }
        }

        fn notify(&self, event: AuthEvent) {
            self.events.lock().unwrap_or_else(|p| p.into_inner()).push(event);
        }
    }

    struct Harness {
        interaction: ProviderAuthInteraction,
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompt_signal: Arc<Mutex<Option<AbortSignal>>>,
        opened: Arc<Notify>,
    }

    fn harness(signal: AbortSignal, answer: Answer) -> Harness {
        let events = Arc::new(Mutex::new(Vec::new()));
        let prompt_signal = Arc::new(Mutex::new(None));
        let opened = Arc::new(Notify::new());
        let inner = Arc::new(TestInteraction {
            signal: signal.clone(),
            answer,
            events: events.clone(),
            opened: opened.clone(),
            prompt_signal: prompt_signal.clone(),
        });
        Harness { interaction: ProviderAuthInteraction::new(signal, inner), events, prompt_signal, opened }
    }

    /// The Anthropic flow posts JSON bodies (post_json), matching the TS tests' getJsonBody.
    fn json_body(request: &HttpRequest) -> Map<String, Value> {
        let body = request.body.clone().unwrap_or_default();
        match serde_json::from_str::<Value>(&body) {
            Ok(Value::Object(object)) => object,
            _ => panic!("expected a JSON object body, got {body}"),
        }
    }

    fn body_str(request: &HttpRequest, key: &str) -> Option<String> {
        json_body(request).get(key).and_then(Value::as_str).map(str::to_string)
    }

    #[tokio::test]
    async fn keeps_the_localhost_redirect_uri_for_manual_callback_login() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some("/oauth/token"), token_response())]));
        let oauth = AnthropicOAuth::new(transport.clone());
        let harness = harness(AbortController::new().signal(), Answer::FromAuthUrl);

        let credentials = oauth.login(&harness.interaction).await.unwrap();

        assert_eq!(credentials.access, "access-token");
        assert_eq!(credentials.refresh, "refresh-token");
        let auth_url = harness
            .events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("auth url");
        let redirect_uri = Url::parse(&auth_url)
            .expect("url")
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .map(|(_, value)| value.into_owned())
            .expect("redirect_uri");
        assert!(redirect_uri.starts_with("http://localhost:"), "{redirect_uri}");
        assert!(redirect_uri.ends_with("/callback"), "{redirect_uri}");

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_str(&requests[0], "grant_type").as_deref(), Some("authorization_code"));
        assert_eq!(body_str(&requests[0], "code").as_deref(), Some("manual-code"));
        assert_eq!(body_str(&requests[0], "redirect_uri").as_deref(), Some(redirect_uri.as_str()));
    }

    #[tokio::test]
    async fn resolves_through_the_manual_code_prompt_and_aborts_it_after_settling() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some("/oauth/token"), token_response())]));
        let oauth = AnthropicOAuth::new(transport.clone());
        let harness = harness(AbortController::new().signal(), Answer::Plain("the-code".into()));

        let credential = oauth.login(&harness.interaction).await.unwrap();

        assert_eq!(credential.access, "access-token");
        assert!(
            harness.events.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|event| matches!(event, AuthEvent::AuthUrl { .. }))
        );
        let prompt_signal = harness.prompt_signal.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert!(prompt_signal.expect("manual prompt opened").aborted());
    }

    #[tokio::test]
    async fn aborts_a_manual_only_login_while_the_manual_prompt_is_still_open() {
        let transport = Arc::new(ScriptedTransport::default());
        let controller = AbortController::new();
        let harness = harness(controller.signal(), Answer::WaitForAbort);
        let interaction = harness.interaction;
        let opened = harness.opened.clone();
        let prompt_signal = harness.prompt_signal.clone();

        let login = tokio::spawn(async move { AnthropicOAuth::new(transport).login(&interaction).await });

        tokio::time::timeout(Duration::from_secs(10), opened.notified()).await.expect("prompt opened");
        controller.abort(None);

        let error = login.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "prompt aborted");
        let signal = prompt_signal.lock().unwrap_or_else(|p| p.into_inner()).clone().expect("prompt signal");
        assert!(signal.aborted());
    }

    /// Mirrors senpi's `__setAnthropicOAuthNodeApisForTests`: every listener bind on this thread
    /// fails with the given errno until the guard drops.
    struct ForcedBindFailure;

    impl ForcedBindFailure {
        fn new(errno: i32) -> Self {
            crate::auth::oauth::loopback::set_forced_bind_failure(Some(errno));
            Self
        }
    }

    impl Drop for ForcedBindFailure {
        fn drop(&mut self) {
            crate::auth::oauth::loopback::set_forced_bind_failure(None);
        }
    }

    async fn manual_fallback_with_failing_bind(errno: i32, code: &str) {
        let _forced = ForcedBindFailure::new(errno);
        let transport = Arc::new(ScriptedTransport::new(vec![(Some("/oauth/token"), token_response())]));
        let oauth = AnthropicOAuth::new(transport.clone());
        let harness = harness(AbortController::new().signal(), Answer::FromAuthUrl);

        let credentials = oauth.login(&harness.interaction).await.unwrap();

        assert_eq!(credentials.access, "access-token");
        let instructions = harness
            .events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { instructions, .. } => instructions.clone(),
                _ => None,
            })
            .expect("auth url instructions");
        assert!(instructions.contains("53692"), "{instructions}");
        assert!(instructions.contains(code), "{instructions}");
        assert!(instructions.to_lowercase().contains("redirect url"), "{instructions}");

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_str(&requests[0], "redirect_uri").as_deref(), Some("http://localhost:53692/callback"));
        assert_eq!(body_str(&requests[0], "code").as_deref(), Some("manual-code"));
    }

    #[tokio::test]
    async fn falls_back_to_manual_redirect_url_entry_when_the_callback_port_fails_with_eacces() {
        manual_fallback_with_failing_bind(13, "EACCES").await;
    }

    #[tokio::test]
    async fn falls_back_to_manual_redirect_url_entry_when_the_callback_port_fails_with_eaddrinuse() {
        manual_fallback_with_failing_bind(98, "EADDRINUSE").await;
    }

    #[tokio::test]
    async fn falls_back_to_manual_redirect_url_entry_when_the_callback_port_fails_with_eperm() {
        manual_fallback_with_failing_bind(1, "EPERM").await;
    }

    #[tokio::test]
    async fn rejects_non_bind_callback_errors_with_the_callback_host_and_port() {
        // EADDRNOTAVAIL is not a bind-failure code, so the listener must surface the raw error
        // naming the host and port instead of falling back to the manual prompt.
        let _forced = ForcedBindFailure::new(99);
        let oauth = AnthropicOAuth::new(Arc::new(ScriptedTransport::default()));
        let harness = harness(AbortController::new().signal(), Answer::WaitForAbort);

        let error = oauth.login(&harness.interaction).await.unwrap_err();

        assert!(error.to_string().contains("127.0.0.1:53692"), "{error}");
    }

    #[tokio::test]
    async fn omits_scope_from_refresh_token_requests() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/oauth/token"),
            json(
                200,
                serde_json::json!({ "access_token": "new-access-token", "refresh_token": "new-refresh-token", "expires_in": 3600 }),
            ),
        )]));
        transport.set_now_ms(1_700_000_000_000.0);
        let oauth = AnthropicOAuth::new(transport.clone());
        let signal = AbortController::new().signal();

        let credentials = oauth
            .refresh(&OAuthCredential::new("old-access-token", "refresh-token", 0.0), &signal)
            .await
            .unwrap();

        assert_eq!(credentials.access, "new-access-token");
        assert_eq!(credentials.refresh, "new-refresh-token");
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_str(&requests[0], "grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(body_str(&requests[0], "refresh_token").as_deref(), Some("refresh-token"));
        assert!(body_str(&requests[0], "client_id").is_some());
        assert!(!json_body(&requests[0]).contains_key("scope"));
    }

    fn redirect_uri_of(events: &Arc<Mutex<Vec<AuthEvent>>>) -> String {
        let url = events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("auth url");
        Url::parse(&url)
            .expect("url")
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .map(|(_, value)| value.into_owned())
            .expect("redirect_uri")
    }

    #[tokio::test]
    async fn keeps_two_concurrent_logins_in_one_process_independent() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/oauth/token"), token_response()),
            (Some("/oauth/token"), token_response()),
        ]));
        let first = harness(AbortController::new().signal(), Answer::Plain("code-first".into()));
        let second = harness(AbortController::new().signal(), Answer::Plain("code-second".into()));
        let first_events = first.events.clone();
        let second_events = second.events.clone();
        let first_interaction = first.interaction;
        let second_interaction = second.interaction;
        let first_oauth = AnthropicOAuth::new(transport.clone());
        let second_oauth = AnthropicOAuth::new(transport.clone());

        let (first_credential, second_credential) = tokio::join!(
            tokio::spawn(async move { first_oauth.login(&first_interaction).await }),
            tokio::spawn(async move { second_oauth.login(&second_interaction).await }),
        );

        assert_eq!(first_credential.unwrap().unwrap().access, "access-token");
        assert_eq!(second_credential.unwrap().unwrap().access, "access-token");

        let first_redirect = redirect_uri_of(&first_events);
        let second_redirect = redirect_uri_of(&second_events);
        assert_ne!(first_redirect, second_redirect, "each login owns its own loopback port");

        let exchanges: Vec<(String, String)> = transport
            .requests()
            .iter()
            .map(|request| {
                (body_str(request, "code").unwrap_or_default(), body_str(request, "redirect_uri").unwrap_or_default())
            })
            .collect();
        assert_eq!(exchanges.len(), 2);
        assert!(exchanges.contains(&("code-first".to_string(), first_redirect.clone())), "{exchanges:?}");
        assert!(exchanges.contains(&("code-second".to_string(), second_redirect.clone())), "{exchanges:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_an_idle_login_after_10_minutes_and_releases_its_listener() {
        let transport = Arc::new(ScriptedTransport::default());
        let harness = harness(AbortController::new().signal(), Answer::WaitForAbort);
        let events = harness.events.clone();
        let interaction = harness.interaction;
        let login = tokio::spawn(async move { AnthropicOAuth::new(transport).login(&interaction).await });

        // The outer bound is longer than the flow's 10-minute idle timer, so a broken timeout
        // fails the test instead of hanging it; the paused clock advances the timer itself.
        let error = tokio::time::timeout(Duration::from_secs(3600), login)
            .await
            .expect("the idle timer settles the login")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().to_lowercase().contains("timed out"), "{error}");

        let port = Url::parse(&redirect_uri_of(&events)).expect("url").port().expect("port");
        let mut released = false;
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                drop(listener);
                released = true;
                break;
            }
        }
        assert!(released, "the listener on port {port} must be released");
    }
}
