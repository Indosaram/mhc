//! Shared loopback callback server for the OAuth flows.
//!
//! The TS callback listeners (`anthropic-callback-listener.ts`, `devin-callback.ts`,
//! `openrouter.ts`) each build a `node:http` server. Rust has no equivalent one-liner, so the
//! minimal HTTP/1.1 responder they need lives here and the three listeners drive it.

use crate::utils::provider_env::get_provider_env_value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct LoopbackRequest {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub url: String,
}

impl LoopbackRequest {
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query.get(name).cloned()
    }
}

#[derive(Debug, Clone)]
pub struct LoopbackResponse {
    pub status: u16,
    pub content_type: String,
    pub cache_control: Option<String>,
    pub body: String,
}

impl LoopbackResponse {
    pub fn html(status: u16, body: String) -> Self {
        Self { status, content_type: "text/html; charset=utf-8".into(), cache_control: None, body }
    }

    pub fn plain(status: u16, body: String) -> Self {
        Self { status, content_type: "text/plain; charset=utf-8".into(), cache_control: None, body }
    }

    pub fn no_store(mut self) -> Self {
        self.cache_control = Some("no-store".into());
        self
    }
}

pub type LoopbackFuture = std::pin::Pin<Box<dyn std::future::Future<Output = LoopbackResponse> + Send>>;
pub type LoopbackHandler = Arc<dyn Fn(LoopbackRequest) -> LoopbackFuture + Send + Sync>;

pub fn html_handler(
    handler: impl Fn(LoopbackRequest) -> LoopbackResponse + Send + Sync + 'static,
) -> LoopbackHandler {
    Arc::new(move |request| Box::pin(std::future::ready(handler(request))))
}

pub struct LoopbackListener {
    pub port: u16,
    task: JoinHandle<()>,
}

impl LoopbackListener {
    pub fn callback_url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.port)
    }

    pub fn close(&self) {
        self.task.abort();
    }
}

impl Drop for LoopbackListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn callback_host() -> String {
    get_provider_env_value("PI_OAUTH_CALLBACK_HOST", None).unwrap_or_else(|| "127.0.0.1".into())
}

pub fn bind_failure_code(error: &std::io::Error) -> Option<&'static str> {
    match error.raw_os_error()? {
        13 => Some("EACCES"),
        98 => Some("EADDRINUSE"),
        1 => Some("EPERM"),
        _ => None,
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam mirroring senpi's `__setAnthropicOAuthNodeApisForTests`: forces every
    /// `bind_loopback` on this thread to fail with the given errno, so a listener's failing-bind
    /// fallbacks are reachable without a privileged or already-occupied port.
    static FORCED_BIND_FAILURE: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// Sets (or clears) the forced bind failure for the current thread and returns the previous value.
#[cfg(test)]
pub fn set_forced_bind_failure(errno: Option<i32>) -> Option<i32> {
    FORCED_BIND_FAILURE.with(|cell| cell.replace(errno))
}

pub async fn bind_loopback(port: u16, host: &str, handler: LoopbackHandler) -> std::io::Result<LoopbackListener> {
    #[cfg(test)]
    if let Some(errno) = FORCED_BIND_FAILURE.with(std::cell::Cell::get) {
        return Err(std::io::Error::from_raw_os_error(errno));
    }
    let listener = TcpListener::bind((host, port)).await?;
    let bound_port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match stream.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(read) => {
                            buffer.extend_from_slice(&chunk[..read]);
                            if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() > 16 * 1024 {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let head = String::from_utf8_lossy(&buffer).into_owned();
                let Some(request_line) = head.lines().next() else {
                    return;
                };
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let target = parts.next().unwrap_or("/").to_string();
                let (path, query) = match target.split_once('?') {
                    Some((path, query)) => (path.to_string(), parse_query(query)),
                    None => (target.clone(), BTreeMap::new()),
                };
                let response = handler(LoopbackRequest { method, path, query, url: target }).await;
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                    response.status,
                    status_text(response.status),
                    response.content_type,
                    response.body.len(),
                );
                if let Some(cache_control) = &response.cache_control {
                    head.push_str(&format!("cache-control: {cache_control}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(response.body.as_bytes()).await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Ok(LoopbackListener { port: bound_port, task })
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(url: &str) -> (u16, String) {
        let response = reqwest::get(url).await.expect("request");
        let status = response.status().as_u16();
        (status, response.text().await.expect("body"))
    }

    #[tokio::test]
    async fn serves_the_handler_response_for_a_get_request() {
        let listener = bind_loopback(
            0,
            "127.0.0.1",
            html_handler(|request: LoopbackRequest| {
                assert_eq!(request.method, "GET");
                assert_eq!(request.path, "/callback");
                assert_eq!(request.query_param("code").as_deref(), Some("abc"));
                LoopbackResponse::html(200, "hello".into())
            }),
        )
        .await
        .expect("bind");
        let (status, body) = get(&listener.callback_url("127.0.0.1", "/callback?code=abc")).await;
        assert_eq!(status, 200);
        assert_eq!(body, "hello");
    }

    #[tokio::test]
    async fn reports_the_os_bind_failure_code() {
        let listener = bind_loopback(0, "127.0.0.1", html_handler(|_| LoopbackResponse::plain(200, String::new())))
            .await
            .expect("bind");
        let error = TcpListener::bind(("127.0.0.1", listener.port)).await.expect_err("port already bound");
        assert_eq!(bind_failure_code(&error), Some("EADDRINUSE"));
    }
}
