//! Port of senpi packages/ai/src/auth/oauth/devin-callback.ts.

use crate::auth::oauth::devin_token::exchange_devin_authorization_code;
use crate::auth::oauth::loopback::{LoopbackListener, LoopbackRequest, LoopbackResponse, bind_loopback, callback_host};
use crate::auth::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use crate::auth::oauth::transport::{OAuthTransport, default_transport};
use crate::auth::types::OAuthCredential;
use crate::utils::abort::AbortSignal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub const DEVIN_CALLBACK_PORT: u16 = 59653;
pub const DEVIN_CALLBACK_PATH: &str = "/callback";

#[derive(Debug, Clone)]
enum Outcome {
    Credential(Option<OAuthCredential>),
    Error(String),
}

pub struct DevinCallbackInput {
    pub state: String,
    pub verifier: String,
    pub signal: AbortSignal,
    pub login_timeout_ms: u64,
}

struct Shared {
    state: String,
    verifier: String,
    signal: AbortSignal,
    transport: Arc<dyn OAuthTransport>,
    claimed: AtomicBool,
    settled: AtomicBool,
    outcome: watch::Sender<Option<Outcome>>,
}

impl Shared {
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

pub struct DevinCallbackServer {
    pub callback_url: String,
    shared: Arc<Shared>,
    outcome: watch::Receiver<Option<Outcome>>,
    listener: Mutex<Option<LoopbackListener>>,
    timeout_task: tokio::task::JoinHandle<()>,
    signal: AbortSignal,
    listener_id: crate::utils::abort::ListenerId,
}

impl DevinCallbackServer {
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

pub async fn start_devin_callback_server(input: DevinCallbackInput) -> anyhow::Result<DevinCallbackServer> {
    start_devin_callback_server_on_port(input, DEVIN_CALLBACK_PORT, default_transport()).await
}

pub async fn start_devin_callback_server_on_port(
    input: DevinCallbackInput,
    port: u16,
    transport: Arc<dyn OAuthTransport>,
) -> anyhow::Result<DevinCallbackServer> {
    if input.signal.aborted() {
        anyhow::bail!("Login cancelled");
    }
    let host = callback_host();
    let (outcome_tx, outcome_rx) = watch::channel(None);
    let shared = Arc::new(Shared {
        state: input.state,
        verifier: input.verifier,
        signal: input.signal.clone(),
        transport,
        claimed: AtomicBool::new(false),
        settled: AtomicBool::new(false),
        outcome: outcome_tx,
    });

    let handler = {
        let shared = shared.clone();
        Arc::new(
            move |request: LoopbackRequest| -> crate::auth::oauth::loopback::LoopbackFuture {
                let shared = shared.clone();
                Box::pin(async move { handle_callback(&shared, request).await })
            },
        )
    };

    let listener = bind_loopback(port, &host, handler).await?;
    let callback_url = format!("http://{host}:{}{DEVIN_CALLBACK_PATH}", listener.port);

    let abort_shared = shared.clone();
    let listener_id = input.signal.add_abort_listener(move |_| abort_shared.finish(Outcome::Error("Login cancelled".into())));
    if input.signal.aborted() {
        listener.close();
        anyhow::bail!("Login cancelled");
    }

    let timeout_shared = shared.clone();
    let timeout_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(input.login_timeout_ms)).await;
        timeout_shared.finish(Outcome::Error("Devin OAuth login timed out".into()));
    });

    Ok(DevinCallbackServer {
        callback_url,
        shared,
        outcome: outcome_rx,
        listener: Mutex::new(Some(listener)),
        timeout_task,
        signal: input.signal,
        listener_id,
    })
}

async fn handle_callback(shared: &Shared, request: LoopbackRequest) -> LoopbackResponse {
    if request.method != "GET" || request.path != DEVIN_CALLBACK_PATH {
        return LoopbackResponse::html(404, oauth_error_html("OAuth callback route not found.", None)).no_store();
    }
    if shared.claimed.load(Ordering::SeqCst) || shared.settled.load(Ordering::SeqCst) {
        return LoopbackResponse::html(409, oauth_error_html("This OAuth callback has already been used.", None))
            .no_store();
    }

    if let Some(oauth_error) = request.query_param("error") {
        let description = request.query_param("error_description").unwrap_or_else(|| oauth_error.clone());
        shared.finish(Outcome::Error(format!("Devin authorization failed: {description}")));
        return LoopbackResponse::html(
            400,
            oauth_error_html("Devin authorization was denied.", Some(&description)),
        )
        .no_store();
    }

    let state = request.query_param("state").unwrap_or_default();
    if state != shared.state {
        shared.finish(Outcome::Error("Devin OAuth callback state mismatch".into()));
        return LoopbackResponse::html(400, oauth_error_html("Devin authorization state mismatch.", None)).no_store();
    }

    let Some(code) = request.query_param("code") else {
        return LoopbackResponse::html(400, oauth_error_html("Devin returned no authorization code.", None)).no_store();
    };
    shared.claimed.store(true, Ordering::SeqCst);

    match exchange_devin_authorization_code(&code, &shared.verifier, &shared.signal, shared.transport.as_ref()).await {
        Ok(credential) => {
            shared.finish(Outcome::Credential(Some(credential)));
            LoopbackResponse::html(200, oauth_success_html("Signed in to Devin. You may now close this page.")).no_store()
        }
        Err(error) => {
            let message = error.to_string();
            shared.finish(Outcome::Error(message.clone()));
            LoopbackResponse::html(502, oauth_error_html("Devin token exchange failed.", Some(&message))).no_store()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::utils::abort::AbortController;
    use std::time::Duration;

    fn input(signal: AbortSignal, state: &str) -> DevinCallbackInput {
        DevinCallbackInput {
            state: state.into(),
            verifier: "verifier-1".into(),
            signal,
            login_timeout_ms: 5 * 60 * 1000,
        }
    }

    async fn get(url: &str) -> (u16, String) {
        let response = reqwest::get(url).await.expect("request");
        (response.status().as_u16(), response.text().await.expect("body"))
    }

    fn transport() -> Arc<ScriptedTransport> {
        Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "token": "devin-token" }) },
        )]))
    }

    #[tokio::test]
    async fn exchanges_the_code_and_returns_the_credential() {
        let controller = AbortController::new();
        let transport = transport();
        let server =
            start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport.clone())
                .await
                .expect("server");
        let (status, body) = get(&format!("{}?code=abc&state=state-1", server.callback_url)).await;
        assert_eq!(status, 200);
        assert!(body.contains("Signed in to Devin"));
        let credential = server.wait_for_credential().await.unwrap().expect("credential");
        assert_eq!(credential.access, "devin-token");
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test]
    async fn rejects_a_foreign_state_before_spending_the_code() {
        let controller = AbortController::new();
        let transport = transport();
        let server =
            start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport.clone())
                .await
                .expect("server");
        let (status, body) = get(&format!("{}?code=abc&state=other", server.callback_url)).await;
        assert_eq!(status, 400);
        assert!(body.contains("Devin authorization state mismatch"));
        assert!(transport.requests().is_empty());
        let error = server.wait_for_credential().await.unwrap_err();
        assert_eq!(error.to_string(), "Devin OAuth callback state mismatch");
    }

    #[tokio::test]
    async fn reports_a_denied_authorization() {
        let controller = AbortController::new();
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .expect("server");
        let (status, body) = get(&format!("{}?error=access_denied&state=state-1", server.callback_url)).await;
        assert_eq!(status, 400);
        assert!(body.contains("Devin authorization was denied."));
        assert!(body.contains("access_denied"));
        let error = server.wait_for_credential().await.unwrap_err();
        assert_eq!(error.to_string(), "Devin authorization failed: access_denied");
    }

    #[tokio::test]
    async fn reports_a_missing_code() {
        let controller = AbortController::new();
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .expect("server");
        let (status, body) = get(&format!("{}?state=state-1", server.callback_url)).await;
        assert_eq!(status, 400);
        assert!(body.contains("Devin returned no authorization code."));
    }

    #[tokio::test]
    async fn reports_a_missing_route() {
        let controller = AbortController::new();
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .expect("server");
        let base = server.callback_url.replace("/callback", "/other");
        let (status, body) = get(&base).await;
        assert_eq!(status, 404);
        assert!(body.contains("OAuth callback route not found."));
    }

    #[tokio::test]
    async fn a_second_callback_is_conflict() {
        let controller = AbortController::new();
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .expect("server");
        let url = format!("{}?code=abc&state=state-1", server.callback_url);
        let (first, _) = get(&url).await;
        assert_eq!(first, 200);
        let (second, body) = get(&url).await;
        assert_eq!(second, 409);
        assert!(body.contains("This OAuth callback has already been used."));
    }

    #[tokio::test]
    async fn cancel_wait_hands_the_login_to_manual_entry() {
        let controller = AbortController::new();
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .expect("server");
        server.cancel_wait();
        assert!(server.wait_for_credential().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn aborting_the_signal_fails_the_login() {
        let controller = AbortController::new();
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .expect("server");
        controller.abort(None);
        let error = server.wait_for_credential().await.unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
    }

    #[tokio::test]
    async fn refuses_to_start_with_an_aborted_signal() {
        let controller = AbortController::new();
        controller.abort(None);
        let error = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport())
            .await
            .err()
            .expect("must fail");
        assert_eq!(error.to_string(), "Login cancelled");
    }

    #[tokio::test(start_paused = true)]
    async fn the_login_timeout_fails_the_wait() {
        let controller = AbortController::new();
        let mut server_input = input(controller.signal(), "state-1");
        server_input.login_timeout_ms = 1000;
        let server = start_devin_callback_server_on_port(server_input, 0, transport()).await.expect("server");
        tokio::time::advance(Duration::from_millis(1000)).await;
        let error = server.wait_for_credential().await.unwrap_err();
        assert_eq!(error.to_string(), "Devin OAuth login timed out");
    }

    #[tokio::test]
    async fn a_failed_exchange_is_reported_to_the_browser() {
        let controller = AbortController::new();
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 400, body: serde_json::json!({ "error": "invalid_code" }) },
        )]));
        let server = start_devin_callback_server_on_port(input(controller.signal(), "state-1"), 0, transport)
            .await
            .expect("server");
        let (status, body) = get(&format!("{}?code=abc&state=state-1", server.callback_url)).await;
        assert_eq!(status, 502);
        assert!(body.contains("Devin token exchange failed."));
        assert!(body.contains("invalid_code"));
    }
}
