//! Local fixture server for the producer-capture suites: the Rust form of the TS tests' mocked
//! SDK clients and `fetch` functions that answer with a fixed `Response` body.

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::http::{Response, StatusCode};
use axum::routing::any;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::types::{Context, Message, UserContent, UserMessage};

/// Serves `body` with `content_type` for every request until dropped.
pub(crate) struct FixtureServer {
    pub(crate) origin: String,
    handle: JoinHandle<()>,
}

impl FixtureServer {
    pub(crate) async fn start(content_type: &'static str, body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let addr: SocketAddr = listener.local_addr().expect("fixture addr");
        let router = Router::new().fallback(any(move || {
            let body = body.clone();
            async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .expect("fixture response")
            }
        }));
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router.into_make_service()).await;
        });
        Self {
            origin: format!("http://{addr}"),
            handle,
        }
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// `data: <json>\n\n` per chunk, then `data: [DONE]\n\n`.
pub(crate) fn sse_data(chunks: &[Value], done: bool) -> Vec<u8> {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    body.into_bytes()
}

/// `event: <type>\ndata: <json>\n\n` per frame.
pub(crate) fn sse_events(frames: &[Value]) -> Vec<u8> {
    let mut body = String::new();
    for frame in frames {
        body.push_str("event: ");
        body.push_str(frame["type"].as_str().unwrap_or_default());
        body.push_str("\ndata: ");
        body.push_str(&frame.to_string());
        body.push_str("\n\n");
    }
    body.into_bytes()
}

/// One AWS event-stream frame (`:event-type`, `:message-type: event`, JSON payload). The CRCs are
/// left zero: the Bedrock reader does not verify them.
pub(crate) fn aws_event_frame(event_type: &str, payload: &Value) -> Vec<u8> {
    let mut headers = Vec::new();
    for (name, value) in [
        (":event-type", event_type),
        (":message-type", "event"),
        (":content-type", "application/json"),
    ] {
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }
    let payload = payload.to_string().into_bytes();
    let total = 12 + headers.len() + payload.len() + 4;
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&(total as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0; 4]);
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&[0; 4]);
    frame
}

/// `normalizeContext({ messages: [{ role: "user", content: "fixture", timestamp: 1 }] })`.
pub(crate) fn fixture_context() -> Context {
    Context {
        messages: vec![Message::User(UserMessage {
            content: UserContent::Text("fixture".into()),
            timestamp: 1,
        })],
        ..Context::default()
    }
}

/// `mintedIdentityKeys`: identity keys a producer must not mint on its terminal message.
pub(crate) fn minted_identity_keys(
    message: Option<&crate::types::AssistantMessage>,
) -> Vec<String> {
    let Some(message) = message else {
        return Vec::new();
    };
    let value = serde_json::to_value(message).expect("message json");
    ["messageId", "turnId", "runtimeItemKey"]
        .into_iter()
        .filter(|key| value.get(*key).is_some())
        .map(str::to_owned)
        .collect()
}
