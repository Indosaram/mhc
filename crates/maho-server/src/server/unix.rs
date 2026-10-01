use super::{
    Server,
    errors::ServerError,
    types::{ByteConnection, ServerFuture},
};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};

pub fn get_unix_socket_path(server_id: &str, directory: &Path) -> Result<PathBuf, ServerError> {
    if !crate::protocol::messages::is_server_id(server_id) {
        return Err(ServerError::new(
            "invalid_request",
            "Unix serverId must be a canonical lowercase UUIDv4",
        ));
    }
    Ok(directory.join(format!("{server_id}.sock")))
}
pub struct UnixServer {
    server: Arc<Server>,
    path: PathBuf,
    identity: (u64, u64),
    shutdown: watch::Sender<bool>,
    tasks: JoinSet<Result<(), ServerError>>,
}
impl UnixServer {
    pub async fn start(server: Arc<Server>, path: PathBuf) -> Result<Self, ServerError> {
        if path.as_os_str().is_empty() {
            return Err(ServerError::new(
                "invalid_request",
                "Server Unix socket path must not be empty",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| ServerError::new("invalid_request", "Socket path has no parent"))?;
        tokio::fs::create_dir_all(parent).await?;
        if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
            if !metadata.file_type().is_socket() {
                return Err(ServerError::new(
                    "internal_error",
                    "Unix listener path is not a socket",
                ));
            }
            match UnixStream::connect(&path).await {
                Ok(_) => {
                    return Err(ServerError::new(
                        "internal_error",
                        "Unix listener socket is already active",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    tokio::fs::remove_file(&path).await?
                }
                Err(error) => return Err(error.into()),
            }
        }
        let listener = UnixListener::bind(&path)?;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        let identity = (metadata.dev(), metadata.ino());
        let (shutdown, mut signal) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let runtime = server.clone();
        tasks.spawn(async move {
            let mut connections=JoinSet::new();
            loop {
                let connection_signal=signal.clone();
                tokio::select! {
                    biased;
                    _=signal.wait_for(|v| *v)=>break,
                    connection=listener.accept()=>{
                        let (stream,_)=connection?; let server=runtime.clone(); let stop=connection_signal;
                        connections.spawn(async move { serve_socket(server,stream,stop).await });
                    },
                    result=connections.join_next(),if !connections.is_empty()=>{ if let Some(result)=result { match result { Ok(Ok(()))=>{},Ok(Err(error))=>eprintln!("{}",error.message),Err(error)=>eprintln!("{error}") } } }
                }
            }
            while let Some(result)=connections.join_next().await { result.map_err(|e| ServerError::new("internal_error",&e.to_string()))??; }
            Ok(())
        });
        Ok(Self {
            server,
            path,
            identity,
            shutdown,
            tasks,
        })
    }
    pub async fn close(&mut self) -> Result<(), ServerError> {
        self.shutdown.send_replace(true);
        let mut failure = None;
        while let Some(result) = self.tasks.join_next().await {
            if let Err(error) = result
                .map_err(|e| ServerError::new("internal_error", &e.to_string()))
                .and_then(|r| r)
            {
                failure = Some(error);
            }
        }
        self.server.close().await?;
        match tokio::fs::symlink_metadata(&self.path).await {
            Ok(meta)
                if meta.file_type().is_socket() && (meta.dev(), meta.ino()) == self.identity =>
            {
                tokio::fs::remove_file(&self.path).await?
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        }
    }
}
struct SocketConnection {
    writer: Mutex<OwnedWriteHalf>,
    closed: AtomicBool,
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
            self.writer.lock().await.write_all(bytes).await?;
            Ok(())
        })
    }
    fn close<'a>(&'a self, bytes: Option<&'a [u8]>) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            if self.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let mut writer = self.writer.lock().await;
            if let Some(bytes) = bytes {
                writer.write_all(bytes).await?;
            }
            writer.shutdown().await?;
            Ok(())
        })
    }
}
async fn serve_socket(
    server: Arc<Server>,
    stream: UnixStream,
    signal: watch::Receiver<bool>,
) -> Result<(), ServerError> {
    let (mut reader, writer) = stream.into_split();
    let connection = Arc::new(SocketConnection {
        writer: Mutex::new(writer),
        closed: AtomicBool::new(false),
    });
    let (tx, rx) = mpsc::channel(64);
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
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
    });
    let result = server.serve(connection, rx, signal).await;
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
