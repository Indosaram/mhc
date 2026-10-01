//! Port of senpi packages/ai/src/auth/oauth/anthropic-callback-listener.ts.

use crate::auth::oauth::error_details::format_error_details;
use crate::auth::oauth::loopback::{
    LoopbackFuture, LoopbackListener, LoopbackRequest, LoopbackResponse, bind_failure_code, bind_loopback,
};
use crate::auth::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub const PREFERRED_CALLBACK_PORT: u16 = 53692;
pub const CALLBACK_PATH: &str = "/callback";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackCode {
    pub code: String,
    pub state: String,
}

struct Shared {
    expected_state: String,
    settled: AtomicBool,
    code: watch::Sender<Option<Option<CallbackCode>>>,
}

impl Shared {
    fn settle(&self, value: Option<CallbackCode>) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.code.send(Some(value));
    }
}

pub struct CallbackListener {
    pub port: Option<u16>,
    pub redirect_uri: String,
    pub callback_unavailable: Option<String>,
    shared: Arc<Shared>,
    code: watch::Receiver<Option<Option<CallbackCode>>>,
    listener: Mutex<Option<LoopbackListener>>,
    signal: Option<crate::utils::abort::AbortSignal>,
    listener_id: Option<crate::utils::abort::ListenerId>,
}

impl CallbackListener {
    pub async fn wait_for_code(&self) -> Option<CallbackCode> {
        let mut code = self.code.clone();
        loop {
            let current = { code.borrow().clone() };
            if let Some(current) = current {
                return current;
            }
            if code.changed().await.is_err() {
                return None;
            }
        }
    }

    pub fn cancel_wait(&self) {
        self.shared.settle(None);
    }

    pub fn close(&self) {
        if let (Some(signal), Some(listener_id)) = (&self.signal, self.listener_id) {
            signal.remove_abort_listener(listener_id);
        }
        if let Some(listener) = self.listener.lock().unwrap_or_else(|p| p.into_inner()).take() {
            listener.close();
        }
    }
}

pub fn callback_redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}{CALLBACK_PATH}")
}

fn foreign_login_html(request_url: &str) -> String {
    oauth_error_html(
        "This browser login belongs to a different session of this app, or to an earlier login attempt.",
        Some(
            &[
                "If a session is still waiting for this login (it shows a prompt to paste the redirect URL), copy the full address from the browser's address bar and paste it there.",
                "Otherwise this attempt is stale: close this tab and run the login again from the session that needs it.",
                "",
                request_url,
            ]
            .join("\n"),
        ),
    )
}

fn handle_callback(shared: &Shared, request: &LoopbackRequest) -> LoopbackResponse {
    if request.path != CALLBACK_PATH {
        return LoopbackResponse::html(404, oauth_error_html("Callback route not found.", None));
    }
    let code = request.query_param("code");
    let state = request.query_param("state");
    let error = request.query_param("error");
    if let Some(error) = error {
        return LoopbackResponse::html(
            400,
            oauth_error_html("Anthropic authentication did not complete.", Some(&format!("Error: {error}"))),
        );
    }
    let (Some(code), Some(state)) = (code, state) else {
        return LoopbackResponse::html(400, oauth_error_html("Missing code or state parameter.", None));
    };
    if state != shared.expected_state {
        return LoopbackResponse::html(400, foreign_login_html(&request.url));
    }
    shared.settle(Some(CallbackCode { code, state }));
    LoopbackResponse::html(200, oauth_success_html("Anthropic authentication completed. You can close this window."))
}

pub async fn start_callback_listener(expected_state: &str, host: &str) -> anyhow::Result<CallbackListener> {
    start_callback_listener_on_port(expected_state, host, PREFERRED_CALLBACK_PORT, None).await
}

pub async fn start_callback_listener_with_signal(
    expected_state: &str,
    host: &str,
    signal: Option<crate::utils::abort::AbortSignal>,
) -> anyhow::Result<CallbackListener> {
    start_callback_listener_on_port(expected_state, host, PREFERRED_CALLBACK_PORT, signal).await
}

pub async fn start_callback_listener_on_port(
    expected_state: &str,
    host: &str,
    preferred_port: u16,
    signal: Option<crate::utils::abort::AbortSignal>,
) -> anyhow::Result<CallbackListener> {
    let (code_tx, code_rx) = watch::channel(None);
    let shared = Arc::new(Shared {
        expected_state: expected_state.to_string(),
        settled: AtomicBool::new(false),
        code: code_tx,
    });

    let mut last_bind_failure: Option<&'static str> = None;
    for port in [preferred_port, 0] {
        let handler = {
            let shared = shared.clone();
            Arc::new(move |request: LoopbackRequest| -> LoopbackFuture {
                let response = handle_callback(&shared, &request);
                Box::pin(std::future::ready(response))
            })
        };
        match bind_loopback(port, host, handler).await {
            Ok(listener) => {
                let actual_port = listener.port;
                let abort_shared = shared.clone();
                let listener_id = signal.as_ref().map(|signal| {
                    
                    signal.add_abort_listener(move |_| abort_shared.settle(None))
                });
                if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                    shared.settle(None);
                }
                return Ok(CallbackListener {
                    port: Some(actual_port),
                    redirect_uri: callback_redirect_uri(actual_port),
                    callback_unavailable: None,
                    shared,
                    code: code_rx,
                    listener: Mutex::new(Some(listener)),
                    signal,
                    listener_id,
                });
            }
            Err(error) => {
                let Some(code) = bind_failure_code(&error) else {
                    anyhow::bail!(
                        "Could not open OAuth callback listener at {host}:{port}: {}",
                        format_error_details(&error)
                    );
                };
                last_bind_failure = Some(code);
            }
        }
    }

    Ok(CallbackListener {
        port: None,
        redirect_uri: callback_redirect_uri(preferred_port),
        callback_unavailable: Some(last_bind_failure.unwrap_or("EADDRINUSE").to_string()),
        shared,
        code: code_rx,
        listener: Mutex::new(None),
        signal: None,
        listener_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(url: &str) -> (u16, String) {
        let response = reqwest::get(url).await.expect("request");
        (response.status().as_u16(), response.text().await.expect("body"))
    }

    async fn listener() -> CallbackListener {
        start_callback_listener_on_port("verifier-state", "127.0.0.1", 0, None).await.expect("listener")
    }

    fn url(listener: &CallbackListener, query: &str) -> String {
        format!("{}?{query}", listener.redirect_uri)
    }

    #[test]
    fn preferred_callback_port_matches_senpi() {
        assert_eq!(PREFERRED_CALLBACK_PORT, 53692);
        assert_eq!(callback_redirect_uri(PREFERRED_CALLBACK_PORT), "http://localhost:53692/callback");
    }

    #[tokio::test]
    async fn resolves_the_code_and_state_from_a_valid_callback() {
        let listener = listener().await;
        assert!(listener.port.is_some());
        assert!(listener.callback_unavailable.is_none());

        let (status, body) = get(&url(&listener, "code=abc&state=verifier-state")).await;
        assert_eq!(status, 200);
        assert!(body.contains("Anthropic authentication completed. You can close this window."));
        assert_eq!(
            listener.wait_for_code().await,
            Some(CallbackCode { code: "abc".into(), state: "verifier-state".into() })
        );
        listener.close();
    }

    #[tokio::test]
    async fn rejects_a_foreign_state_with_the_stale_login_page() {
        let listener = listener().await;
        let (status, body) = get(&url(&listener, "code=abc&state=other")).await;
        assert_eq!(status, 400);
        assert!(body.contains("This browser login belongs to a different session of this app"));
        assert!(body.contains("code=abc"));
        listener.cancel_wait();
        assert_eq!(listener.wait_for_code().await, None);
        listener.close();
    }

    #[tokio::test]
    async fn reports_missing_parameters_and_unknown_routes() {
        let listener = listener().await;
        let (missing, body) = get(&url(&listener, "code=abc")).await;
        assert_eq!(missing, 400);
        assert!(body.contains("Missing code or state parameter."));

        let (not_found, body) = get(&listener.redirect_uri.replace("/callback", "/other")).await;
        assert_eq!(not_found, 404);
        assert!(body.contains("Callback route not found."));
        listener.close();
    }

    #[tokio::test]
    async fn reports_the_provider_error() {
        let listener = listener().await;
        let (status, body) = get(&url(&listener, "error=access_denied&state=verifier-state")).await;
        assert_eq!(status, 400);
        assert!(body.contains("Anthropic authentication did not complete."));
        assert!(body.contains("Error: access_denied"));
        listener.close();
    }

    #[tokio::test]
    async fn cancel_wait_settles_with_no_code() {
        let listener = listener().await;
        listener.cancel_wait();
        assert_eq!(listener.wait_for_code().await, None);
        listener.close();
    }

    #[tokio::test]
    async fn an_aborted_signal_settles_with_no_code() {
        let controller = crate::utils::abort::AbortController::new();
        let listener = start_callback_listener_with_signal("verifier-state", "127.0.0.1", Some(controller.signal()))
            .await
            .expect("listener");
        controller.abort(None);
        assert_eq!(listener.wait_for_code().await, None);
        listener.close();
    }

    #[tokio::test]
    async fn a_taken_preferred_port_falls_back_to_an_ephemeral_one() {
        let taken = listener().await;
        let taken_port = taken.port.expect("bound port");
        let second = start_callback_listener_on_port("verifier-state", "127.0.0.1", taken_port, None)
            .await
            .expect("second listener");
        assert_ne!(second.port, Some(taken_port));
        assert!(second.callback_unavailable.is_none());
        assert_ne!(second.redirect_uri, taken.redirect_uri);
        taken.close();
        second.close();
    }
}
