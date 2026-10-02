use super::{errors::JsonRpcError, server_core::ServerCore, websocket_auth::ResolvedWebSocketListenerAuth, websocket_connection_handler::{serve_websocket, DEFAULT_OUTBOUND_QUEUE_BYTES}};
use std::{net::IpAddr, sync::Arc};
use tokio::{net::TcpListener, sync::{RwLock, watch}, task::JoinHandle};

pub struct WebSocketListenerHandle {
    pub address: std::net::SocketAddr,
    pub core: Arc<RwLock<ServerCore>>,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), JsonRpcError>>,
}
impl WebSocketListenerHandle {
    pub async fn close(self) -> Result<(), JsonRpcError> {
        self.shutdown.send_replace(true);
        self.task.await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?
    }
}
pub async fn start_websocket_listener(host: &str, port: u16, auth: ResolvedWebSocketListenerAuth, core: Arc<RwLock<ServerCore>>, outbound_queue_bytes: Option<usize>) -> Result<WebSocketListenerHandle, JsonRpcError> {
    let ip: IpAddr = host.parse().map_err(|_| JsonRpcError::new(-32600, "Websocket listener host must be an IP literal"))?;
    if matches!(auth, ResolvedWebSocketListenerAuth::Off) && !ip.is_loopback() { return Err(JsonRpcError::new(-32600, "Refusing unauthenticated app-server websocket on non-loopback host.")); }
    let listener = TcpListener::bind((ip, port)).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
    let address = listener.local_addr().map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
    let (shutdown, mut signal) = watch::channel(false);
    let auth = Arc::new(auth); let running_core = core.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::<Result<(), JsonRpcError>>::new(); let mut next_id = 1;
        loop {
            tokio::select! {
                _ = signal.wait_for(|value| *value) => break,
                result = connections.join_next(), if !connections.is_empty() => {
                    match result { Some(Ok(Err(error))) => eprintln!("app-server websocket transport error: {}", error.message), Some(Err(error)) => eprintln!("app-server websocket transport error: {error}"), _ => {} }
                },
                accepted = listener.accept() => {
                    let (stream, _) = accepted.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
                    let id = format!("ws-{next_id}"); next_id += 1;
                    connections.spawn(serve_websocket(stream, running_core.clone(), auth.clone(), id, outbound_queue_bytes.unwrap_or(DEFAULT_OUTBOUND_QUEUE_BYTES)));
                },
            }
        }
        connections.abort_all();
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result && !error.is_cancelled() { eprintln!("app-server websocket transport error: {error}"); }
        }
        for index in 1..next_id { running_core.write().await.remove_connection(&format!("ws-{index}")); }
        drop(listener);
        Ok(())
    });
    Ok(WebSocketListenerHandle { address, core, shutdown, task })
}
