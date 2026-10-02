use super::{errors::JsonRpcError, ndjson::{NdjsonEmission, NdjsonReader, serialize_ndjson_message}, server_core::ServerCore};
use std::sync::Arc;
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, sync::{Mutex, RwLock}};

pub async fn run_shared_stdio<R, W>(core: Arc<RwLock<ServerCore>>, mut input: R, output: W) -> Result<(), JsonRpcError>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin + Send + 'static {
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
        let mut reader = NdjsonReader::default(); let mut buffer = [0;8192];
        loop {
            let length = input.read(&mut buffer).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
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
            if length == 0 {break;}
        }
        Ok(())
    }.await;
    core.write().await.remove_connection("stdio");
    result
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
