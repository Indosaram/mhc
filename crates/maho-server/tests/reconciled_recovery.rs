use maho_server::app_server::{websocket_auth::{resolve_websocket_listener_auth, ResolvedWebSocketListenerAuth}, wire_thread::build_disk_wire_thread};
use serde_json::json;

#[tokio::test]
async fn absent_websocket_auth_without_default_path_resolves_off() {
    assert!(matches!(resolve_websocket_listener_auth(None, None).await.unwrap(), ResolvedWebSocketListenerAuth::Off));
}
#[tokio::test]
async fn disk_wire_missing_dates_project_as_zero_seconds() {
    let built = build_disk_wire_thread(&json!({"id":"disk","sessionId":"disk","cwd":"/work","status":{"type":"notLoaded"},"name":null}), "1.0").await.unwrap();
    assert_eq!(built["createdAt"].as_f64(), Some(0.0));
    assert_eq!(built["updatedAt"].as_f64(), Some(0.0));
    assert_eq!(built["recencyAt"].as_f64(), Some(0.0));
}
