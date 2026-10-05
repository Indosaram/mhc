use maho_server::app_server::daemon_occupancy::{AppServerListenOccupancy, inspect_listen_occupancy};
use serde_json::json;

#[tokio::test]
async fn incompatible_tcp_owner_is_rejected_and_available_address_is_released() {
    let directory = tempfile::tempdir().unwrap();
    let token = directory.path().join("absent-token");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let listen = json!({"kind":"ws","host":"127.0.0.1","port":port,"url":format!("ws://127.0.0.1:{port}")});
    let error = inspect_listen_occupancy(&token, &listen, "test").await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    drop(listener);
    assert_eq!(inspect_listen_occupancy(&token, &listen, "test").await.unwrap(), AppServerListenOccupancy::Available);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1",port)).await.unwrap();
    drop(listener);
}
