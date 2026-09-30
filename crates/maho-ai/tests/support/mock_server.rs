//! Local axum mock server for maho-ai wire-API replay tests (todos 10-12).
//!
//! Serves fixed bytes (the same recorded SSE/JSON bodies `tools/golden/ai-replay.mjs` drives the
//! pinned senpi `stream()` functions against) for every request, with an option to end the stream
//! early instead of sending the whole fixture. A wire-API test points a `Model::base_url`
//! at [`MockFixtureServer::base_url`] and then drives its `stream()`/`stream_simple()` against it,
//! matching the fixture used to produce `tools/golden/replay/<case>.json`.
//!
//! No maho-ai wire API exists yet (todos 10-12 own that), so this module's own tests only cover
//! its own contract: it serves bytes and it can end mid-stream. Once a wire API lands, its tests
//! use this server plus [`super::replay::assert_stream_matches_golden`] to compare the Rust event
//! sequence against the golden.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use axum::routing::any;
use axum::Router;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// One HTTP response to serve for every request the mock server receives.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub status: StatusCode,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Vec<u8>,
    /// When set, only the first this-many bytes of `body` are served and the stream then ends,
    /// with no terminator frame (the mid-stream-close case). This is the SSE transport EOF senpi's
    /// own `openai-completions-stream-lifecycle` test produces with `response.end()` after a
    /// partial write: a wire API must treat it as an error ("Stream ended without
    /// finish_reason"), not as a completed turn.
    pub close_after_bytes: Option<usize>,
}

impl Fixture {
    /// A `text/event-stream` fixture built from `(event, data)` SSE frames, joined the same way
    /// `tools/golden/ai-replay.mjs`'s `fixtureBytes` does: `event: <e>\ndata: <d>\n\n` per frame
    /// (the `event` line is omitted when `event` is `None`, matching a data-only SSE frame).
    pub fn sse<'a>(frames: impl IntoIterator<Item = (Option<&'a str>, &'a str)>) -> Self {
        let mut body = String::new();
        for (event, data) in frames {
            if let Some(event) = event {
                body.push_str("event: ");
                body.push_str(event);
                body.push('\n');
            }
            body.push_str("data: ");
            body.push_str(data);
            body.push_str("\n\n");
        }
        Self {
            status: StatusCode::OK,
            headers: vec![(
                HeaderName::from_static("content-type"),
                HeaderValue::from_static("text/event-stream"),
            )],
            body: body.into_bytes(),
            close_after_bytes: None,
        }
    }

    /// A plain JSON body fixture (e.g. a non-streaming error response).
    pub fn json(status: StatusCode, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![(
                HeaderName::from_static("content-type"),
                HeaderValue::from_static("application/json"),
            )],
            body: body.into().into_bytes(),
            close_after_bytes: None,
        }
    }

    /// Returns a copy that serves only the first `bytes` bytes and then ends the stream there
    /// instead of sending the rest (see [`Fixture::close_after_bytes`]).
    #[must_use]
    pub fn closed_after(mut self, bytes: usize) -> Self {
        self.close_after_bytes = Some(bytes);
        self
    }
}

/// A running mock server; drop it (or call [`MockFixtureServer::shutdown`]) to stop listening.
pub struct MockFixtureServer {
    base_url: String,
    handle: JoinHandle<()>,
}

impl MockFixtureServer {
    /// Starts a server on an OS-assigned localhost port that serves `fixture` for every request.
    pub async fn start(fixture: Fixture) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock fixture server");
        let addr: SocketAddr = listener.local_addr().expect("mock fixture server local addr");
        let router = Router::new().fallback(any(move || serve_fixture(fixture.clone())));
        let handle = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service())
                .await
                .expect("mock fixture server serve");
        });
        Self { base_url: format!("http://{addr}/v1"), handle }
    }

    /// Base URL to hand to `Model::base_url` (matches senpi's own `startServer` test helpers,
    /// which return `http://127.0.0.1:<port>/v1`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Aborts the server task. Also happens implicitly on drop.
    pub fn shutdown(self) {
        self.handle.abort();
    }
}

impl Drop for MockFixtureServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn serve_fixture(fixture: Fixture) -> Result<Response<Body>, Infallible> {
    let Fixture { status, headers, body, close_after_bytes } = fixture;
    let body = match close_after_bytes {
        Some(cut) => body[..cut.min(body.len())].to_vec(),
        None => body,
    };
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    Ok(builder.body(Body::from(body)).expect("build mock fixture response"))
}

/// Runs `drive` (a closure that performs the HTTP round trip against `server`) to completion,
/// giving wire-API tests a single place to await a bounded timeout instead of a fixed sleep.
pub async fn with_timeout<F, T>(seconds: u64, drive: F) -> T
where
    F: Future<Output = T>,
{
    tokio::time::timeout(std::time::Duration::from_secs(seconds), drive)
        .await
        .unwrap_or_else(|_| panic!("mock fixture server round trip did not complete within {seconds}s"))
}
