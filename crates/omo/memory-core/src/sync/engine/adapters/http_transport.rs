//! Transport over the sync routes (omo-service #463) and the account event stream. Digests travel
//! as 43-char base64url and are handled as hex inside the engine. A pull reads
//! `GET /v1/sync/batches?after=n` (at most 500 rows a page); when the cursor is below the retained
//! history it first reads the snapshot from `GET /v1/sync/state`. Each batch row names its batch id
//! (#463 contract 1.17.0; null on keyring rows); the engine checks it against the sealed header.
//! Refusals arrive as `{ error: { code } }`; `stale_base` is answered with the head read back from
//! the state route.
//!
//! The wire itself sits behind [`HttpClient`] (the upstream `fetch` option): the transport owns the
//! routes, framing and error mapping, and a concrete client is injected by the harness.

use std::rc::Rc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::error::{EngineError, EngineResult};
use super::super::ports::{
    AppendRequest, AppendResult, EntryKind, PortFuture, PullPage, PulledEntry, SnapshotRef,
    StreamHandlers, Transport, Unsubscribe,
};
use crate::support::sha256::hex;

pub const STREAM_RETRY_MS: u64 = 5_000;

pub fn b64url_of_hex(hex_text: &str) -> String {
    URL_SAFE_NO_PAD.encode(unhex(hex_text))
}

pub fn hex_of_b64url(b64url: &str) -> String {
    hex(&URL_SAFE_NO_PAD
        .decode(b64url.trim_end_matches('='))
        .unwrap_or_default())
}

fn unhex(text: &str) -> Vec<u8> {
    let digit = |c: u8| (c as char).to_digit(16);
    text.as_bytes()
        .chunks_exact(2)
        .map_while(|pair| Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHttpError {
    pub code: String,
    pub status: u16,
    pub transient: bool,
}

impl SyncHttpError {
    pub fn new(code: impl Into<String>, status: u16, transient: bool) -> Self {
        Self {
            code: code.into(),
            status,
            transient,
        }
    }
}

impl std::fmt::Display for SyncHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sync request refused: {} ({})", self.code, self.status)
    }
}
impl std::error::Error for SyncHttpError {}

pub struct HttpRequest {
    pub method: &'static str,
    /// Absolute URL.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A response body read chunk by chunk (the event stream).
pub trait ByteStream {
    /// The next chunk, or `None` at a clean end of stream.
    fn next_chunk(&mut self) -> PortFuture<'_, Option<Vec<u8>>>;
}

pub struct StreamResponse {
    pub status: u16,
    pub body: Box<dyn ByteStream>,
}

pub trait HttpClient {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse>;
    /// Opens a streaming GET; the response body is read through [`ByteStream`].
    fn open_stream(&self, request: HttpRequest) -> PortFuture<'_, StreamResponse>;
}

pub type TokenSource = Rc<dyn Fn() -> PortFuture<'static, String>>;

#[derive(Clone)]
pub struct HttpTransportOptions {
    pub base_url: String,
    /// The device's bearer token, read per request so a refresh takes effect.
    pub token: TokenSource,
    pub client: Rc<dyn HttpClient>,
}

#[derive(Deserialize)]
struct WireAuthor {
    kind: String,
    id: String,
}

#[derive(Deserialize)]
struct WireRow {
    seq: u64,
    kind: EntryKind,
    author: WireAuthor,
    digest: String,
    object_id: Option<String>,
    #[serde(default)]
    batch_id: Option<String>,
}

#[derive(Deserialize)]
struct WireHead {
    seq: u64,
}

#[derive(Deserialize)]
struct WireSnapshot {
    covers_seq: u64,
    chunk_object_ids: Vec<String>,
}

#[derive(Deserialize)]
struct WireState {
    head: WireHead,
    oldest_retained_seq: u64,
    snapshot: Option<WireSnapshot>,
}

#[derive(Deserialize)]
struct WireBatches {
    entries: Vec<WireRow>,
}

#[derive(Deserialize)]
struct WireUpload {
    upload_id: String,
}

#[derive(Deserialize)]
struct WireSeq {
    seq: u64,
}

pub struct HttpTransport {
    options: HttpTransportOptions,
}

pub fn create_http_transport(options: HttpTransportOptions) -> HttpTransport {
    HttpTransport { options }
}

fn entry_of(row: WireRow) -> PulledEntry {
    PulledEntry {
        seq: row.seq,
        kind: row.kind,
        batch_id: row.batch_id,
        digest: hex_of_b64url(&row.digest),
        object_id: row.object_id.unwrap_or_default(),
        author: format!("{}:{}", row.author.kind, row.author.id),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> EngineResult<T> {
    Ok(serde_json::from_slice(body)?)
}

impl HttpTransport {
    async fn request(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Vec<u8>>,
        json: bool,
    ) -> EngineResult<HttpResponse> {
        let token = (self.options.token)()
            .await
            .map_err(|_| EngineError::Auth)?;
        let mut headers = vec![("authorization".to_string(), format!("Bearer {token}"))];
        if body.is_some() && json {
            headers.push(("content-type".to_string(), "application/json".to_string()));
        }
        let response = self
            .options
            .client
            .send(HttpRequest {
                method,
                url: format!("{}{path}", self.options.base_url),
                headers,
                body,
            })
            .await
            .map_err(|_| EngineError::Network)?;
        if (200..300).contains(&response.status) {
            return Ok(response);
        }
        let code = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/code")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());
        let transient = response.status >= 500;
        Err(SyncHttpError::new(
            if transient {
                "unavailable".to_string()
            } else {
                code
            },
            response.status,
            transient,
        )
        .into())
    }

    async fn read_state(&self) -> EngineResult<WireState> {
        parse(
            &self
                .request("GET", "/v1/sync/state", None, true)
                .await?
                .body,
        )
    }

    async fn do_pull(&self, after: u64, limit: usize) -> EngineResult<PullPage> {
        let state = self.read_state().await?;
        let snapshot = state
            .snapshot
            .filter(|_| after < state.oldest_retained_seq.saturating_sub(1))
            .map(|s| SnapshotRef {
                covers_seq: s.covers_seq,
                object_ids: s.chunk_object_ids,
                digest: None,
            });
        let from = snapshot.as_ref().map_or(after, |s| after.max(s.covers_seq));
        let page: WireBatches = parse(
            &self
                .request("GET", &format!("/v1/sync/batches?after={from}"), None, true)
                .await?
                .body,
        )?;
        let rows: Vec<WireRow> = page.entries.into_iter().take(limit).collect();
        let last = rows.last().map_or(from, |r| r.seq);
        Ok(PullPage {
            snapshot,
            entries: rows.into_iter().map(entry_of).collect(),
            head: state.head.seq.max(last),
            more: last < state.head.seq,
        })
    }

    async fn do_append(&self, req: AppendRequest) -> EngineResult<AppendResult> {
        let body = json!({
            "upload_id": req.upload_id,
            "digest": b64url_of_hex(&req.digest),
            "base_seq": req.base_seq,
            "idempotency_key": req.batch_id,
            "epoch": req.epoch,
            "link": req.link,
            "record_sig": req.record_sig,
        });
        match self
            .request(
                "POST",
                "/v1/sync/batches",
                Some(body.to_string().into_bytes()),
                true,
            )
            .await
        {
            Ok(response) => Ok(AppendResult::Ok {
                seq: parse::<WireSeq>(&response.body)?.seq,
            }),
            Err(EngineError::Http(error)) if error.code == "stale_base" => {
                Ok(AppendResult::StaleBase {
                    head: self.read_state().await?.head.seq,
                })
            }
            Err(error) => Err(error),
        }
    }
}

impl Transport for HttpTransport {
    fn pull(&self, after: u64, limit: usize) -> PortFuture<'_, PullPage> {
        Box::pin(self.do_pull(after, limit))
    }

    fn download<'a>(&'a self, object_id: &'a str) -> PortFuture<'a, Vec<u8>> {
        Box::pin(async move {
            Ok(self
                .request("GET", &format!("/v1/sync/objects/{object_id}"), None, true)
                .await?
                .body)
        })
    }

    fn reserve<'a>(&'a self, size: usize, digest: &'a str) -> PortFuture<'a, String> {
        Box::pin(async move {
            let body = json!({ "kind": "batch", "bytes": size, "sha256": b64url_of_hex(digest) });
            let response = self
                .request(
                    "POST",
                    "/v1/sync/uploads",
                    Some(body.to_string().into_bytes()),
                    true,
                )
                .await?;
            Ok(parse::<WireUpload>(&response.body)?.upload_id)
        })
    }

    fn upload<'a>(&'a self, upload_id: &'a str, bytes: &'a [u8]) -> PortFuture<'a, ()> {
        Box::pin(async move {
            self.request(
                "PUT",
                &format!("/v1/sync/uploads/{upload_id}"),
                Some(bytes.to_vec()),
                false,
            )
            .await?;
            Ok(())
        })
    }

    fn append(&self, request: AppendRequest) -> PortFuture<'_, AppendResult> {
        Box::pin(self.do_append(request))
    }

    fn subscribe(&self, handlers: StreamHandlers) -> Unsubscribe {
        let base_url = self.options.base_url.clone();
        let token = self.options.token.clone();
        let client = self.options.client.clone();
        let handle = tokio::task::spawn_local(async move {
            stream_loop(&base_url, &token, client.as_ref(), &handlers).await;
        });
        Box::new(move || handle.abort())
    }
}

async fn stream_once(
    base_url: &str,
    token: &TokenSource,
    client: &dyn HttpClient,
    handlers: &StreamHandlers,
    up: &mut bool,
) -> EngineResult<()> {
    let bearer = token().await?;
    let response = client
        .open_stream(HttpRequest {
            method: "GET",
            url: format!("{base_url}/v1/me/events"),
            headers: vec![
                ("authorization".to_string(), format!("Bearer {bearer}")),
                ("accept".to_string(), "text/event-stream".to_string()),
            ],
            body: None,
        })
        .await?;
    if !(200..300).contains(&response.status) {
        return Err(SyncHttpError::new("stream", response.status, true).into());
    }
    *up = true;
    (handlers.on_connected)(true);
    let mut body = response.body;
    let mut buffer = String::new();
    let mut pending: Vec<u8> = Vec::new();
    while let Some(chunk) = body.next_chunk().await? {
        pending.extend(chunk);
        // Keep an incomplete trailing UTF-8 sequence for the next chunk.
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) => e.valid_up_to(),
        };
        buffer.push_str(&String::from_utf8_lossy(&pending[..valid]));
        pending.drain(..valid);
        while let Some(end) = buffer.find("\n\n") {
            if let Some(head) = advanced_head(&buffer[..end]) {
                (handlers.on_advanced)(head);
            }
            buffer.drain(..end + 2);
        }
    }
    Ok(())
}

async fn stream_loop(
    base_url: &str,
    token: &TokenSource,
    client: &dyn HttpClient,
    handlers: &StreamHandlers,
) {
    loop {
        let mut up = false;
        // A dropped or refused stream is retried; only an abort of the task ends the loop.
        let _ = stream_once(base_url, token, client, handlers, &mut up).await;
        if up {
            (handlers.on_connected)(false);
        }
        tokio::time::sleep(Duration::from_millis(STREAM_RETRY_MS)).await;
    }
}

/// The advertised head of a `sync.advanced` / `sync.keyring.changed` frame; `None` for any other
/// frame, an empty frame, or malformed data.
pub fn advanced_head(frame: &str) -> Option<u64> {
    let data = frame
        .split('\n')
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return None;
    }
    let event: Value = serde_json::from_str(&data).ok()?;
    match event.get("type").and_then(Value::as_str) {
        Some("sync.advanced" | "sync.keyring.changed") => {
            event.pointer("/head/seq").and_then(Value::as_u64)
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "http_transport_tests.rs"]
mod tests;
