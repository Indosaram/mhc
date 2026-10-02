use super::{errors::JsonRpcError, server_core::ServerCore, websocket_auth::ResolvedWebSocketListenerAuth, websocket_connection_handler::{serve_websocket, DEFAULT_OUTBOUND_QUEUE_BYTES}};
use std::{os::unix::{ffi::OsStrExt, fs::PermissionsExt}, path::PathBuf, sync::Arc};
use tokio::{net::{UnixListener, UnixStream}, sync::{RwLock, watch}, task::JoinHandle};

pub struct UnixSocketListenerHandle {
    pub socket_path: PathBuf,
    pub core: Arc<RwLock<ServerCore>>,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), JsonRpcError>>,
}
impl UnixSocketListenerHandle {
    pub async fn close(self) -> Result<(), JsonRpcError> {
        self.shutdown.send_replace(true);
        self.task.await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?
    }
}
pub async fn start_unix_socket_listener(socket_path: PathBuf, enforce_owner_only_directory: bool, auth: ResolvedWebSocketListenerAuth, core: Arc<RwLock<ServerCore>>, outbound_queue_bytes: Option<usize>) -> Result<UnixSocketListenerHandle, JsonRpcError> {
    if socket_path.as_os_str().as_bytes().len() > 100 { return Err(JsonRpcError::new(-32600, format!("Unix socket path is too long for portable app-server startup: {}. pass a shorter unix:///path.", socket_path.display()))); }
    let parent = socket_path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut directory = tokio::fs::DirBuilder::new(); directory.recursive(true).mode(0o700);
    directory.create(parent).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
    if enforce_owner_only_directory { tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?; }
    match tokio::fs::metadata(&socket_path).await {
        Ok(_) => {
            if matches!(tokio::time::timeout(std::time::Duration::from_secs(1), UnixStream::connect(&socket_path)).await, Ok(Ok(_))) { return Err(JsonRpcError::new(-32603, format!("{}: address already in use by a live server.", socket_path.display()))); }
            tokio::fs::remove_file(&socket_path).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(JsonRpcError::new(-32603, error.to_string())),
    }
    let listener = UnixListener::bind(&socket_path).map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
    let (shutdown, mut signal) = watch::channel(false);
    let path = socket_path.clone(); let running_core = core.clone(); let auth = Arc::new(auth);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::<Result<(), JsonRpcError>>::new(); let mut next_id = 1;
        loop {
            tokio::select! {
                _ = signal.wait_for(|value| *value) => break,
                result = connections.join_next(), if !connections.is_empty() => {
                    match result { Some(Ok(Err(error))) => eprintln!("app-server unix transport error: {}", error.message), Some(Err(error)) => eprintln!("app-server unix transport error: {error}"), _ => {} }
                },
                accepted = listener.accept() => {
                    let (stream, _) = accepted.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
                    let id = format!("unix-{next_id}"); next_id += 1;
                    connections.spawn(serve_websocket(stream, running_core.clone(), auth.clone(), id, outbound_queue_bytes.unwrap_or(DEFAULT_OUTBOUND_QUEUE_BYTES)));
                },
            }
        }
        connections.abort_all();
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result && !error.is_cancelled() { eprintln!("app-server unix transport error: {error}"); }
        }
        for index in 1..next_id { running_core.write().await.remove_connection(&format!("unix-{index}")); }
        drop(listener);
        match tokio::fs::remove_file(path).await { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(JsonRpcError::new(-32603, error.to_string())) }
    });
    Ok(UnixSocketListenerHandle { socket_path, core, shutdown, task })
}
