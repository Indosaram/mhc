#![cfg(unix)]
use maho_server::{
    protocol::codec::{MessageDecoder, encode_client_message},
    server::{
        Server,
        errors::ServerError,
        types::*,
        unix::{UnixServerOptions, create_unix_server, get_unix_socket_path},
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
async fn listener_creates_owner_only_parent_without_changing_existing_directory() {
    use std::os::unix::fs::PermissionsExt;
    let directory=tempfile::tempdir().unwrap();
    let parent=directory.path().join("private");
    let path=parent.join("server.sock");
    let listener = start_unix(path.clone()).await;
    assert_eq!(std::fs::metadata(&parent).unwrap().permissions().mode()&0o777,0o700);
    listener.close().await.unwrap();
    std::fs::set_permissions(&parent,std::fs::Permissions::from_mode(0o750)).unwrap();
    let listener = start_unix(path).await;
    assert_eq!(std::fs::metadata(&parent).unwrap().permissions().mode()&0o777,0o750);
    listener.close().await.unwrap();
}
#[tokio::test]
async fn listener_options_validate_before_publication_and_apply_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("options.sock");
    let options = UnixServerOptions {
        server_id: ID.into(),
        path: path.clone(),
        mode: Some(0o640),
        max_frame_length: Some(MAX),
        max_pending_bytes: Some(u64::from(MAX) + 3),
        ..Default::default()
    };
    assert!(create_unix_server(Arc::new(Host), options.clone()).is_err());
    assert!(!path.exists());
    let server = create_unix_server(
        Arc::new(Host),
        UnixServerOptions { max_pending_bytes: Some(u64::from(MAX) + 4), ..options },
    )
    .unwrap();
    server.start().await.unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    server.close().await.unwrap();
}
#[tokio::test]
async fn owned_bind_path_avoids_linux_public_path_length_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{}.sock", "s".repeat(100)));
    let listener = start_unix(path.clone()).await;
    assert!(path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    listener.close().await.unwrap();
    assert!(!path.exists());
}
async fn start_unix_with(path: std::path::PathBuf, handshake_timeout: Option<Duration>) -> Arc<Server> {
    let server = create_unix_server(
        Arc::new(Host),
        UnixServerOptions { server_id: ID.into(), path, max_frame_length: Some(MAX), handshake_timeout, ..Default::default() },
    )
    .unwrap();
    server.start().await.unwrap();
    server
}
async fn start_unix(path: std::path::PathBuf) -> Arc<Server> {
    start_unix_with(path, None).await
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
            if call["member"] == "leak" {
                return Err(ServerError::new("internal_error", "open /private/host/path: denied"));
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
    let listener = start_unix(path.clone()).await;
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
    let listener = start_unix(path.clone()).await;
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
async fn wrong_protocol_version_receives_source_error_and_socket_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix(path.clone()).await;
    let mut stream = UnixStream::connect(&path).await.unwrap();
    write(&mut stream, json!({"type":"hello","version":7})).await;
    let response = read(&mut stream, &mut MessageDecoder::server()).await;
    assert_eq!(response, json!({"type":"hello_error","error":{"code":"version","message":"Unsupported protocol version 7; expected 8"}}));
    drop(stream);
    listener.close().await.unwrap();
    assert!(!path.exists());
}
#[tokio::test]
async fn preserves_replacement_socket_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix(path.clone()).await;
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
    let listener = start_unix(path.clone()).await;
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
    let listener = start_unix(path.clone()).await;
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

/// Reads the next server message, keeping any extra frames from the same read for later calls.
async fn next_message(stream: &mut UnixStream, decoder: &mut MessageDecoder, queued: &mut std::collections::VecDeque<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(message) = queued.pop_front() {
                return message;
            }
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).await.expect("socket read");
            assert!(n > 0, "server closed before the expected message");
            queued.extend(decoder.push(&bytes[..n]).expect("valid server frames"));
        }
    })
    .await
    .expect("server response deadline")
}
async fn connect_ready(path: &std::path::Path) -> (UnixStream, MessageDecoder) {
    let mut stream = UnixStream::connect(path).await.unwrap();
    let mut decoder = MessageDecoder::server();
    write(&mut stream, json!({"type":"hello","version":8})).await;
    assert_eq!(read(&mut stream, &mut decoder).await["type"], "hello");
    (stream, decoder)
}
async fn assert_closed(stream: &mut UnixStream) {
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut rest)).await.expect("close deadline").expect("socket read");
    assert!(rest.is_empty());
}

#[tokio::test]
async fn second_hello_fails_the_protocol_with_a_final_hello_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix(path.clone()).await;
    let (mut stream, mut decoder) = connect_ready(&path).await;
    write(&mut stream, json!({"type":"hello","version":8})).await;
    assert_eq!(read(&mut stream, &mut decoder).await, json!({"type":"hello_error","error":{"code":"invalid_request","message":"hello may only be sent as the first message"}}));
    assert_closed(&mut stream).await;
    listener.close().await.unwrap();
}

#[tokio::test]
async fn malformed_first_frame_fails_the_protocol_with_a_final_hello_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix(path.clone()).await;
    let mut stream = UnixStream::connect(&path).await.unwrap();
    stream.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    let response = read(&mut stream, &mut MessageDecoder::server()).await;
    assert_eq!(response["type"], "hello_error");
    assert_eq!(response["error"]["code"], "invalid_request");
    assert_closed(&mut stream).await;
    listener.close().await.unwrap();
}

#[tokio::test]
async fn handshake_timeout_fails_the_protocol_with_a_final_hello_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix_with(path.clone(), Some(Duration::from_millis(20))).await;
    let mut stream = UnixStream::connect(&path).await.unwrap();
    assert_eq!(read(&mut stream, &mut MessageDecoder::server()).await, json!({"type":"hello_error","error":{"code":"invalid_request","message":"Handshake timeout"}}));
    assert_closed(&mut stream).await;
    listener.close().await.unwrap();
}

#[tokio::test]
async fn internal_service_errors_are_masked_on_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix(path.clone()).await;
    let (mut stream, mut decoder) = connect_ready(&path).await;
    write(&mut stream,json!({"type":"request","id":"1","target":{"serverId":ID},"call":{"serviceId":"echo","member":"leak","args":[]}})).await;
    assert_eq!(read(&mut stream, &mut decoder).await, json!({"type":"response","id":"1","ok":false,"error":{"code":"internal_error","message":"Internal server error"}}));
    drop(stream);
    listener.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_subscription_names_the_id_and_wrong_server_is_checked_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let listener = start_unix(path.clone()).await;
    let (mut stream, mut decoder) = connect_ready(&path).await;
    let mut queued = std::collections::VecDeque::new();
    let subscribe = |id: &str, server: &str| json!({"type":"request","id":id,"target":{"serverId":server},"call":{"serviceId":"$chord.service","member":"subscribe","args":["s1","echo","singleton"]}});
    write(&mut stream, subscribe("1", ID)).await;
    loop {
        let message = next_message(&mut stream, &mut decoder, &mut queued).await;
        if message["type"] == "response" { assert_eq!(message["ok"], true); break; }
    }
    write(&mut stream, subscribe("2", "00000000-0000-4000-8000-000000000002")).await;
    let wrong = loop {
        let message = next_message(&mut stream, &mut decoder, &mut queued).await;
        if message["type"] == "response" { break message; }
    };
    assert_eq!(wrong["error"]["code"], "wrong_server");
    write(&mut stream, subscribe("3", ID)).await;
    let duplicate = loop {
        let message = next_message(&mut stream, &mut decoder, &mut queued).await;
        if message["type"] == "response" { break message; }
    };
    assert_eq!(duplicate["error"], json!({"code":"invalid_request","message":"Duplicate service subscription s1"}));
    drop(stream);
    listener.close().await.unwrap();
}

#[tokio::test]
async fn server_close_ends_live_unix_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_unix_socket_path(ID, dir.path()).unwrap();
    let server = start_unix(path.clone()).await;
    let (mut stream, _decoder) = connect_ready(&path).await;
    tokio::time::timeout(Duration::from_secs(5), server.close()).await.expect("close deadline").unwrap();
    assert_closed(&mut stream).await;
    assert!(!path.exists());
}
