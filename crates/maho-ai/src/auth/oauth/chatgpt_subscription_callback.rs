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
                    if !error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::AddrInUse)
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
                                let response = reqwest::Client::new()
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
}
