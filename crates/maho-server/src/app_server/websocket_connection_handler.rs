use super::{connection::TransportKind, envelope::{ClassifiedIncoming, classify_incoming}, errors::JsonRpcError, notifications::{SendFuture, SendMessage}, server_core::{ConnectionInput, ServerCore}, websocket_auth::{ResolvedWebSocketListenerAuth, is_websocket_request_authorized}};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, sync::atomic::{AtomicBool, AtomicUsize, Ordering}};
use tokio::{io::{AsyncRead, AsyncWrite}, sync::{RwLock, mpsc, oneshot}, task::JoinSet};
use tokio_tungstenite::{accept_hdr_async, tungstenite::{Message, handshake::server::{Request, Response, ErrorResponse}, http::StatusCode, protocol::{CloseFrame, frame::coding::CloseCode}}};

pub const DEFAULT_OUTBOUND_QUEUE_BYTES: usize = 16 * 1024 * 1024;
struct UpgradeAuthorization(Arc<ResolvedWebSocketListenerAuth>);
impl tokio_tungstenite::tungstenite::handshake::server::Callback for UpgradeAuthorization {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        let status = if request.headers().contains_key("origin") { Some(StatusCode::FORBIDDEN) }
        else if request.uri().path_and_query().map(|path| path.as_str()) != Some("/") { Some(StatusCode::BAD_REQUEST) }
        else if !is_websocket_request_authorized(request.headers().get("authorization").and_then(|header| header.to_str().ok()), &self.0) { Some(StatusCode::UNAUTHORIZED) }
        else { None };
        match status {
            Some(status) => { let mut error = ErrorResponse::new(None); *error.status_mut() = status; Err(error) },
            None => Ok(response),
        }
    }
}
pub async fn serve_websocket<S>(stream: S, core: Arc<RwLock<ServerCore>>, auth: Arc<ResolvedWebSocketListenerAuth>, id: String, transport_kind: TransportKind, queue_limit: usize) -> Result<(), JsonRpcError>
where S: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    let websocket = accept_hdr_async(stream, UpgradeAuthorization(auth)).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
    let (mut sink, mut source) = websocket.split();
    let pending_bytes = Arc::new(AtomicUsize::new(0));
    let (send, mut receive) = mpsc::unbounded_channel::<(Message, Option<oneshot::Sender<Result<(), JsonRpcError>>>, usize)>();
    let close_sent = Arc::new(AtomicBool::new(false));
    // senpi `websocket.close(WEBSOCKET_RETRY_LATER, "slow-client")`, sent at most once.
    let close_frame = { let send = send.clone(); let close_sent = close_sent.clone(); move || {
        if close_sent.swap(true, Ordering::SeqCst) { return; }
        if send.send((Message::Close(Some(CloseFrame { code: CloseCode::Again, reason: "slow-client".into() })), None, 0)).is_err() { eprintln!("app-server websocket closed before slow-client close"); }
    } };
    let queued = pending_bytes.clone();
    let slow_close = close_frame.clone();
    // senpi `sendWebSocketMessage`: the frame is queued when `send` is called (like `websocket.send`),
    // so the order of calls is the order on the wire; the returned future resolves on delivery.
    let send_message: SendMessage = Arc::new(move |message| -> SendFuture {
        let payload = match serde_json::to_string(&message) { Ok(payload) => payload, Err(error) => return Box::pin(std::future::ready(Err(JsonRpcError::new(-32603, error.to_string())))) };
        let length = payload.len();
        let previous = queued.fetch_add(length, Ordering::SeqCst);
        if previous.saturating_add(length) > queue_limit {
            queued.fetch_sub(length, Ordering::SeqCst);
            slow_close();
            return Box::pin(std::future::ready(Ok(())));
        }
        let (resolve, result) = oneshot::channel();
        if send.send((Message::Text(payload.into()), Some(resolve), length)).is_err() {
            queued.fetch_sub(length, Ordering::SeqCst);
            return Box::pin(std::future::ready(Err(JsonRpcError::new(-32603, "WebSocket closed"))));
        }
        Box::pin(async move { result.await.map_err(|_| JsonRpcError::new(-32603, "WebSocket closed"))? })
    });
    core.write().await.add_connection_input(ConnectionInput { id: id.clone(), transport_kind, send: send_message, close: Some(Arc::new(move |_reason| close_frame())) });
    let writer = tokio::spawn(async move {
        while let Some((message, resolve, length)) = receive.recv().await {
            let closing = message.is_close();
            let result = sink.send(message).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()));
            pending_bytes.fetch_sub(length, Ordering::SeqCst);
            let failed = result.is_err();
            if let Some(resolve) = resolve { let _delivery = resolve.send(result); }
            if failed || closing { break; }
        }
    });
    // senpi's message handler calls `core.receive(...)` without awaiting it: dispatches of one
    // connection run concurrently, and a rejected dispatch terminates the connection.
    let mut dispatches = JoinSet::<Result<(), JsonRpcError>>::new();
    let result = async {
        loop {
            let message = tokio::select! {
                message = source.next() => message,
                Some(joined) = dispatches.join_next(), if !dispatches.is_empty() => {
                    match joined {
                        Ok(Ok(())) => continue,
                        Ok(Err(error)) => { eprintln!("app-server websocket transport error: {}", error.message); return Err(error); },
                        Err(error) => { eprintln!("app-server websocket transport error: {error}"); return Err(JsonRpcError::new(-32603, error.to_string())); },
                    }
                },
            };
            let Some(message) = message else { break; };
            let message = message.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
            if let Message::Text(text) = message {
                // Pinned per-message dispatch holds no shared lock: snapshot under the read guard, drop
                // it, then dispatch, so add/remove_connection writers and nested `core.read()` (turn
                // `emit`) never queue behind a slow handler.
                let (context, connection) = { let core = core.read().await; (core.receive_context(&id), core.get_connection(&id)) };
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(value) => if let Some(context) = context {
                        // senpi's `initialize` and response/notification routing complete before the
                        // next message is read (no await precedes them); only method dispatch overlaps.
                        let envelope = classify_incoming(value);
                        if matches!(&envelope, ClassifiedIncoming::Request(request) if request["method"] != "initialize") {
                            dispatches.spawn(async move { context.receive(envelope).await });
                        } else { context.receive(envelope).await?; }
                    },
                    Err(_) => { if let Some(connection) = connection { dispatches.spawn((connection.send)(serde_json::json!({"id":null,"error":super::errors::parse_error()}))); } },
                }
            } else if message.is_close() { break; }
        }
        Ok(())
    }.await;
    core.write().await.remove_connection(&id);
    // In-flight dispatches finish on their own (their sends fail once the connection is gone),
    // like senpi's un-awaited `core.receive` promises.
    dispatches.detach_all();
    writer.abort();
    if let Err(error) = writer.await && !error.is_cancelled() { return Err(JsonRpcError::new(-32603, error.to_string())); }
    result
}
