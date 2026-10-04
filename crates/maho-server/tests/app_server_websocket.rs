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

#[tokio::test]
async fn tcp_listener_http_readiness_rejects_origin_before_path_and_does_not_require_auth() {
    use maho_server::app_server::websocket::start_websocket_listener;
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let listener = start_websocket_listener("127.0.0.1",0,ResolvedWebSocketListenerAuth::Bearer {token:"fixture".into(),path:None},core(),None).await.unwrap();
    for (method,path,origin,status,body) in [
        ("GET","/readyz","",200,"ok\n"),
        ("POST","/healthz","",200,"ok\n"),
        ("GET","/readyz","Origin: https://example.test\r\n",403,"forbidden\n"),
        ("GET","/readyz?x=1","",400,"websocket upgrade required\n"),
        ("GET","/","",400,"websocket upgrade required\n"),
    ] {
        let scenario = async {
            let mut stream = tokio::net::TcpStream::connect(listener.address).await.unwrap();
            stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{origin}\r\n").as_bytes()).await.unwrap();
            let mut response = String::new();stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status} ")),"{response}");
            assert!(response.contains("content-type: text/plain; charset=utf-8\r\n"));
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1,body);
        };
        tokio::time::timeout(std::time::Duration::from_secs(3),scenario).await.unwrap();
    }
    listener.close().await.unwrap();
}

#[tokio::test]
async fn readiness_keeps_http_connection_for_pipelined_requests_and_consumes_body() {
    use maho_server::app_server::websocket::start_websocket_listener;
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let listener = start_websocket_listener("127.0.0.1",0,ResolvedWebSocketListenerAuth::Off,core(),None).await.unwrap();
    let scenario = async {
        let mut stream = tokio::net::TcpStream::connect(listener.address).await.unwrap();
        stream.write_all(b"POST /readyz HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\ndataGET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert_eq!(response.matches("HTTP/1.1 200 OK").count(), 2);
        assert_eq!(response.matches("connection: close").count(), 1);
    };
    tokio::time::timeout(std::time::Duration::from_secs(3),scenario).await.unwrap();
    listener.close().await.unwrap();
}
