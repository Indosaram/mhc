use maho_server::app_server::{server_core::ServerCore, unix_socket::start_unix_socket_listener, websocket_auth::ResolvedWebSocketListenerAuth};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::{client_async, tungstenite::Message};
#[tokio::test]
async fn unix_websocket_initializes_and_removes_socket_after_shutdown() {
    let directory = tempfile::tempdir().unwrap(); let path = directory.path().join("app.sock");
    let core = Arc::new(RwLock::new(ServerCore::new("/tmp/home".into(), "1".into(), "Linux".into(), "test".into(), "x64".into(), "linux".into())));
    let listener = start_unix_socket_listener(path.clone(), true, ResolvedWebSocketListenerAuth::Off, core.clone(), None).await.unwrap();
    let probe = maho_server::app_server::daemon_probe::probe_listen(&directory.path().join("absent-token"), &json!({"kind":"unix","path":path,"url":"unix://"}), 2000, "1").await.unwrap();
    assert!(probe.is_some());
    let scenario = async {
        let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        let (mut client, _) = client_async("ws://localhost/", stream).await.unwrap();
        client.send(Message::Text(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"c","version":"1"}}}).to_string().into())).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(response["id"], 1);
        client.close(None).await.unwrap(); drop(client);
        listener.close().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), scenario).await.unwrap();
    assert!(!path.exists());
    assert!(core.read().await.get_connection("unix-1").is_none());
    assert!(core.read().await.get_connection("unix-2").is_none());
}
