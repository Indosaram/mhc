use super::{envelope::classify_incoming, errors::JsonRpcError, server_core::ServerCore, websocket_auth::{ResolvedWebSocketListenerAuth, is_websocket_request_authorized}};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, sync::atomic::{AtomicUsize, Ordering}};
use tokio::{io::{AsyncRead, AsyncWrite}, sync::{Mutex, RwLock, mpsc, oneshot}};
use tokio_tungstenite::{accept_hdr_async, tungstenite::{Message, handshake::server::{Request, Response, ErrorResponse}, http::StatusCode}};

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
pub async fn serve_websocket<S>(stream: S, core: Arc<RwLock<ServerCore>>, auth: Arc<ResolvedWebSocketListenerAuth>, id: String, queue_limit: usize) -> Result<(), JsonRpcError>
where S: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    let websocket = accept_hdr_async(stream, UpgradeAuthorization(auth)).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
    let (mut sink, mut source) = websocket.split();
    let pending_bytes = Arc::new(AtomicUsize::new(0));
    let (send, mut receive) = mpsc::unbounded_channel::<(Message, Option<oneshot::Sender<Result<(), JsonRpcError>>>, usize)>();
    let queued = pending_bytes.clone();
    let close_sent = Arc::new(Mutex::new(false));
    let closing = close_sent.clone();
    core.write().await.add_connection(id.clone(), Arc::new(move |message| {
        let send = send.clone(); let queued = queued.clone(); let closing = closing.clone();
        Box::pin(async move {
            let payload = serde_json::to_string(&message).map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
            let length = payload.len();
            let previous = queued.fetch_add(length, Ordering::SeqCst);
            if previous.saturating_add(length) > queue_limit {
                queued.fetch_sub(length, Ordering::SeqCst);
                let mut closing = closing.lock().await;
                if !*closing {
                    *closing = true;
                    send.send((Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame { code:tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Again, reason:"slow-client".into() })), None, 0)).map_err(|_| JsonRpcError::new(-32603, "WebSocket closed"))?;
                }
                return Ok(());
            }
            let (resolve, result) = oneshot::channel();
            send.send((Message::Text(payload.into()), Some(resolve), length)).map_err(|_| JsonRpcError::new(-32603, "WebSocket closed"))?;
            result.await.map_err(|_| JsonRpcError::new(-32603, "WebSocket closed"))?
        })
    }));
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
    let result = async {
        while let Some(message) = source.next().await {
            let message = message.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
            if let Message::Text(text) = message {
                let core = core.read().await;
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(value) => core.receive(&id, classify_incoming(value)).await?,
                    Err(_) => { if let Some(connection) = core.get_connection(&id) { (connection.send)(serde_json::json!({"id":null,"error":super::errors::parse_error()})).await?; } },
                }
            } else if message.is_close() { break; }
        }
        Ok(())
    }.await;
    core.write().await.remove_connection(&id);
    writer.abort();
    if let Err(error) = writer.await && !error.is_cancelled() { return Err(JsonRpcError::new(-32603, error.to_string())); }
    result
}
