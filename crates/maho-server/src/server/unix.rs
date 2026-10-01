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
        atomic::{AtomicBool,AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};
use sha2::{Digest,Sha256};

async fn remove_owned_socket(path:&Path,identity:(u64,u64),prefix:&str)->Result<(),ServerError> {
    let metadata=match tokio::fs::symlink_metadata(path).await {Ok(metadata)=>metadata,Err(error)if error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error.into())};
    if !metadata.file_type().is_socket() || (metadata.dev(),metadata.ino())!=identity {return Ok(());}
    let preserved=path.parent().ok_or_else(||ServerError::new("internal_error","Socket has no parent"))?.join(format!("{prefix}-{}",&uuid::Uuid::new_v4().to_string()[..6]));
    match tokio::fs::rename(path,&preserved).await {Ok(())=>{},Err(error)if error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error.into())}
    let moved=tokio::fs::symlink_metadata(&preserved).await?;
    if moved.file_type().is_socket() && (moved.dev(),moved.ino())==identity {tokio::fs::remove_file(&preserved).await?;return Ok(());}
    match tokio::fs::symlink_metadata(path).await {Err(error)if error.kind()==std::io::ErrorKind::NotFound=>tokio::fs::rename(&preserved,path).await?,Ok(_)=>{},Err(error)=>return Err(error.into())}
    Err(ServerError::new("internal_error",&format!("Unix listener path changed during cleanup; preserved replacement at {}",preserved.display())))
}
async fn remove_stale_socket(path:&Path)->Result<(),ServerError> {
    let metadata=match tokio::fs::symlink_metadata(path).await {Ok(metadata)=>metadata,Err(error)if error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error.into())};
    if !metadata.file_type().is_socket() {return Err(ServerError::new("internal_error","Refusing to remove non-socket Unix listener path"));}
    match tokio::time::timeout(std::time::Duration::from_secs(1),UnixStream::connect(path)).await {
        Err(_)|Ok(Ok(_))=>return Err(ServerError::new("internal_error","Unix listener is already running")),
        Ok(Err(error))if matches!(error.kind(),std::io::ErrorKind::ConnectionRefused|std::io::ErrorKind::NotFound|std::io::ErrorKind::BrokenPipe|std::io::ErrorKind::ConnectionReset)=>{},
        Ok(Err(error))=>return Err(error.into()),
    }
    remove_owned_socket(path,(metadata.dev(),metadata.ino()),"stale").await
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
pub struct UnixServer {
    server: Arc<Server>,
    path: PathBuf,
    identity: (u64, u64),
    shutdown: watch::Sender<bool>,
    tasks: JoinSet<Result<(), ServerError>>,
}
#[derive(Clone,Copy)]
pub struct UnixListenerOptions {pub mode:u32,pub max_pending_bytes:u64,pub graceful_close_timeout_ms:u32}
impl UnixServer {
    pub async fn start(server: Arc<Server>, path: PathBuf) -> Result<Self, ServerError> {
        let max_pending_bytes=u64::from(server.max_frame_length())*4;
        Self::start_with_options(server,path,UnixListenerOptions {mode:0o600,max_pending_bytes,graceful_close_timeout_ms:5000}).await
    }
    pub async fn start_with_options(server:Arc<Server>,path:PathBuf,options:UnixListenerOptions)->Result<Self,ServerError> {
        if options.mode>0o777 || options.max_pending_bytes<u64::from(server.max_frame_length())+4 || options.max_pending_bytes>9_007_199_254_740_991 || options.graceful_close_timeout_ms==0 || options.graceful_close_timeout_ms>2_147_483_647 {return Err(ServerError::new("invalid_request","Invalid Unix listener options"));}
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
        remove_stale_socket(&path).await?;
        let hash=format!("{:x}",Sha256::digest(path.to_string_lossy().as_bytes()));
        let owned=parent.join(format!("bind-{}",&hash[..8]));
        remove_stale_socket(&owned).await?;
        let listener = UnixListener::bind(&owned)?;
        let metadata = tokio::fs::symlink_metadata(&owned).await?;
        let identity = (metadata.dev(), metadata.ino());
        let publication=async {tokio::fs::hard_link(&owned,&path).await?;tokio::fs::set_permissions(&path,std::fs::Permissions::from_mode(options.mode)).await?;Ok::<_,ServerError>(())}.await;
        tokio::fs::remove_file(&owned).await?;
        if let Err(error)=publication {drop(listener);remove_owned_socket(&path,identity,"cleanup").await?;return Err(error);}
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
                        connections.spawn(async move { serve_socket(server,stream,stop,options).await });
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
        remove_owned_socket(&self.path,self.identity,"cleanup").await?;
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
    pending_bytes:AtomicU64,
    options:UnixListenerOptions,
}
struct PendingBytes<'a> {pending:&'a AtomicU64,bytes:u64}
impl Drop for PendingBytes<'_> {fn drop(&mut self) {self.pending.fetch_sub(self.bytes,Ordering::SeqCst);}}
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
            let count=bytes.len() as u64;
            self.pending_bytes.fetch_update(Ordering::SeqCst,Ordering::SeqCst,|pending|pending.checked_add(count).filter(|sum|*sum<=self.options.max_pending_bytes)).map_err(|_|ServerError::new("internal_error","Unix connection exceeded its pending byte limit"))?;
            let _reservation=PendingBytes {pending:&self.pending_bytes,bytes:count};
            self.writer.lock().await.write_all(bytes).await?;
            Ok(())
        })
    }
    fn close<'a>(&'a self, bytes: Option<&'a [u8]>) -> ServerFuture<'a, ()> {
        Box::pin(async move {
            if self.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let graceful=async {let mut writer=self.writer.lock().await;if let Some(bytes)=bytes {writer.write_all(bytes).await?;}writer.shutdown().await};
            let _closed=tokio::time::timeout(std::time::Duration::from_millis(u64::from(self.options.graceful_close_timeout_ms)),graceful).await;
            Ok(())
        })
    }
}
async fn serve_socket(
    server: Arc<Server>,
    stream: UnixStream,
    signal: watch::Receiver<bool>,
    options:UnixListenerOptions,
) -> Result<(), ServerError> {
    let (mut reader, writer) = stream.into_split();
    let connection = Arc::new(SocketConnection {
        writer: Mutex::new(writer),
        closed: AtomicBool::new(false),
        pending_bytes:AtomicU64::new(0),
        options,
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
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pending_write_limit_rejects_without_writing_and_releases_reservation() {
        let (stream,mut peer)=UnixStream::pair().unwrap();let (_,writer)=stream.into_split();
        let connection=SocketConnection {writer:Mutex::new(writer),closed:AtomicBool::new(false),pending_bytes:AtomicU64::new(0),options:UnixListenerOptions {mode:0o600,max_pending_bytes:2,graceful_close_timeout_ms:5000}};
        assert!(connection.send(b"abc").await.is_err());assert_eq!(connection.pending_bytes.load(Ordering::SeqCst),0);
        connection.send(b"ab").await.unwrap();assert_eq!(connection.pending_bytes.load(Ordering::SeqCst),0);
        let mut received=[0;2];peer.read_exact(&mut received).await.unwrap();assert_eq!(&received,b"ab");connection.close(None).await.unwrap();
    }
}
