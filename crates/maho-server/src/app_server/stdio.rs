use super::{errors::JsonRpcError, ndjson::{NdjsonEmission, NdjsonReader, serialize_ndjson_message}, server_core::ServerCore};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, sync::{Mutex, RwLock}};

static ACTIVE_STDIO: AtomicBool = AtomicBool::new(false);
pub type StdioShutdownCallback = Arc<dyn Fn(&str) + Send + Sync>;

struct ActiveStdioGuard;
impl Drop for ActiveStdioGuard {
    fn drop(&mut self) { ACTIVE_STDIO.store(false, Ordering::SeqCst); }
}
fn acquire_active_stdio() -> Result<ActiveStdioGuard, JsonRpcError> {
    if ACTIVE_STDIO.swap(true, Ordering::SeqCst) {
        return Err(JsonRpcError::new(-32603, "stdio transport already active"));
    }
    Ok(ActiveStdioGuard)
}

pub async fn run_shared_stdio<R, W>(core: Arc<RwLock<ServerCore>>, input: R, output: W) -> Result<(), JsonRpcError>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin + Send + 'static {
    run_shared_stdio_until(core,input,output,std::future::pending(),None).await
}

pub async fn run_shared_stdio_until<R, W>(core: Arc<RwLock<ServerCore>>, mut input: R, output: W, shutdown: impl std::future::Future<Output=()>, on_shutdown: Option<StdioShutdownCallback>) -> Result<(), JsonRpcError>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin + Send + 'static {
    let _active = acquire_active_stdio()?;
    let writer = Arc::new(Mutex::new(output));
    core.write().await.add_connection("stdio".into(),Arc::new(move |message| {
        let writer = writer.clone();
        Box::pin(async move {
            let line = serialize_ndjson_message(&message).map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
            let mut writer = writer.lock().await;
            writer.write_all(line.as_bytes()).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
            writer.flush().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))
        })
    }));
    let result = async {
        tokio::pin!(shutdown);
        let mut reader = NdjsonReader::default(); let mut buffer = [0;8192];
        let mut reason = "shutdown";
        loop {
            let length = tokio::select! {
                biased;
                _ = &mut shutdown => break,
                result = input.read(&mut buffer) => result.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?,
            };
            let emissions = if length == 0 {reader.end().into_iter().collect()} else {reader.push(&buffer[..length])};
            for emission in emissions {
                match emission {
                    NdjsonEmission::Incoming(envelope) => core.read().await.receive("stdio",envelope).await?,
                    NdjsonEmission::ParseError(response) => {
                        let connection = core.read().await.get_connection("stdio").ok_or_else(||JsonRpcError::new(-32603,"Connection is closed"))?;
                        (connection.send)(response).await?;
                    },
                }
            }
            if length == 0 {reason = "stdin ended";break;}
        }
        Ok(reason)
    }.await;
    core.write().await.remove_connection("stdio");
    let reason = match &result { Ok(reason) => *reason, Err(_) => "error" };
    eprintln!("app-server stdio closed: {reason}");
    if let Some(callback) = &on_shutdown { callback(reason); }
    result.map(|_| ())
}

pub async fn run_stdio<R, W>(core: &mut ServerCore, id: &str, mut input: R, output: W) -> Result<(), JsonRpcError>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin + Send + 'static {
    let output = Arc::new(Mutex::new(output));
    let writer = output.clone();
    core.add_connection(id.to_owned(), Arc::new(move |message| {
        let writer = writer.clone();
        Box::pin(async move {
            let line = serialize_ndjson_message(&message).map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
            writer.lock().await.write_all(line.as_bytes()).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))
        })
    }));
    let result = async {
        let mut reader = NdjsonReader::default();
        let mut buffer = [0; 8192];
        loop {
            let length = input.read(&mut buffer).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
            let messages = if length == 0 { reader.end().into_iter().collect() } else { reader.push(&buffer[..length]) };
            for message in messages {
                match message {
                    NdjsonEmission::Incoming(envelope) => core.receive(id, envelope).await?,
                    NdjsonEmission::ParseError(response) => {
                        let connection = core.get_connection(id).ok_or_else(|| JsonRpcError::new(-32603, "Connection is closed"))?;
                        (connection.send)(response).await?;
                    }
                }
            }
            if length == 0 { break; }
        }
        output.lock().await.flush().await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))
    }.await;
    core.remove_connection(id);
    result
}
