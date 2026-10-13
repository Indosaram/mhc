use super::{
    Server,
    errors::ServerError,
    listener::{AcceptedConnection, ByteConnectionAcceptor, ServerListener},
    runtime::{ConnectionCountObserver, ServerErrorObserver, report_to},
    types::{ByteConnection, ServerFuture, ServerHost},
};
use crate::protocol::framing::DEFAULT_MAX_FRAME_LENGTH;
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener as NetUnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};

async fn remove_owned_socket(
    path: &Path,
    identity: (u64, u64),
    prefix: &str,
) -> Result<(), ServerError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket() || (metadata.dev(), metadata.ino()) != identity {
        return Ok(());
    }
    let preserved = path
        .parent()
        .ok_or_else(|| ServerError::new("internal_error", "Socket has no parent"))?
        .join(format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().to_string()[..6]
        ));
    match tokio::fs::rename(path, &preserved).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let moved = tokio::fs::symlink_metadata(&preserved).await?;
    if moved.file_type().is_socket() && (moved.dev(), moved.ino()) == identity {
        tokio::fs::remove_file(&preserved).await?;
        return Ok(());
    }
    match tokio::fs::symlink_metadata(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::rename(&preserved, path).await?
        }
        Ok(_) => {}
        Err(error) => return Err(error.into()),
    }
    Err(ServerError::new(
        "internal_error",
        &format!(
            "Unix listener path changed during cleanup; preserved replacement at {}",
            preserved.display()
        ),
    ))
}
async fn remove_stale_socket(path: &Path) -> Result<(), ServerError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket() {
        return Err(ServerError::new(
            "internal_error",
            "Refusing to remove non-socket Unix listener path",
        ));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(1), UnixStream::connect(path)).await {
        Err(_) | Ok(Ok(_)) => {
            return Err(ServerError::new(
                "internal_error",
                "Unix listener is already running",
            ));
        }
        Ok(Err(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
            ) => {}
        Ok(Err(error)) => return Err(error.into()),
    }
    remove_owned_socket(path, (metadata.dev(), metadata.ino()), "stale").await
}

pub fn get_unix_socket_path(server_id: &str, directory: &Path) -> Result<PathBuf, ServerError> {
    if !crate::protocol::messages::is_server_id(server_id) {
        return Err(ServerError::new(
            "invalid_request",
            "Unix serverId must be a canonical lowercase UUIDv4",
        ));
    }
    Ok(directory.join(format!("{server_id}.sock")))
}
/// Pinned `UnixListenerOptions`; unset fields take the pinned defaults.
#[derive(Clone, Default)]
pub struct UnixListenerOptions {
    pub path: PathBuf,
    /// Socket filesystem permissions. Defaults to owner read/write only (0o600).
    pub mode: Option<u32>,
    /// Maximum framed bytes queued per connection before a slow peer is disconnected.
    pub max_pending_bytes: Option<u64>,
    pub graceful_close_timeout_ms: Option<u32>,
    /// Used to derive and validate max_pending_bytes. Must match the server when customized.
    pub max_frame_length: Option<u32>,
    pub on_error: Option<ServerErrorObserver>,
}
/// Pinned `UnixServerOptions`: server options without listeners plus the Unix listener options.
#[derive(Clone, Default)]
pub struct UnixServerOptions {
    pub server_id: String,
    pub path: PathBuf,
    pub mode: Option<u32>,
    pub max_frame_length: Option<u32>,
    pub max_pending_bytes: Option<u64>,
    pub graceful_close_timeout_ms: Option<u32>,
    pub handshake_timeout: Option<std::time::Duration>,
    pub on_connection_count_changed: Option<ConnectionCountObserver>,
    pub on_error: Option<ServerErrorObserver>,
}
#[derive(Clone, Copy)]
struct ResolvedUnixListenerOptions {
    mode: u32,
    max_pending_bytes: u64,
    graceful_close_timeout_ms: u32,
}
fn resolve_unix_listener_options(
    options: &UnixListenerOptions,
) -> Result<ResolvedUnixListenerOptions, ServerError> {
    if options.path.as_os_str().is_empty() {
        return Err(ServerError::new(
            "invalid_request",
            "Server Unix socket path must not be empty",
        ));
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    let resolved = ResolvedUnixListenerOptions {
        mode: options.mode.unwrap_or(0o600),
        max_pending_bytes: options
            .max_pending_bytes
            .unwrap_or(u64::from(max_frame_length) * 4),
        graceful_close_timeout_ms: options.graceful_close_timeout_ms.unwrap_or(5000),
    };
    if max_frame_length == 0
        || resolved.mode > 0o777
        || resolved.max_pending_bytes < u64::from(max_frame_length) + 4
        || resolved.max_pending_bytes > 9_007_199_254_740_991
        || resolved.graceful_close_timeout_ms == 0
        || resolved.graceful_close_timeout_ms > 2_147_483_647
    {
        return Err(ServerError::new(
            "invalid_request",
            "Invalid Unix listener options",
        ));
    }
    Ok(resolved)
}
struct RunningUnixListener {
    identity: (u64, u64),
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), ServerError>>,
}
/// Pinned `UnixListener`: a `ServerListener` that publishes one Unix-domain socket and hands
/// each accepted connection to the owning `Server`.
pub struct UnixListener {
    path: PathBuf,
    options: ResolvedUnixListenerOptions,
    on_error: Option<ServerErrorObserver>,
    closing: AtomicBool,
    running: Mutex<Option<RunningUnixListener>>,
}
/// Pinned `createUnixListener`.
pub fn create_unix_listener(options: UnixListenerOptions) -> Result<Arc<dyn ServerListener>, ServerError> {
    let resolved = resolve_unix_listener_options(&options)?;
    Ok(Arc::new(UnixListener {
        path: options.path,
        options: resolved,
        on_error: options.on_error,
        closing: AtomicBool::new(false),
        running: Mutex::new(None),
    }))
}
/// Pinned `createUnixServer`: compose `Server` with one Unix-domain socket listener. The caller
/// starts and closes the returned server; `Server::close` also closes the listener.
pub fn create_unix_server(
    host: Arc<dyn ServerHost>,
    options: UnixServerOptions,
) -> Result<Arc<Server>, ServerError> {
    let listener = create_unix_listener(UnixListenerOptions {
        path: options.path,
        mode: options.mode,
        max_pending_bytes: options.max_pending_bytes,
        graceful_close_timeout_ms: options.graceful_close_timeout_ms,
        max_frame_length: options.max_frame_length,
        on_error: options.on_error.clone(),
    })?;
    Server::with_options(
        host,
        options.server_id,
        options.max_frame_length,
        options.handshake_timeout,
        vec![listener],
        options.on_error,
        options.on_connection_count_changed,
    )
}
impl ServerListener for UnixListener {
    fn start(&self, accept: ByteConnectionAcceptor) -> ServerFuture<'_, ()> {
        Box::pin(async move {
            let mut running = self.running.lock().await;
            if running.is_some() {
                return Err(ServerError::new("internal_error", "Unix listener is already started"));
            }
            if self.closing.load(Ordering::SeqCst) {
                return Err(ServerError::new("internal_error", "Unix listener is closing or closed"));
            }
            let path = &self.path;
            let options = self.options;
            let parent = path
                .parent()
                .ok_or_else(|| ServerError::new("invalid_request", "Socket path has no parent"))?;
            tokio::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent).await?;
            remove_stale_socket(path).await?;
            let hash = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
            let owned = parent.join(format!("bind-{}", &hash[..8]));
            remove_stale_socket(&owned).await?;
            let listener = NetUnixListener::bind(&owned)?;
            let metadata = tokio::fs::symlink_metadata(&owned).await?;
            let identity = (metadata.dev(), metadata.ino());
            let publication = async {
                tokio::fs::hard_link(&owned, path).await?;
                tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(options.mode))
                    .await?;
                Ok::<_, ServerError>(())
            }
            .await;
            tokio::fs::remove_file(&owned).await?;
            if let Err(error) = publication {
                drop(listener);
                remove_owned_socket(path, identity, "cleanup").await?;
                return Err(error);
            }
            let (shutdown, mut signal) = watch::channel(false);
            let on_error = self.on_error.clone();
            let task = tokio::spawn(async move {
                let mut connections=JoinSet::new();
                loop {
                    let connection_signal=signal.clone();
                    tokio::select! {
                        biased;
                        _=signal.wait_for(|v| *v)=>break,
                        connection=listener.accept()=>{
                            let (stream,_)=connection?; let accept=accept.clone(); let stop=connection_signal;
                            connections.spawn(async move { serve_socket(accept,stream,stop,options).await });
                        },
                        result=connections.join_next(),if !connections.is_empty()=>{ if let Some(result)=result { match result { Ok(Ok(()))=>{},Ok(Err(error))=>report_to(on_error.as_ref(),error),Err(error)=>report_to(on_error.as_ref(),ServerError::new("internal_error",&error.to_string())) } } }
                    }
                }
                let mut failures = Vec::new();
                let drained = tokio::time::timeout(std::time::Duration::from_millis(u64::from(options.graceful_close_timeout_ms)),async {
                    while let Some(result)=connections.join_next().await {
                        match result {
                            Ok(Ok(()))=>{},
                            Ok(Err(error))=>failures.push(error.message),
                            Err(error)=>failures.push(error.to_string()),
                        }
                    }
                }).await;
                if drained.is_err() {
                    connections.abort_all();
                    while let Some(result)=connections.join_next().await {
                        if let Err(error)=result && !error.is_cancelled() {failures.push(error.to_string());}
                    }
                }
                if failures.is_empty() {Ok(())} else {Err(ServerError::new("internal_error",&failures.join("; ")))}
            });
            *running = Some(RunningUnixListener { identity, shutdown, task });
            Ok(())
        })
    }
    fn close(&self) -> ServerFuture<'_, ()> {
        Box::pin(async move {
            self.closing.store(true, Ordering::SeqCst);
            let mut running = self.running.lock().await;
            let Some(listener) = running.take() else {
                return Ok(());
            };
            listener.shutdown.send_replace(true);
            let mut failure = listener
                .task
                .await
                .map_err(|e| ServerError::new("internal_error", &e.to_string()))
                .and_then(|r| r)
                .err();
            if let Err(error) = remove_owned_socket(&self.path, listener.identity, "cleanup").await {
                failure = Some(error);
            }
            match failure {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
    }
}
struct SocketConnection {
    writer: Mutex<OwnedWriteHalf>,
    closed: AtomicBool,
    pending_bytes: AtomicU64,
    options: ResolvedUnixListenerOptions,
}
struct PendingBytes<'a> {
    pending: &'a AtomicU64,
    bytes: u64,
}
impl Drop for PendingBytes<'_> {
    fn drop(&mut self) {
        self.pending.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}
impl ByteConnection for SocketConnection {
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            if self.closed() {
                return Err(ServerError::new(
                    "internal_error",
                    "Unix connection is closed",
                ));
            }
            let count = bytes.len() as u64;
            self.pending_bytes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |pending| {
                    pending
                        .checked_add(count)
                        .filter(|sum| *sum <= self.options.max_pending_bytes)
                })
                .map_err(|_| {
                    ServerError::new(
                        "internal_error",
                        "Unix connection exceeded its pending byte limit",
                    )
                })?;
            let _reservation = PendingBytes {
                pending: &self.pending_bytes,
                bytes: count,
            };
            self.writer.lock().await.write_all(bytes).await?;
            Ok(())
        })
    }
    fn close<'a>(&'a self, bytes: Option<&'a [u8]>) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            if self.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let graceful = async {
                let mut writer = self.writer.lock().await;
                if let Some(bytes) = bytes {
                    writer.write_all(bytes).await?;
                }
                writer.shutdown().await
            };
            let _closed = tokio::time::timeout(
                std::time::Duration::from_millis(u64::from(self.options.graceful_close_timeout_ms)),
                graceful,
            )
            .await;
            Ok(())
        })
    }
}
async fn serve_socket(
    accept: ByteConnectionAcceptor,
    stream: UnixStream,
    mut stop: watch::Receiver<bool>,
    options: ResolvedUnixListenerOptions,
) -> Result<(), ServerError> {
    let (mut reader, writer) = stream.into_split();
    let connection = Arc::new(SocketConnection {
        writer: Mutex::new(writer),
        closed: AtomicBool::new(false),
        pending_bytes: AtomicU64::new(0),
        options,
    });
    let (tx, rx) = mpsc::channel(64);
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let reading = async {
            let mut bytes = [0; 65536];
            loop {
                match reader.read(&mut bytes).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ok(bytes[..n].to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _receiver_closed = tx.send(Err(error.into())).await;
                        break;
                    }
                }
            }
        };
        // Listener close ends the inbound stream, so the server tears the connection down.
        tokio::select! {
            _ = reading => {},
            _ = stop.wait_for(|v| *v) => {},
        }
    });
    let result = accept(AcceptedConnection { connection, inbound: rx }).await;
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            return Err(ServerError::new("internal_error", &error.to_string()));
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pending_write_limit_rejects_without_writing_and_releases_reservation() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let (_, writer) = stream.into_split();
        let connection = SocketConnection {
            writer: Mutex::new(writer),
            closed: AtomicBool::new(false),
            pending_bytes: AtomicU64::new(0),
            options: ResolvedUnixListenerOptions {
                mode: 0o600,
                max_pending_bytes: 2,
                graceful_close_timeout_ms: 5000,
            },
        };
        assert!(connection.send(b"abc").await.is_err());
        assert_eq!(connection.pending_bytes.load(Ordering::SeqCst), 0);
        connection.send(b"ab").await.unwrap();
        assert_eq!(connection.pending_bytes.load(Ordering::SeqCst), 0);
        let mut received = [0; 2];
        peer.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"ab");
        connection.close(None).await.unwrap();
    }
}
