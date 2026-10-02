use maho_cli::experimental::coordinator::*;
#[cfg(unix)]
#[tokio::test]
async fn coordinator_routes_control_messages_and_cleans_owned_sockets() {
    use maho_cli::experimental::mini::shared::transport::JsonConnection;
    let directory = tempfile::tempdir().unwrap();
    let public = directory.path().join("public"); let control = directory.path().join("control");
    let shutdown = maho_ai::utils::abort::AbortController::new();
    let (ready, readiness) = tokio::sync::oneshot::channel();
    let signal = shutdown.signal();
    let runtime = run_coordinator_ready(&public, &control, &signal, Some(ready));
    let client = async {
        readiness.await.unwrap();
        let socket = tokio::net::UnixStream::connect(&control).await.unwrap(); let (input, output) = socket.into_split(); let mut server = JsonConnection::new(input, output);
        server.send(&serde_json::json!({"type":"register_server", "protocol":3, "serverConnectionId":"server-one", "endpoint":"/unused"})).await.unwrap();
        assert_eq!(server.receive().await.unwrap().unwrap()["type"], "server_registered");
        let socket = tokio::net::UnixStream::connect(&control).await.unwrap(); let (input, output) = socket.into_split(); let mut peer = JsonConnection::new(input, output);
        peer.send(&serde_json::json!({"type":"register_peer", "protocol":3, "peerId":"peer-one"})).await.unwrap();
        assert_eq!(peer.receive().await.unwrap().unwrap()["type"], "peer_registered");
        assert_eq!(server.receive().await.unwrap().unwrap()["type"], "peer_connected");
        peer.send(&serde_json::json!({"type":"send", "to":"server", "payload":{"value":42}})).await.unwrap();
        assert_eq!(server.receive().await.unwrap().unwrap(), serde_json::json!({"type":"message", "from":"peer-one", "payload":{"value":42}}));
        server.send(&serde_json::json!({"type":"broadcast", "payload":2})).await.unwrap();
        assert_eq!(peer.receive().await.unwrap().unwrap()["payload"], 2);
        shutdown.abort(None);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async { tokio::join!(runtime, client) }).await.unwrap(); result.unwrap();
    assert!(!public.exists()); assert!(!control.exists());
}
#[test]
fn coordinator_messages_keep_machine_field_names() {
    let value = serde_json::json!({"type":"server_registered", "serverConnectionId":"id", "peers":["peer"]});
    let message: CoordinatorMessage = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(message).unwrap(), value);
    assert!(serde_json::from_value::<CoordinatorMessage>(serde_json::json!({"type":"invalid"})).is_err());
}
#[test]
fn routing_drops_unknown_targets_and_limits_broadcast_to_server() {
    let peers = vec!["peer".to_owned()];
    assert_eq!(routed_messages("peer", &serde_json::json!({"type":"send", "to":"server", "payload":1}), &peers, true).unwrap().len(), 1);
    assert!(routed_messages("peer", &serde_json::json!({"type":"send", "to":"missing"}), &peers, true).unwrap().is_empty());
    assert!(routed_messages("peer", &serde_json::json!({"type":"broadcast"}), &peers, true).is_err());
    assert_eq!(routed_messages("server", &serde_json::json!({"type":"broadcast", "payload":2}), &peers, true).unwrap().len(), 1);
}
