//! Port of senpi packages/ai/src/auth/oauth/chatgpt-subscription-callback.ts.

use super::callback_server::{
    OAUTH_CANCEL_HEADER, OAuthCallbackServer, OAuthCallbackServerOptions,
    start_oauth_callback_server,
};
use crate::utils::abort::AbortSignal;
use std::sync::Arc;

pub const CHATGPT_CALLBACK_PORTS: &[u16] = &[1455, 1457];

pub async fn start_chat_gpt_callback(
    host: &str,
    state: &str,
    signal: &AbortSignal,
    ports: Option<&[u16]>,
) -> anyhow::Result<Option<OAuthCallbackServer<String>>> {
    let configured =
        crate::utils::provider_env::get_provider_env_value("PI_CHATGPT_OAUTH_CALLBACK_PORTS", None)
            .unwrap_or_default()
            .split(',')
            .filter_map(|value| value.trim().parse::<u16>().ok().filter(|port| *port > 0))
            .collect::<Vec<_>>();
    let ports = ports.unwrap_or_else(|| {
        if configured.is_empty() {
            CHATGPT_CALLBACK_PORTS
        } else {
            &configured
        }
    });
    let Some((&primary, fallbacks)) = ports.split_first() else {
        return Ok(None);
    };
    for (index, port) in std::iter::once(primary)
        .chain(fallbacks.iter().copied())
        .enumerate()
    {
        let attempts = if index == 0 { 10 } else { 1 };
        for attempt in 0..attempts {
            signal.throw_if_aborted()?;
            match start_oauth_callback_server(OAuthCallbackServerOptions {
                provider_name: "OpenAI".into(),
                host: host.into(),
                port,
                path: "/auth/callback".into(),
                redirect_host: Some("localhost".into()),
                state: Some(state.into()),
                cancel_path: Some("/cancel".into()),
                complete: Arc::new(|code| -> super::callback_server::CompletionFuture<String> {
                    Box::pin(async move { Ok(code) })
                }),
                signal: Some(signal.clone()),
                timeout_ms: None,
            })
            .await
            {
                Ok(server) => return Ok(Some(server)),
                Err(error) => {
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_none_or(|error| error.kind() != std::io::ErrorKind::AddrInUse)
                    {
                        return Ok(None);
                    }
                    if index == 0 {
                        if attempt == 0 {
                            let url = format!(
                                "http://{}:{port}/cancel",
                                if host.contains(':') {
                                    format!("[{host}]")
                                } else {
                                    host.to_owned()
                                }
                            );
                            let cancel = async {
                                let response = reqwest::Client::builder()
                                    .no_proxy()
                                    .redirect(reqwest::redirect::Policy::none())
                                    .build()?
                                    .post(url)
                                    .header(OAUTH_CANCEL_HEADER, "1")
                                    .timeout(std::time::Duration::from_secs(2))
                                    .send()
                                    .await?;
                                response.bytes().await
                            };
                            tokio::select! {
                                _ = signal.cancelled() => { signal.throw_if_aborted()?; }
                                _ = cancel => {}
                            }
                        }
                        crate::utils::sleep::sleep(200, signal).await?;
                    }
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;
    use std::time::Duration;
    use tokio::net::TcpListener;

    const HOST: &str = "127.0.0.1";

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
    }

    async fn callback(client: &reqwest::Client, port: u16, state: &str) -> reqwest::Response {
        client
            .get(format!(
                "http://{HOST}:{port}/auth/callback?code=the-code&state={state}"
            ))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn empty_port_list_uses_manual_only_mode() {
        let signal = crate::utils::abort::AbortController::new().signal();
        assert!(
            start_chat_gpt_callback("127.0.0.1", "state", &signal, Some(&[]))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn redirect_uri_names_the_port_that_was_bound() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let server = start_chat_gpt_callback("127.0.0.1", "state", &signal, Some(&[0]))
            .await
            .unwrap()
            .unwrap();
        let url = url::Url::parse(&server.redirect_uri).unwrap();
        assert_eq!(url.host_str(), Some("localhost"));
        assert_ne!(url.port().unwrap(), 0);
        assert_eq!(url.path(), "/auth/callback");
    }

    #[tokio::test]
    async fn takes_the_port_over_from_an_earlier_login() {
        let signal = AbortController::new().signal();
        let first = start_chat_gpt_callback(HOST, "first", &signal, Some(&[0]))
            .await
            .unwrap()
            .unwrap();
        let port = url::Url::parse(&first.redirect_uri)
            .unwrap()
            .port()
            .unwrap();
        let next = start_chat_gpt_callback(HOST, "second", &signal, Some(&[port, 0]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            next.redirect_uri,
            format!("http://localhost:{port}/auth/callback")
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), first.wait())
                .await
                .unwrap()
                .unwrap(),
            None
        );
        assert_eq!(callback(&client(), port, "second").await.status(), 200);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), next.wait())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("the-code")
        );
    }

    async fn stubborn_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind((HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    async fn reject_takeover(listener: &TcpListener) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"POST /cancel HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn falls_back_to_second_port_when_the_holder_will_not_let_go() {
        let (held, primary) = stubborn_listener().await;
        let signal = AbortController::new().signal();
        let (fallback_guard, fallback) = stubborn_listener().await;
        drop(fallback_guard);
        let ports = [primary, fallback];
        let (login, ()) = tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(
                start_chat_gpt_callback(HOST, "next", &signal, Some(&ports)),
                reject_takeover(&held)
            )
        })
        .await
        .unwrap();
        let login = login.unwrap().unwrap();
        assert_eq!(
            login.redirect_uri,
            format!("http://localhost:{fallback}/auth/callback")
        );
        assert_eq!(callback(&client(), fallback, "next").await.status(), 200);
    }

    #[tokio::test]
    async fn reports_no_listener_when_every_registered_port_is_held() {
        let (primary, a) = stubborn_listener().await;
        let (_fallback, b) = stubborn_listener().await;
        let signal = AbortController::new().signal();
        let ports = [a, b];
        let (login, ()) = tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(
                start_chat_gpt_callback(HOST, "next", &signal, Some(&ports)),
                reject_takeover(&primary)
            )
        })
        .await
        .unwrap();
        assert!(login.unwrap().is_none());
    }

    #[tokio::test]
    async fn refuses_browser_cancel_attempts_and_explains_a_foreign_state() {
        let signal = AbortController::new().signal();
        let server = start_chat_gpt_callback(HOST, "mine", &signal, Some(&[0]))
            .await
            .unwrap()
            .unwrap();
        let port = url::Url::parse(&server.redirect_uri)
            .unwrap()
            .port()
            .unwrap();
        let client = client();
        let cancel = format!("http://{HOST}:{port}/cancel");
        for request in [
            client.get(&cancel),
            client.post(&cancel),
            client
                .post(&cancel)
                .header(OAUTH_CANCEL_HEADER, "1")
                .header("origin", "https://example.test"),
        ] {
            assert_eq!(request.send().await.unwrap().status(), 403);
        }
        let foreign = callback(&client, port, "someone-else").await;
        assert_eq!(foreign.status(), 400);
        assert!(
            foreign
                .text()
                .await
                .unwrap()
                .contains("belongs to a different sign-in")
        );
        assert_eq!(callback(&client, port, "mine").await.status(), 200);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server.wait())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("the-code")
        );
    }
}
