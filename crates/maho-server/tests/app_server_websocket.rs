use maho_server::app_server::{server_core::ServerCore, websocket_auth::ResolvedWebSocketListenerAuth, websocket_connection_handler::serve_websocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::{client_async, tungstenite::{Message, client::IntoClientRequest}};

fn core() -> Arc<RwLock<ServerCore>> { Arc::new(RwLock::new(ServerCore::new("/tmp/home".into(), "1".into(), "Linux".into(), "test".into(), "x64".into(), "linux".into()))) }
#[tokio::test]
async fn websocket_real_handshake_and_text_initialize_remove_connection_on_close() {
    let core = core();
    let (client, server) = tokio::io::duplex(8192);
    let running = tokio::spawn(serve_websocket(server, core.clone(), Arc::new(ResolvedWebSocketListenerAuth::Off), "ws-1".into(), 1024));
    let scenario = async {
        let (mut websocket, _) = client_async("ws://localhost/", client).await.unwrap();
        websocket.send(Message::Text(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"c","version":"1"}}}).to_string().into())).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(websocket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["codexHome"], "/tmp/home");
        websocket.close(None).await.unwrap();
        drop(websocket);
        running.await.unwrap().unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), scenario).await.unwrap();
    assert!(core.read().await.get_connection("ws-1").is_none());
}
#[tokio::test]
async fn websocket_rejects_origin_before_authentication() {
    let (client, server) = tokio::io::duplex(8192);
    let running = tokio::spawn(serve_websocket(server, core(), Arc::new(ResolvedWebSocketListenerAuth::Bearer { token:"fixture".into(), path:None }), "ws-1".into(), 1024));
    let mut request = "ws://localhost/".into_client_request().unwrap();
    request.headers_mut().insert("origin", "https://example.test".parse().unwrap());
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), client_async(request, client)).await.unwrap();
    match result { Err(tokio_tungstenite::tungstenite::Error::Http(response)) => assert_eq!(response.status().as_u16(), 403), _ => panic!("origin should be rejected") }
    assert!(running.await.unwrap().is_err());
}

#[tokio::test]
async fn tcp_listener_initializes_client_and_releases_listening_port() {
    use maho_server::app_server::websocket::start_websocket_listener;
    let core = core();
    let listener = start_websocket_listener("127.0.0.1", 0, ResolvedWebSocketListenerAuth::Off, core.clone(), None).await.unwrap();
    let address = listener.address;
    let scenario = async {
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, _) = client_async(format!("ws://{address}/"), stream).await.unwrap();
        client.send(Message::Text(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"c","version":"1"}}}).to_string().into())).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(response["id"], 1);
        client.close(None).await.unwrap();
        drop(client);
        listener.close().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), scenario).await.unwrap();
    assert!(core.read().await.get_connection("ws-1").is_none());
    let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
    drop(rebound);
}

#[tokio::test]
async fn daemon_probe_observes_user_agent_through_tcp_listener() {
    use maho_server::app_server::{websocket::start_websocket_listener, daemon_probe::probe_websocket};
    let listener = start_websocket_listener("127.0.0.1", 0, ResolvedWebSocketListenerAuth::Off, core(), None).await.unwrap();
    let url = format!("ws://{}/", listener.address);
    assert_eq!(probe_websocket(&url, None, 2000, "1").await, Some("senpi_app_server_daemon/1 (Linux test; x64) senpi_app_server".into()));
    listener.close().await.unwrap();
}
