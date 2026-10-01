//! Port of senpi packages/ai/src/auth/oauth/openrouter.ts.

use crate::auth::oauth::loopback::{LoopbackRequest, LoopbackResponse, bind_loopback, callback_host};
use crate::auth::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::transport::{HttpRequest, OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use crate::utils::abort::{AbortController, AbortSignal};
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use url::Url;

const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const TOKEN_EXCHANGE_TIMEOUT_MS: u64 = 30_000;

fn parse_authorization_input(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(url) = Url::parse(value) {
        return url.query_pairs().find(|(key, _)| key == "code").map(|(_, value)| value.into_owned());
    }
    if value.contains("code=") {
        return url::form_urlencoded::parse(value.as_bytes())
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned());
    }
    Some(value.to_string())
}

fn error_detail(body: &Map<String, Value>) -> Option<String> {
    if let Some(Value::String(description)) = body.get("error_description") {
        return Some(description.clone());
    }
    if let Some(Value::String(message)) = body.get("message") {
        return Some(message.clone());
    }
    if let Some(Value::String(error)) = body.get("error") {
        return Some(error.clone());
    }
    if let Some(Value::Object(error)) = body.get("error")
        && let Some(Value::String(message)) = error.get("message") {
            return Some(message.clone());
        }
    None
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    signal: &AbortSignal,
    transport: &dyn OAuthTransport,
) -> anyhow::Result<OAuthCredential> {
    if signal.aborted() {
        anyhow::bail!("Login cancelled");
    }

    let request = HttpRequest {
        method: "POST".into(),
        url: TOKEN_URL.into(),
        headers: vec![
            ("accept".into(), "application/json".into()),
            ("content-type".into(), "application/json".into()),
        ],
        body: Some(
            serde_json::json!({ "code": code, "code_verifier": verifier, "code_challenge_method": "S256" })
                .to_string(),
        ),
        timeout_ms: None,
    };

    let response = tokio::select! {
        biased;
        () = signal.cancelled() => anyhow::bail!("Login cancelled"),
        () = tokio::time::sleep(Duration::from_millis(TOKEN_EXCHANGE_TIMEOUT_MS)) => {
            anyhow::bail!("OpenRouter OAuth token exchange timed out")
        }
        result = transport.execute(request, signal) => match result {
            Ok(response) => response,
            Err(error) => {
                if signal.aborted() {
                    anyhow::bail!("Login cancelled");
                }
                return Err(error);
            }
        },
    };

    let body = match serde_json::from_str::<Value>(&response.body) {
        Ok(Value::Object(object)) => object,
        Ok(_) => Map::new(),
        Err(_) => {
            if response.ok() {
                anyhow::bail!("OpenRouter OAuth returned invalid JSON");
            }
            Map::new()
        }
    };

    if !response.ok() {
        let detail = error_detail(&body);
        let suffix = detail.map(|detail| format!(": {detail}")).unwrap_or_default();
        anyhow::bail!("OpenRouter OAuth key exchange failed (HTTP {}){suffix}", response.status);
    }

    let key = match body.get("key") {
        Some(Value::String(key)) if !key.is_empty() => key.clone(),
        _ => anyhow::bail!("OpenRouter OAuth response carries no \"key\""),
    };

    Ok(OAuthCredential::new(key, "", 9_007_199_254_740_991.0))
}

#[derive(Debug, Clone)]
enum Outcome {
    Credential(Option<OAuthCredential>),
    Error(String),
}

struct CallbackShared {
    verifier: String,
    signal: AbortSignal,
    transport: Arc<dyn OAuthTransport>,
    callback_path: String,
    claimed: AtomicBool,
    settled: AtomicBool,
    outcome: watch::Sender<Option<Outcome>>,
}

impl CallbackShared {
    fn finish(&self, outcome: Outcome) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.outcome.send(Some(outcome));
    }

    fn cancel_wait(&self) {
        if !self.claimed.load(Ordering::SeqCst) {
            self.finish(Outcome::Credential(None));
        }
    }
}

pub struct OpenRouterCallbackServer {
    pub callback_url: String,
    shared: Arc<CallbackShared>,
    outcome: watch::Receiver<Option<Outcome>>,
    listener: Mutex<Option<crate::auth::oauth::loopback::LoopbackListener>>,
    timeout_task: tokio::task::JoinHandle<()>,
    signal: AbortSignal,
    listener_id: crate::utils::abort::ListenerId,
}

impl OpenRouterCallbackServer {
    pub fn close(&self) {
        self.timeout_task.abort();
        self.signal.remove_abort_listener(self.listener_id);
        if let Some(listener) = self.listener.lock().unwrap_or_else(|p| p.into_inner()).take() {
            listener.close();
        }
    }

    pub fn cancel_wait(&self) {
        self.shared.cancel_wait();
    }

    pub async fn wait_for_credential(&self) -> anyhow::Result<Option<OAuthCredential>> {
        let mut outcome = self.outcome.clone();
        loop {
            let current = { outcome.borrow().clone() };
            if let Some(current) = current {
                return match current {
                    Outcome::Credential(credential) => Ok(credential),
                    Outcome::Error(message) => Err(anyhow::anyhow!("{message}")),
                };
            }
            outcome.changed().await?;
        }
    }
}

async fn start_callback_server(
    callback_path: &str,
    verifier: &str,
    signal: &AbortSignal,
    transport: Arc<dyn OAuthTransport>,
) -> anyhow::Result<OpenRouterCallbackServer> {
    if signal.aborted() {
        anyhow::bail!("Login cancelled");
    }
    let host = callback_host();
    let (outcome_tx, outcome_rx) = watch::channel(None);
    let shared = Arc::new(CallbackShared {
        verifier: verifier.to_string(),
        signal: signal.clone(),
        transport,
        callback_path: callback_path.to_string(),
        claimed: AtomicBool::new(false),
        settled: AtomicBool::new(false),
        outcome: outcome_tx,
    });

    let handler = {
        let shared = shared.clone();
        Arc::new(move |request: LoopbackRequest| -> crate::auth::oauth::loopback::LoopbackFuture {
            let shared = shared.clone();
            Box::pin(async move { handle_callback(&shared, request).await })
        })
    };

    let listener = bind_loopback(0, &host, handler).await?;
    let callback_url = format!("http://{host}:{}{callback_path}", listener.port);

    let abort_shared = shared.clone();
    let listener_id =
        signal.add_abort_listener(move |_| abort_shared.finish(Outcome::Error("Login cancelled".into())));
    if signal.aborted() {
        listener.close();
        anyhow::bail!("Login cancelled");
    }

    let timeout_shared = shared.clone();
    let timeout_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(LOGIN_TIMEOUT_MS)).await;
        timeout_shared.finish(Outcome::Error("OpenRouter OAuth login timed out".into()));
    });

    Ok(OpenRouterCallbackServer {
        callback_url,
        shared,
        outcome: outcome_rx,
        listener: Mutex::new(Some(listener)),
        timeout_task,
        signal: signal.clone(),
        listener_id,
    })
}

async fn handle_callback(shared: &CallbackShared, request: LoopbackRequest) -> LoopbackResponse {
    if request.method != "GET" || request.path != shared.callback_path {
        return LoopbackResponse::html(404, oauth_error_html("OAuth callback route not found.", None)).no_store();
    }
    if shared.claimed.load(Ordering::SeqCst) || shared.settled.load(Ordering::SeqCst) {
        return LoopbackResponse::html(409, oauth_error_html("This OAuth callback has already been used.", None))
            .no_store();
    }

    if let Some(oauth_error) = request.query_param("error") {
        let description = request.query_param("error_description").unwrap_or_else(|| oauth_error.clone());
        shared.finish(Outcome::Error(format!("OpenRouter authorization failed: {description}")));
        return LoopbackResponse::html(400, oauth_error_html("OpenRouter authorization was denied.", Some(&description)))
            .no_store();
    }

    let Some(code) = request.query_param("code") else {
        return LoopbackResponse::html(400, oauth_error_html("OpenRouter returned no authorization code.", None))
            .no_store();
    };
    shared.claimed.store(true, Ordering::SeqCst);

    match exchange_authorization_code(&code, &shared.verifier, &shared.signal, shared.transport.as_ref()).await {
        Ok(credential) => {
            shared.finish(Outcome::Credential(Some(credential)));
            LoopbackResponse::html(200, oauth_success_html("Signed in to OpenRouter. You may now close this page."))
                .no_store()
        }
        Err(error) => {
            let message = error.to_string();
            shared.finish(Outcome::Error(message.clone()));
            LoopbackResponse::html(502, oauth_error_html("OpenRouter key exchange failed.", Some(&message))).no_store()
        }
    }
}

fn authorize_url(callback_url: &str, challenge: &str) -> String {
    let mut url = Url::parse(AUTHORIZE_URL).expect("static authorize url");
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("callback_url", callback_url);
    serializer.append_pair("code_challenge", challenge);
    serializer.append_pair("code_challenge_method", "S256");
    url.set_query(Some(&serializer.finish()));
    url.to_string()
}

enum ManualOutcome {
    Input(String),
    Error(String),
}

pub struct OpenRouterOAuth {
    transport: Arc<dyn OAuthTransport>,
}

impl OpenRouterOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }

    async fn login_open_router(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let pkce = generate_pkce();
        let callback_path = format!("/oauth/callback/{}", uuid::Uuid::new_v4());
        let callback = Arc::new(
            start_callback_server(&callback_path, &pkce.verifier, &interaction.signal, self.transport.clone())
                .await?,
        );
        let manual_abort = AbortController::new();
        let manual_input: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let manual_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let outcome = async {
            let authorize_url = authorize_url(&callback.callback_url, &pkce.challenge);

            interaction.notify(AuthEvent::Progress {
                message: format!("Listening for OpenRouter OAuth callback on {}", callback.callback_url),
            });
            interaction.notify(AuthEvent::AuthUrl {
                url: authorize_url,
                instructions: Some(
                    "Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here."
                        .into(),
                ),
            });

            let prompt = AuthPrompt {
                kind: AuthPromptKind::ManualCode {
                    message: "Complete sign-in in your browser, or paste the authorization code / redirect URL here:"
                        .into(),
                    placeholder: Some(callback.callback_url.clone()),
                },
                signal: Some(manual_abort.signal()),
            };

            let manual_callback = callback.clone();
            let input_slot = manual_input.clone();
            let error_slot = manual_error.clone();
            let manual = async move {
                match interaction.prompt(prompt).await {
                    Ok(input) => {
                        *input_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(input.clone());
                        manual_callback.cancel_wait();
                        ManualOutcome::Input(input)
                    }
                    Err(error) => {
                        let message = error.to_string();
                        *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(message.clone());
                        manual_callback.cancel_wait();
                        ManualOutcome::Error(message)
                    }
                }
            };
            tokio::pin!(manual);

            let mut manual_settled: Option<ManualOutcome> = None;
            let mut callback_result: Option<anyhow::Result<Option<OAuthCredential>>> = None;
            tokio::select! {
                biased;
                result = callback.wait_for_credential() => callback_result = Some(result),
                settled = &mut manual => manual_settled = Some(settled),
            }

            let outcome = match manual_settled {
                Some(outcome) => outcome,
                None => {
                    let credential = callback_result.expect("the callback branch settled")?;
                    if let Some(error) = manual_error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                        anyhow::bail!("{error}");
                    }
                    match credential {
                        Some(credential) => return Ok(credential),
                        None => manual.await,
                    }
                }
            };

            match outcome {
                ManualOutcome::Error(message) => anyhow::bail!("{message}"),
                ManualOutcome::Input(input) => {
                    let Some(code) = parse_authorization_input(&input) else {
                        anyhow::bail!("Missing authorization code");
                    };
                    interaction.notify(AuthEvent::Progress {
                        message: "Exchanging authorization code for an API key...".into(),
                    });
                    exchange_authorization_code(&code, &pkce.verifier, &interaction.signal, self.transport.as_ref())
                        .await
                }
            }
        }
        .await;

        manual_abort.abort(None);
        callback.close();
        outcome
    }
}

pub fn open_router_oauth() -> Arc<dyn OAuthAuth> {
    static INSTANCE: LazyLock<Arc<dyn OAuthAuth>> =
        LazyLock::new(|| Arc::new(OpenRouterOAuth::new(default_transport())));
    INSTANCE.clone()
}

#[async_trait]
impl OAuthAuth for OpenRouterOAuth {
    fn name(&self) -> &str {
        "OpenRouter OAuth"
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with OpenRouter")
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.login_open_router(interaction).await
    }

    async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        Ok(credential.clone())
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{HttpResponse, ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction};
    use std::sync::atomic::AtomicUsize;

    const NATIVE_HOST: &str = "127.0.0.1";

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    fn token_response(key: &str) -> ScriptedResponse {
        json(200, serde_json::json!({ "key": key }))
    }

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

    fn base64url(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    enum PromptBehavior {
        /// Never answers; the loopback callback settles the login.
        Pending,
        /// Answers with the string built by the closure.
        Answer(Box<dyn Fn() -> String + Send + Sync>),
        /// Yields until the closure produces an answer; used when the answer depends on the
        /// auth-url event the notify handler has not delivered yet.
        AnswerWhenReady(Box<dyn Fn() -> Option<String> + Send + Sync>),
        /// Fails, as a cancelled prompt does.
        Fail(&'static str),
    }

    struct TestInteraction {
        signal: AbortSignal,
        prompt_behavior: PromptBehavior,
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompt_signals: Arc<Mutex<Vec<AbortSignal>>>,
        /// Every loopback callback request the notify handler fired, as (status, body).
        callbacks: Arc<Mutex<Vec<(u16, String)>>>,
        /// The callback URL captured from the auth-url event.
        callback_url: Arc<Mutex<Option<String>>>,
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
            if let Some(signal) = &prompt.signal {
                self.prompt_signals.lock().unwrap_or_else(|p| p.into_inner()).push(signal.clone());
            }
            match &self.prompt_behavior {
                PromptBehavior::Pending => std::future::pending::<anyhow::Result<String>>().await,
                PromptBehavior::Answer(build) => Ok(build()),
                PromptBehavior::AnswerWhenReady(build) => {
                    for _ in 0..100_000 {
                        if let Some(value) = build() {
                            return Ok(value);
                        }
                        tokio::task::yield_now().await;
                    }
                    anyhow::bail!("the manual answer never became ready")
                }
                PromptBehavior::Fail(message) => anyhow::bail!("{message}"),
            }
        }

        fn notify(&self, event: AuthEvent) {
            self.events.lock().unwrap_or_else(|p| p.into_inner()).push(event.clone());
            let AuthEvent::AuthUrl { url, .. } = event else {
                return;
            };
            let parsed = Url::parse(&url).expect("auth url");
            let callback_url = parsed
                .query_pairs()
                .find(|(key, _)| key == "callback_url")
                .map(|(_, value)| value.into_owned())
                .expect("callback_url in the auth url");
            *self.callback_url.lock().unwrap_or_else(|p| p.into_inner()) = Some(callback_url.clone());

            // The TS suite fires the loopback request from notify; do the same in the background.
            let callbacks = self.callbacks.clone();
            tokio::spawn(async move {
                let response = reqwest::get(format!("{callback_url}?code=authorization-code")).await;
                match response {
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let body = response.text().await.unwrap_or_default();
                        callbacks.lock().unwrap_or_else(|p| p.into_inner()).push((status, body));
                    }
                    Err(error) => {
                        callbacks.lock().unwrap_or_else(|p| p.into_inner()).push((0, error.to_string()));
                    }
                }
            });
        }
    }

    struct Harness {
        interaction: ProviderAuthInteraction,
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompt_signals: Arc<Mutex<Vec<AbortSignal>>>,
        callbacks: Arc<Mutex<Vec<(u16, String)>>>,
        callback_url: Arc<Mutex<Option<String>>>,
    }

    fn harness(signal: AbortSignal, prompt_behavior: PromptBehavior) -> Harness {
        harness_with(signal, prompt_behavior, Arc::new(Mutex::new(None)))
    }

    fn harness_with(
        signal: AbortSignal,
        prompt_behavior: PromptBehavior,
        callback_url: Arc<Mutex<Option<String>>>,
    ) -> Harness {
        let events = Arc::new(Mutex::new(Vec::new()));
        let prompt_signals = Arc::new(Mutex::new(Vec::new()));
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::new(TestInteraction {
            signal: signal.clone(),
            prompt_behavior,
            events: events.clone(),
            prompt_signals: prompt_signals.clone(),
            callbacks: callbacks.clone(),
            callback_url: callback_url.clone(),
        });
        Harness {
            interaction: ProviderAuthInteraction::new(signal, inner),
            events,
            prompt_signals,
            callbacks,
            callback_url,
        }
    }

    fn auth_url(events: &Arc<Mutex<Vec<AuthEvent>>>) -> Url {
        let url = events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("auth url event");
        Url::parse(&url).expect("parse auth url")
    }

    fn query_of(url: &Url, key: &str) -> Option<String> {
        url.query_pairs().find(|(name, _)| name == key).map(|(_, value)| value.into_owned())
    }

    /// Waits for a condition the test itself drives, bounded so a broken flow fails fast.
    async fn wait_for(mut condition: impl FnMut() -> bool) {
        for _ in 0..10_000 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition never became true");
    }

    /// The callback request runs in a spawned task; wait until it has been recorded.
    async fn callbacks_of(callbacks: &Arc<Mutex<Vec<(u16, String)>>>, expected: usize) -> Vec<(u16, String)> {
        let probe = callbacks.clone();
        wait_for(move || probe.lock().unwrap_or_else(|p| p.into_inner()).len() >= expected).await;
        callbacks.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A transport that parks the exchange until the test releases it, so a second callback
    /// arrives while the first exchange is still in flight.
    struct GatedTransport {
        started: AtomicUsize,
        release: tokio::sync::Notify,
        response: HttpResponse,
    }

    impl GatedTransport {
        fn new(response: HttpResponse) -> Self {
            Self { started: AtomicUsize::new(0), release: tokio::sync::Notify::new(), response }
        }
    }

    #[async_trait]
    impl OAuthTransport for GatedTransport {
        async fn execute(&self, _request: HttpRequest, _signal: &AbortSignal) -> anyhow::Result<HttpResponse> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            Ok(self.response.clone())
        }
    }

    fn ok_response(body: Value) -> HttpResponse {
        HttpResponse { status: 200, headers: Vec::new(), body: body.to_string() }
    }

    #[tokio::test]
    async fn runs_pkce_on_a_one_shot_loopback_callback_and_exchanges_the_code_for_a_permanent_api_key() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some(TOKEN_URL), token_response("sk-or-test"))]));
        let oauth = OpenRouterOAuth::new(transport.clone());
        let harness = harness(AbortController::new().signal(), PromptBehavior::Pending);

        let credential = oauth.login(&harness.interaction).await.unwrap();

        assert_eq!(credential.access, "sk-or-test");
        assert_eq!(credential.refresh, "");
        assert_eq!(credential.expires, 9_007_199_254_740_991.0);
        assert_eq!(credential.get_extra("env"), None);

        let callbacks = callbacks_of(&harness.callbacks, 1).await;
        assert_eq!(callbacks.len(), 1, "{callbacks:?}");
        assert_eq!(callbacks[0].0, 200);
        assert!(callbacks[0].1.contains("Signed in to OpenRouter"), "{}", callbacks[0].1);

        let signals = harness.prompt_signals.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(signals.len(), 1);
        assert!(signals[0].aborted(), "the manual prompt signal is aborted once login settles");

        let authorize_url = auth_url(&harness.events);
        assert_eq!(authorize_url.origin().ascii_serialization(), "https://openrouter.ai");
        assert_eq!(authorize_url.path(), "/auth");
        assert_eq!(query_of(&authorize_url, "code_challenge_method").as_deref(), Some("S256"));

        let callback = Url::parse(&query_of(&authorize_url, "callback_url").expect("callback_url")).expect("url");
        assert_eq!(callback.host_str(), Some(NATIVE_HOST));
        let path = callback.path().to_string();
        assert!(path.starts_with("/oauth/callback/"), "{path}");
        assert_eq!(path.rsplit('/').next().map(str::len), Some(36), "a uuid path segment: {path}");

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_str(&requests[0], "code").as_deref(), Some("authorization-code"));
        assert_eq!(body_str(&requests[0], "code_challenge_method").as_deref(), Some("S256"));
        let verifier = body_str(&requests[0], "code_verifier").expect("code_verifier");
        use sha2::{Digest, Sha256};
        assert_eq!(query_of(&authorize_url, "code_challenge").as_deref(), Some(base64url(&Sha256::digest(verifier.as_bytes())).as_str()));
    }

    #[tokio::test]
    async fn reports_token_exchange_failures_through_both_the_callback_page_and_login() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some(TOKEN_URL),
            json(403, serde_json::json!({ "error": { "message": "invalid code" } })),
        )]));
        let oauth = OpenRouterOAuth::new(transport);
        let harness = harness(AbortController::new().signal(), PromptBehavior::Pending);

        let error = oauth.login(&harness.interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "OpenRouter OAuth key exchange failed (HTTP 403): invalid code");

        let callbacks = callbacks_of(&harness.callbacks, 1).await;
        assert_eq!(callbacks.len(), 1, "{callbacks:?}");
        assert_eq!(callbacks[0].0, 502);
        assert!(callbacks[0].1.contains("OpenRouter key exchange failed."), "{}", callbacks[0].1);
    }

    #[tokio::test]
    async fn allows_only_one_token_exchange_for_a_callback() {
        let transport = Arc::new(GatedTransport::new(ok_response(serde_json::json!({ "key": "sk-or-test" }))));
        let oauth = OpenRouterOAuth::new(transport.clone());
        let harness = harness(AbortController::new().signal(), PromptBehavior::Pending);
        let interaction = harness.interaction;
        let callbacks = harness.callbacks.clone();
        let callback_slot = harness.callback_url.clone();

        let login = tokio::spawn(async move { oauth.login(&interaction).await });

        wait_for(|| transport.started.load(Ordering::SeqCst) == 1).await;
        let callback_url = callback_slot.lock().unwrap_or_else(|p| p.into_inner()).clone().expect("callback url");
        let second = reqwest::get(format!("{callback_url}?code=authorization-code")).await.expect("second callback");
        assert_eq!(second.status().as_u16(), 409);
        assert_eq!(transport.started.load(Ordering::SeqCst), 1, "a second callback must not start another exchange");

        transport.release.notify_one();
        let credential = login.await.unwrap().unwrap();
        assert_eq!(credential.access, "sk-or-test");
        let callbacks = callbacks_of(&callbacks, 1).await;
        assert_eq!(callbacks.len(), 1, "{callbacks:?}");
        assert_eq!(callbacks[0].0, 200);
    }

    #[tokio::test]
    async fn rejects_a_successful_response_that_does_not_contain_a_key() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some(TOKEN_URL),
            json(200, serde_json::json!({ "user_id": "user-1" })),
        )]));
        let oauth = OpenRouterOAuth::new(transport);
        let harness = harness(AbortController::new().signal(), PromptBehavior::Pending);

        let error = oauth.login(&harness.interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "OpenRouter OAuth response carries no \"key\"");

        let callbacks = callbacks_of(&harness.callbacks, 1).await;
        assert_eq!(callbacks.len(), 1, "{callbacks:?}");
        assert_eq!(callbacks[0].0, 502);
    }

    #[tokio::test]
    async fn mints_a_key_from_a_pasted_redirect_url_when_the_loopback_callback_never_arrives() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some(TOKEN_URL), token_response("sk-or-manual"))]));
        let oauth = OpenRouterOAuth::new(transport.clone());
        let slot = Arc::new(Mutex::new(None));
        let build_slot = slot.clone();
        let harness = harness_with(
            AbortController::new().signal(),
            PromptBehavior::AnswerWhenReady(Box::new(move || {
                build_slot
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
                    .map(|url| format!("{url}?code=manual-code"))
            })),
            slot,
        );

        let credential = oauth.login(&harness.interaction).await.unwrap();

        assert_eq!(credential.access, "sk-or-manual");
        assert_eq!(credential.refresh, "");
        assert_eq!(credential.expires, 9_007_199_254_740_991.0);

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_str(&requests[0], "code").as_deref(), Some("manual-code"));
        assert_eq!(body_str(&requests[0], "code_challenge_method").as_deref(), Some("S256"));
    }

    #[tokio::test]
    async fn accepts_a_bare_authorization_code_from_the_manual_prompt() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some(TOKEN_URL), token_response("sk-or-manual"))]));
        let oauth = OpenRouterOAuth::new(transport.clone());
        let harness = harness(
            AbortController::new().signal(),
            PromptBehavior::Answer(Box::new(|| "  manual-code  ".to_string())),
        );

        let credential = oauth.login(&harness.interaction).await.unwrap();

        assert_eq!(credential.access, "sk-or-manual");
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(body_str(&requests[0], "code").as_deref(), Some("manual-code"));
    }

    #[tokio::test]
    async fn fails_login_when_the_manual_prompt_is_cancelled() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some(TOKEN_URL), token_response("sk-or-unexpected"))]));
        let oauth = OpenRouterOAuth::new(transport.clone());
        let harness = harness(AbortController::new().signal(), PromptBehavior::Fail("Login cancelled"));

        let error = oauth.login(&harness.interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
        assert!(transport.requests().is_empty(), "a cancelled manual prompt must not exchange a code");
    }

    #[tokio::test]
    async fn rejects_empty_manual_input_without_exchanging_a_code() {
        let transport = Arc::new(ScriptedTransport::new(vec![(Some(TOKEN_URL), token_response("sk-or-unexpected"))]));
        let oauth = OpenRouterOAuth::new(transport.clone());
        let harness =
            harness(AbortController::new().signal(), PromptBehavior::Answer(Box::new(|| "   ".to_string())));

        let error = oauth.login(&harness.interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Missing authorization code");
        assert!(transport.requests().is_empty(), "empty manual input must not exchange a code");
    }

    #[tokio::test]
    async fn closes_the_pending_callback_when_login_is_cancelled() {
        let transport = Arc::new(ScriptedTransport::default());
        let oauth = OpenRouterOAuth::new(transport);
        let controller = AbortController::new();
        let harness = harness(controller.signal(), PromptBehavior::Pending);
        let interaction = harness.interaction;
        let callback_url = harness.callback_url.clone();
        let abort = controller.clone();

        let login = tokio::spawn(async move { oauth.login(&interaction).await });
        wait_for(|| callback_url.lock().unwrap_or_else(|p| p.into_inner()).is_some()).await;
        abort.abort(None);

        let error = login.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");

        let url = callback_url.lock().unwrap_or_else(|p| p.into_inner()).clone().expect("callback url");
        let released = reqwest::get(format!("{url}?code=authorization-code")).await;
        assert!(released.is_err(), "the callback listener must be closed after cancellation");
    }

    #[tokio::test]
    async fn rejects_before_opening_a_callback_server_when_login_is_already_cancelled() {
        let transport = Arc::new(ScriptedTransport::default());
        let oauth = OpenRouterOAuth::new(transport);
        let controller = AbortController::new();
        controller.abort(None);
        let harness = harness(controller.signal(), PromptBehavior::Answer(Box::new(String::new)));

        let error = oauth.login(&harness.interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
        assert!(
            harness.events.lock().unwrap_or_else(|p| p.into_inner()).is_empty(),
            "a cancelled login must not emit events"
        );
    }

    /// The TS case stubs PI_OAUTH_CALLBACK_HOST, which the crate-wide unsafe_code = "forbid"
    /// lint makes impossible to set from a test; this pins the default host the flow uses
    /// instead (get_provider_env_value's override branch is covered in utils::provider_env).
    #[tokio::test]
    async fn uses_the_configured_oauth_callback_host() {
        assert!(std::env::var_os("PI_OAUTH_CALLBACK_HOST").is_none(), "this test pins the default host");
        assert_eq!(callback_host(), NATIVE_HOST);

        let transport = Arc::new(ScriptedTransport::default());
        let oauth = OpenRouterOAuth::new(transport);
        let controller = AbortController::new();
        let harness = harness(controller.signal(), PromptBehavior::Pending);
        let interaction = harness.interaction;
        let callback_url = harness.callback_url.clone();
        let abort = controller.clone();

        let login = tokio::spawn(async move { oauth.login(&interaction).await });
        wait_for(|| callback_url.lock().unwrap_or_else(|p| p.into_inner()).is_some()).await;
        let url = Url::parse(&callback_url.lock().unwrap_or_else(|p| p.into_inner()).clone().expect("callback url"))
            .expect("url");
        assert_eq!(url.host_str(), Some(callback_host().as_str()));
        abort.abort(None);
        assert_eq!(login.await.unwrap().unwrap_err().to_string(), "Login cancelled");
    }
}
