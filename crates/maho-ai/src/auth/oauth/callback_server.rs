//! Port of senpi packages/ai/src/auth/oauth/callback-server.ts.

use super::loopback::{
    LoopbackFuture, LoopbackHandler, LoopbackListener, LoopbackRequest, LoopbackResponse,
    bind_loopback,
};
use super::oauth_page::{oauth_error_html, oauth_success_html};
use crate::auth::types::{AuthPrompt, AuthPromptKind, ProviderAuthInteraction};
use crate::utils::abort::AbortController;
use crate::utils::abort::AbortSignal;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub const OAUTH_CANCEL_HEADER: &str = "x-oauth-callback-cancel";
pub type CompletionFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send>>;
pub type Complete<T> = Arc<dyn Fn(String) -> CompletionFuture<T> + Send + Sync>;

pub struct OAuthCallbackServerOptions<T> {
    pub provider_name: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub redirect_host: Option<String>,
    pub state: Option<String>,
    pub cancel_path: Option<String>,
    pub complete: Complete<T>,
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<u64>,
}

struct State<T> {
    claimed: bool,
    outcome: Option<Result<Option<T>, String>>,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    changed: watch::Sender<u64>,
    listener: Mutex<Option<LoopbackListener>>,
}

impl<T> Shared<T> {
    fn finish(&self, outcome: Result<Option<T>, String>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.outcome.is_none() {
            state.outcome = Some(outcome);
            self.changed.send_modify(|version| *version += 1);
        }
    }

    fn close_listener(&self) {
        if let Some(listener) = self
            .listener
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            listener.close();
        }
    }
}

pub struct OAuthCallbackServer<T> {
    pub redirect_uri: String,
    shared: Arc<Shared<T>>,
    lifecycle: Option<tokio::task::JoinHandle<()>>,
}

impl<T: Clone> OAuthCallbackServer<T> {
    pub async fn wait(&self) -> anyhow::Result<Option<T>> {
        let mut changed = self.shared.changed.subscribe();
        loop {
            let outcome = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .outcome
                .clone();
            if let Some(outcome) = outcome {
                return outcome.map_err(anyhow::Error::msg);
            }
            changed
                .changed()
                .await
                .map_err(|error| anyhow::anyhow!(error))?;
        }
    }

    pub fn cancel(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.claimed && state.outcome.is_none() {
            state.outcome = Some(Ok(None));
            self.shared.changed.send_modify(|version| *version += 1);
        }
    }

    pub fn close(&self) {
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.abort();
        }
        self.shared
            .finish(Err("OAuth callback server closed".into()));
        self.shared.close_listener();
    }
}

impl<T> Drop for OAuthCallbackServer<T> {
    fn drop(&mut self) {
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.abort();
        }
        self.shared.close_listener();
    }
}

pub enum CallbackOrManual<T> {
    Callback(T),
    Manual(String),
}

struct ManualAbort(AbortController);
impl Drop for ManualAbort {
    fn drop(&mut self) {
        self.0.abort(None);
    }
}

pub async fn wait_for_callback_or_manual_input<T: Clone>(
    interaction: &ProviderAuthInteraction,
    callback: Option<&OAuthCallbackServer<T>>,
    message: &str,
    placeholder: &str,
) -> anyhow::Result<CallbackOrManual<T>> {
    let abort = ManualAbort(AbortController::new());
    let manual = async {
        let result = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::ManualCode {
                    message: message.into(),
                    placeholder: Some(placeholder.into()),
                },
                signal: Some(abort.0.signal()),
            })
            .await;
        if let Some(callback) = callback {
            callback.cancel();
        }
        result
    };
    tokio::pin!(manual);
    let wait = async {
        if let Some(callback) = callback {
            tokio::select! {
                biased;
                result = callback.wait() => {
                    if let Some(value) = result? { return Ok(CallbackOrManual::Callback(value)); }
                    Ok(CallbackOrManual::Manual(manual.await?))
                }
                result = &mut manual => {
                    let input = result?;
                    if let Some(value) = callback.wait().await? { return Ok(CallbackOrManual::Callback(value)); }
                    Ok(CallbackOrManual::Manual(input))
                }
            }
        } else {
            Ok(CallbackOrManual::Manual(manual.await?))
        }
    };
    tokio::select! {
        biased;
        _ = interaction.signal.cancelled() => anyhow::bail!("Login cancelled"),
        result = wait => result,
    }
}

pub async fn start_oauth_callback_server<T: Clone + Send + 'static>(
    options: OAuthCallbackServerOptions<T>,
) -> anyhow::Result<OAuthCallbackServer<T>> {
    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        anyhow::bail!("Login cancelled");
    }
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            claimed: false,
            outcome: None,
        }),
        changed: watch::channel(0).0,
        listener: Mutex::new(None),
    });
    let options = Arc::new(options);
    let handler: LoopbackHandler = {
        let shared = shared.clone();
        let options = options.clone();
        Arc::new(move |request: LoopbackRequest| -> LoopbackFuture {
            let shared = shared.clone();
            let options = options.clone();
            Box::pin(async move {
                if options.cancel_path.as_deref() == Some(request.path.as_str()) {
                    if request.method != "POST"
                        || request.headers.get(OAUTH_CANCEL_HEADER).map(String::as_str) != Some("1")
                        || request.headers.contains_key("origin")
                    {
                        return LoopbackResponse::html(403, oauth_error_html("Not allowed.", None))
                            .no_store();
                    }
                    let release = {
                        let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                        if state.claimed {
                            false
                        } else {
                            if state.outcome.is_none() {
                                state.outcome = Some(Ok(None));
                            }
                            shared.changed.send_modify(|version| *version += 1);
                            true
                        }
                    };
                    if release {
                        let listener = shared
                            .listener
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .take();
                        if let Some(mut listener) = listener {
                            listener.close_and_wait().await;
                        }
                    }
                    return LoopbackResponse::html(
                        200,
                        oauth_error_html("This sign-in was replaced by a newer one.", None),
                    )
                    .no_store();
                }
                if request.method != "GET" || request.path != options.path {
                    return LoopbackResponse::html(
                        404,
                        oauth_error_html("Callback route not found.", None),
                    )
                    .no_store();
                }
                if options
                    .state
                    .as_ref()
                    .is_some_and(|state| request.query.get("state") != Some(state))
                {
                    return LoopbackResponse::html(400, oauth_error_html("State mismatch.", Some("This page belongs to a different sign-in than the one listening here. Copy this page's address and paste it into the session that is waiting for it, or start the sign-in again."))).no_store();
                }
                let code = {
                    let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                    if state.claimed || state.outcome.is_some() {
                        return LoopbackResponse::html(
                            409,
                            oauth_error_html("This sign-in has already been handled.", None),
                        )
                        .no_store();
                    }
                    if let Some(error) =
                        request.query.get("error").filter(|error| !error.is_empty())
                    {
                        let description = request.query.get("error_description").unwrap_or(error);
                        state.outcome = Some(Err(format!(
                            "{} authorization failed: {description}",
                            options.provider_name
                        )));
                        shared.changed.send_modify(|version| *version += 1);
                        return LoopbackResponse::html(
                            400,
                            oauth_error_html(
                                &format!("{} authorization failed.", options.provider_name),
                                Some(description),
                            ),
                        )
                        .no_store();
                    }
                    let Some(code) = request.query.get("code").filter(|code| !code.is_empty())
                    else {
                        return LoopbackResponse::html(
                            400,
                            oauth_error_html("Missing authorization code.", None),
                        )
                        .no_store();
                    };
                    state.claimed = true;
                    code.clone()
                };
                match (options.complete)(code).await {
                    Ok(value) => {
                        shared.finish(Ok(Some(value)));
                        LoopbackResponse::html(
                            200,
                            oauth_success_html(&format!(
                                "Signed in to {}. You may now close this page.",
                                options.provider_name
                            )),
                        )
                        .no_store()
                    }
                    Err(error) => {
                        let message = error.to_string();
                        shared.finish(Err(message.clone()));
                        LoopbackResponse::html(
                            502,
                            oauth_error_html(
                                &format!("{} sign-in failed.", options.provider_name),
                                Some(&message),
                            ),
                        )
                        .no_store()
                    }
                }
            })
        })
    };
    let listener = bind_loopback(options.port, &options.host, handler).await?;
    let host = options.redirect_host.as_deref().unwrap_or(&options.host);
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let redirect_uri = format!("http://{host}:{}{}", listener.port, options.path);
    *shared.listener.lock().unwrap_or_else(|p| p.into_inner()) = Some(listener);
    let lifecycle = if options.signal.is_some() || options.timeout_ms.is_some() {
        let shared = shared.clone();
        Some(tokio::spawn(async move {
            let abort = async {
                match &options.signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            let timeout = async {
                match options.timeout_ms {
                    Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = abort => shared.finish(Err("Login cancelled".into())),
                _ = timeout => shared.finish(Err(format!("{} sign-in timed out", options.provider_name))),
            }
        }))
    } else {
        None
    };
    Ok(OAuthCallbackServer {
        redirect_uri,
        shared,
        lifecycle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn server(complete: Complete<String>) -> OAuthCallbackServer<String> {
        start_oauth_callback_server(OAuthCallbackServerOptions {
            provider_name: "OpenAI".into(),
            host: "127.0.0.1".into(),
            port: 0,
            path: "/callback".into(),
            redirect_host: None,
            state: Some("expected".into()),
            cancel_path: Some("/cancel".into()),
            complete,
            signal: None,
            timeout_ms: None,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn rejects_cross_origin_takeover_and_foreign_state_then_accepts_one_code() {
        let server = server(Arc::new(|code| Box::pin(async move { Ok(code) }))).await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let cancel = server.redirect_uri.replace("/callback", "/cancel");
        for request in [
            client.get(&cancel),
            client.post(&cancel),
            client
                .post(&cancel)
                .header(OAUTH_CANCEL_HEADER, "1")
                .header("origin", "https://example.com"),
        ] {
            assert_eq!(request.send().await.unwrap().status().as_u16(), 403);
        }
        let foreign = client
            .get(format!("{}?state=foreign&code=wrong", server.redirect_uri))
            .send()
            .await
            .unwrap();
        assert_eq!(foreign.status().as_u16(), 400);
        assert!(foreign.text().await.unwrap().contains("different sign-in"));
        let valid = format!("{}?state=expected&code=right", server.redirect_uri);
        assert_eq!(
            client.get(&valid).send().await.unwrap().status().as_u16(),
            200
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server.wait())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("right")
        );
        assert_eq!(
            client.get(&valid).send().await.unwrap().status().as_u16(),
            409
        );
    }

    #[tokio::test]
    async fn local_takeover_releases_the_listener_and_cancels_wait() {
        let server = server(Arc::new(|code| Box::pin(async move { Ok(code) }))).await;
        let url = url::Url::parse(&server.redirect_uri).unwrap();
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .post(server.redirect_uri.replace("/callback", "/cancel"))
            .header(OAUTH_CANCEL_HEADER, "1")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), server.wait())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let rebound = tokio::net::TcpListener::bind(("127.0.0.1", url.port().unwrap()))
            .await
            .unwrap();
        drop(rebound);
    }

    #[tokio::test]
    async fn completion_failure_reaches_both_browser_and_waiter() {
        let server = server(Arc::new(|_| {
            Box::pin(async { anyhow::bail!("exchange failed") })
        }))
        .await;
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("{}?state=expected&code=code", server.redirect_uri))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 502);
        assert!(response.text().await.unwrap().contains("exchange failed"));
        assert_eq!(
            server.wait().await.unwrap_err().to_string(),
            "exchange failed"
        );
    }

    #[tokio::test]
    async fn claimed_typed_callback_survives_manual_cancel_and_rejects_duplicate() {
        // Given
        let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let exchange = Arc::new(Mutex::new(Some((claimed_tx, finish_rx))));
        let complete: Complete<String> = Arc::new(move |code| -> CompletionFuture<String> {
            let (claimed, finish) = exchange.lock().unwrap().take().unwrap();
            Box::pin(async move {
                claimed.send(code).unwrap();
                Ok(finish.await?)
            })
        });
        let server = server(complete).await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build().unwrap();
        let url = format!("{}?state=expected&code=first", server.redirect_uri);
        let request_client = client.clone();
        let request_url = url.clone();
        let first = tokio::spawn(async move { request_client.get(request_url).send().await });

        // When
        let claimed = tokio::time::timeout(Duration::from_secs(5), claimed_rx)
            .await.unwrap().unwrap();
        let duplicate = client.get(url).send().await.unwrap();
        server.cancel();
        finish_tx.send("completed".to_owned()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(5), first)
            .await.unwrap().unwrap().unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), server.wait())
            .await.unwrap().unwrap();

        // Then
        assert_eq!(claimed, "first");
        assert_eq!(duplicate.status().as_u16(), 409);
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(outcome.as_deref(), Some("completed"));
        server.close();
    }
}
