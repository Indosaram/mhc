use maho_server::{
    protocol::codec::{MessageDecoder, encode_client_message},
    server::{
        Server,
        errors::ServerError,
        types::*,
        unix::{UnixServer, get_unix_socket_path},
    },
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
const ID: &str = "00000000-0000-4000-8000-000000000001";
const MAX: u32 = 1024 * 1024;
#[tokio::test]
async fn listener_options_validate_before_publication_and_apply_mode() {
    use maho_server::server::unix::UnixListenerOptions;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("options.sock");
    let server = Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap();
    let mut options = UnixListenerOptions {
        mode: 0o640,
        max_pending_bytes: u64::from(MAX) + 4,
        graceful_close_timeout_ms: 5000,
    };
    options.max_pending_bytes -= 1;
    assert!(
        UnixServer::start_with_options(server.clone(), path.clone(), options)
            .await
            .is_err()
    );
    assert!(!path.exists());
    options.max_pending_bytes += 1;
    let mut listener = UnixServer::start_with_options(server, path.clone(), options)
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    listener.close().await.unwrap();
}
#[tokio::test]
async fn owned_bind_path_avoids_linux_public_path_length_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{}.sock", "s".repeat(100)));
    let mut listener = UnixServer::start(
        Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap(),
        path.clone(),
    )
    .await
    .unwrap();
    assert!(path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    listener.close().await.unwrap();
    assert!(!path.exists());
}
struct Host;
impl ServerHost for Host {
    fn server_services(&self) -> &dyn RoutedServerServiceHost {
        self
    }
    fn resolve_session<'a>(&'a self, id: &'a str) -> ServerFuture<'a, Value> {
        Box::pin(async move { Ok(json!({"id":id})) })
    }
    fn open_session(&self, _metadata: Value) -> ServerFuture<'_, Arc<dyn RoutedSessionHandle>> {
        Box::pin(async { Err(ServerError::new("session_not_found", "Session not found")) })
    }
}
impl RoutedServerServiceHost for Host {
    fn attach_client(
        &self,
        _presentation: Arc<dyn RoutedServerPresentation>,
    ) -> ServerFuture<'_, Arc<dyn RoutedServerServiceAttachment>> {
        Box::pin(async { Ok(Arc::new(Services) as Arc<dyn RoutedServerServiceAttachment>) })
    }
}
struct Services;
impl RoutedServerServiceAttachment for Services {
    fn invoke_service<'a>(
        &'a self,
        call: Value,
        publish: Publisher,
        mut context: Context,
    ) -> ServerFuture<'a, Option<Value>> {
        Box::pin(async move {
            if call["member"] == "subscribe" {
                publish(
                    call["args"][0].as_str().expect("subscription id").into(),
                    json!({"type":"state","member":"value","sequence":1,"ops":[["s",["n"],1]]}),
                )
                .await?;
                return Ok(Some(
                    json!({"serviceId":"echo","mode":"singleton","instances":[{"members":[{"name":"value","kind":"state","sequence":0,"ops":[["r",{"n":0}]]}]}]}),
                ));
            }
            if call["member"] == "block" {
                context
                    .cancelled
                    .wait_for(|v| *v)
                    .await
                    .map_err(|e| ServerError::new("cancelled", &e.to_string()))?;
            }
            Ok(Some(call["args"].clone()))
        })
    }
    fn release(&self) -> ServerFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
async fn write(stream: &mut UnixStream, value: Value) {
    stream
        .write_all(&encode_client_message(&value, MAX).expect("valid client frame"))
        .await
        .expect("socket write");
}
async fn read(stream: &mut UnixStream, decoder: &mut MessageDecoder) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).await.expect("socket read");
            assert!(n > 0);
            let frames = decoder.push(&bytes[..n]).expect("valid server frames");
            if !frames.is_empty() {
                assert_eq!(frames.len(), 1);
                return frames[0].clone();
            }
        }
    })
    .await
    .expect("server response deadline")
}
#[tokio::test]
async fn real_socket_handshake_and_service_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let server = Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap();
    let mut listener = UnixServer::start(server, path.clone()).await.unwrap();
    let mut stream = UnixStream::connect(&path).await.unwrap();
    let mut decoder = MessageDecoder::server();
    write(&mut stream, json!({"type":"hello","version":8})).await;
    assert_eq!(
        read(&mut stream, &mut decoder).await,
        json!({"type":"hello","version":8,"serverId":ID})
    );
    write(&mut stream,json!({"type":"request","id":"1","target":{"serverId":ID},"call":{"serviceId":"echo","member":"echo","args":["hello"]}})).await;
    assert_eq!(
        read(&mut stream, &mut decoder).await,
        json!({"type":"response","id":"1","ok":true,"result":["hello"]})
    );
    drop(stream);
    listener.close().await.unwrap();
    assert!(!path.exists());
}
#[tokio::test]
async fn wrong_server_does_not_invoke_service() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let mut listener = UnixServer::start(
        Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap(),
        path.clone(),
    )
    .await
    .unwrap();
    let mut stream = UnixStream::connect(&path).await.unwrap();
    let mut decoder = MessageDecoder::server();
    write(&mut stream, json!({"type":"hello","version":8})).await;
    read(&mut stream, &mut decoder).await;
    write(&mut stream,json!({"type":"request","id":"1","target":{"serverId":"00000000-0000-4000-8000-000000000002"},"call":{"serviceId":"echo","member":"echo","args":[]}})).await;
    assert_eq!(
        read(&mut stream, &mut decoder).await["error"]["code"],
        "wrong_server"
    );
    drop(stream);
    listener.close().await.unwrap();
}
#[tokio::test]
async fn preserves_replacement_socket_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let mut listener = UnixServer::start(
        Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap(),
        path.clone(),
    )
    .await
    .unwrap();
    std::fs::rename(&path, dir.path().join("previous.sock")).unwrap();
    let replacement = tokio::net::UnixListener::bind(&path).unwrap();
    listener.close().await.unwrap();
    assert!(path.exists());
    drop(replacement);
}

#[tokio::test]
async fn subscription_flushes_updates_after_snapshot_over_real_client() {
    use maho_server::client::{Client, unix::UnixTransportFactory};
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let mut listener = UnixServer::start(
        Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap(),
        path.clone(),
    )
    .await
    .unwrap();
    let client = Client::new(
        ID.into(),
        MAX,
        Arc::new(UnixTransportFactory::new(path, None).unwrap()),
    )
    .unwrap();
    client.connect().await.unwrap();
    let mut subscription = client
        .subscribe(json!({"serverId":ID}), "echo", "singleton")
        .await
        .unwrap();
    assert_eq!(
        subscription.snapshot["instances"][0]["members"][0]["ops"],
        json!([["r",{"n":0}]])
    );
    subscription.start();
    let update = tokio::time::timeout(Duration::from_secs(3), subscription.updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(update["ops"], json!([["s", ["n"], 1]]));
    client.dispose();
    drop(subscription);
    drop(client);
    listener.close().await.unwrap();
}

#[tokio::test]
async fn callback_service_adapter_delivers_after_activation_and_closes() {
    use maho_server::client::{
        Client, service_transport::ClientServiceTransport, unix::UnixTransportFactory,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let mut listener = UnixServer::start(
        Server::new(Arc::new(Host), ID.into(), Some(MAX), None).unwrap(),
        path.clone(),
    )
    .await
    .unwrap();
    let client = Client::new(
        ID.into(),
        MAX,
        Arc::new(UnixTransportFactory::new(path, None).unwrap()),
    )
    .unwrap();
    client.connect().await.unwrap();
    let adapter =
        ClientServiceTransport::new(client.clone(), Arc::new(|| Some(json!({"serverId":ID}))));
    let (delivered, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (errors, mut observed) = tokio::sync::mpsc::unbounded_channel();
    client
        .set_listener_error_observer(Some(Arc::new(move |error| {
            errors.send(error).expect("error observer");
        })))
        .unwrap();
    let mut subscription = adapter
        .subscribe(
            "echo",
            "singleton",
            Arc::new(move |update| {
                let delivered = delivered.clone();
                Box::pin(async move {
                    delivered.send(update).expect("callback receiver");
                    Err(maho_server::client::errors::ClientError::Protocol(
                        "callback failed".into(),
                    ))
                })
            }),
        )
        .await
        .unwrap();
    assert!(received.try_recv().is_err());
    subscription.activate();
    let update = tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(update["ops"], json!([["s", ["n"], 1]]));
    let error = tokio::time::timeout(Duration::from_secs(3), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        error,
        maho_server::client::errors::ClientError::Protocol("callback failed".into())
    );
    assert!(client.connected());
    subscription.close().await.unwrap();
    subscription.close().await.unwrap();
    client.dispose();
    drop(adapter);
    drop(client);
    listener.close().await.unwrap();
}
