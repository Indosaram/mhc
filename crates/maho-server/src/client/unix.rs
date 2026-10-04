use super::{
    errors::ClientError,
    transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers, TransportFuture},
};
use crate::protocol::framing::DEFAULT_MAX_FRAME_LENGTH;
use std::os::unix::fs::FileTypeExt;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex as AsyncMutex, watch},
    task::JoinSet,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixServerRoute {
    pub server_id: String,
    pub path: PathBuf,
}

pub async fn discover_unix_servers(
    directory: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<Vec<UnixServerRoute>, ClientError> {
    if timeout.is_zero() || timeout.as_millis() > 2_147_483_647 {
        return Err(ClientError::InvalidOptions(
            "Unix discovery timeoutMs must be an integer between 1 and 2147483647".into(),
        ));
    }
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut candidates = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_suffix(".sock") else {
            continue;
        };
        if !crate::protocol::messages::is_server_id(id) {
            continue;
        }
        match tokio::fs::symlink_metadata(entry.path()).await {
            Ok(meta) if meta.file_type().is_socket() => candidates.push(UnixServerRoute {
                server_id: id.into(),
                path: entry.path(),
            }),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut tasks = JoinSet::new();
    let mut routes = Vec::new();
    let mut candidates = candidates.into_iter();
    loop {
        while tasks.len() < 16 {
            if let Some(route) = candidates.next() {
                tasks.spawn(probe(route, timeout));
            } else {
                break;
            }
        }
        let Some(result) = tasks.join_next().await else {
            break;
        };
        if let Some(route) = result.map_err(|e| ClientError::Disconnected(e.to_string()))?? {
            routes.push(route);
        }
    }
    routes.sort_by(|a, b| a.server_id.cmp(&b.server_id));
    Ok(routes)
}

async fn probe(
    route: UnixServerRoute,
    timeout: std::time::Duration,
) -> Result<Option<UnixServerRoute>, ClientError> {
    let factory = Arc::new(UnixTransportFactory::new(route.path.clone(), None)?);
    let client = super::Client::new(route.server_id.clone(), DEFAULT_MAX_FRAME_LENGTH, factory)?;
    let result = tokio::time::timeout(timeout, client.connect()).await;
    client.dispose();
    match result {
        Ok(Ok(_)) => Ok(Some(route)),
        Err(_) | Ok(Err(ClientError::Protocol(_))) | Ok(Err(ClientError::Disconnected(_))) => {
            Ok(None)
        }
        Ok(Err(ClientError::Server { code, .. })) if code == "version" => Ok(None),
        Ok(Err(ClientError::Io { kind:std::io::ErrorKind::NotFound|std::io::ErrorKind::ConnectionRefused|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::BrokenPipe|std::io::ErrorKind::TimedOut, .. }))=>Ok(None),
        Ok(Err(error)) => Err(error),
    }
}

pub struct UnixTransportFactory {
    path: PathBuf,
    max_pending_bytes: usize,
}
impl UnixTransportFactory {
    pub fn new(path: PathBuf, max_pending_bytes: Option<usize>) -> Result<Self, ClientError> {
        if path.as_os_str().is_empty() {
            return Err(ClientError::InvalidOptions(
                "Unix transport path must not be empty".into(),
            ));
        }
        let max_pending_bytes = max_pending_bytes.unwrap_or(
            usize::try_from(DEFAULT_MAX_FRAME_LENGTH)
                .map_err(|e| ClientError::InvalidOptions(e.to_string()))?
                * 4,
        );
        if max_pending_bytes == 0 {
            return Err(ClientError::InvalidOptions(
                "Unix transport maxPendingBytes must be a positive safe integer".into(),
            ));
        }
        Ok(Self {
            path,
            max_pending_bytes,
        })
    }
}
struct UnixByteTransport {
    writer: AsyncMutex<OwnedWriteHalf>,
    closed: AtomicBool,
    pending: AtomicUsize,
    max_pending: usize,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<JoinSet<()>>,
}
impl ByteTransportFactory for UnixTransportFactory {
    fn connect(
        &self,
        handlers: ByteTransportHandlers,
    ) -> TransportFuture<'_, Arc<dyn ByteTransport>> {
        Box::pin(async move {
            let stream = UnixStream::connect(&self.path).await?;
            let (mut reader, writer) = stream.into_split();
            let (shutdown, mut stop) = watch::channel(false);
            let transport = Arc::new(UnixByteTransport {
                writer: AsyncMutex::new(writer),
                closed: AtomicBool::new(false),
                pending: AtomicUsize::new(0),
                max_pending: self.max_pending_bytes,
                shutdown,
                tasks: Mutex::new(JoinSet::new()),
            });
            let weak = Arc::downgrade(&transport);
            transport.tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).spawn(async move {
                let mut buffer=[0;65536];
                loop {
                    tokio::select! {
                        biased;
                        _=stop.changed()=>break,
                        result=reader.read(&mut buffer)=>match result {
                            Ok(0)=>{ (handlers.on_close)(); break; },
                            Ok(n)=>{ if weak.upgrade().is_some_and(|t| !t.closed.load(Ordering::SeqCst)) { (handlers.on_data)(&buffer[..n]); } else { break; } },
                            Err(error)=>{ (handlers.on_error)(error.into()); break; },
                        }
                    }
                }
            });
            Ok(transport as Arc<dyn ByteTransport>)
        })
    }
}
struct Pending<'a>(&'a AtomicUsize, usize);
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::SeqCst);
    }
}
impl ByteTransport for UnixByteTransport {
    fn send<'a>(&'a self, chunk: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if self.closed.load(Ordering::SeqCst) {
                return Err(ClientError::Disconnected("Unix transport is closed".into()));
            }
            self.pending
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    n.checked_add(chunk.len())
                        .filter(|n| *n <= self.max_pending)
                })
                .map_err(|_| {
                    ClientError::Disconnected(
                        "Unix transport exceeded its pending byte limit".into(),
                    )
                })?;
            let _pending = Pending(&self.pending, chunk.len());
            let mut writer = self.writer.lock().await;
            if self.closed.load(Ordering::SeqCst) {
                return Err(ClientError::Disconnected("Unix transport is closed".into()));
            }
            writer.write_all(chunk).await?;
            Ok(())
        })
    }
    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.shutdown.send_replace(true);
            self.tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .abort_all();
        }
    }
}
