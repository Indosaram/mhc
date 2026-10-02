#[cfg(unix)]
#[tokio::test]
async fn session_listing_uses_real_socket_and_closes_peer() {
    use maho_cli::experimental::mini::{session::list_sessions, shared::{rpc::*, transport::SocketTransport, protocol::SESSIONS}};
    use std::{collections::HashMap, sync::Arc};
    let directory = tempfile::tempdir().unwrap();
    let transport = SocketTransport { path: directory.path().join("socket") };
    let listener = transport.listen().await.unwrap();
    let server = async {
        let (socket, _) = listener.accept().await.unwrap();
        let (input, output) = socket.into_split();
        let peer = create_peer(input, output, PeerOptions { dead_ms: 0, ..Default::default() });
        let handler: Handler = Arc::new(|_, _| Box::pin(async { Ok(serde_json::json!([{"id":"session", "path":"/session", "cwd":"/project", "createdAt":42}])) }));
        peer.provide(SESSIONS, HashMap::from([("list".to_owned(), handler)]));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let sender = std::sync::Mutex::new(Some(sender));
        peer.on_close(move || { if let Some(sender) = sender.lock().expect("close listener").take() { let _ = sender.send(()); } });
        receiver.await.unwrap();
    };
    let client = async { let sessions = list_sessions(&transport).await.unwrap(); assert_eq!(sessions[0].id, "session"); assert_eq!(sessions[0].created_at, 42.0); };
    tokio::time::timeout(std::time::Duration::from_secs(1), async { tokio::join!(server, client) }).await.unwrap();
}
