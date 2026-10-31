use maho_cli::experimental::radius_relay::*;
#[test]
fn relay_frame_roundtrip_and_invalid_headers() {
    let id = "11111111-2222-4333-8444-555555555555";
    let frame = encode_relay_data_frame(id, b"payload").unwrap();
    assert_eq!(parse_relay_data_frame(&frame), Some((id.to_owned(), b"payload".to_vec())));
    assert!(parse_relay_data_frame(&[1; 17]).is_none());
    let mut wrong = frame; wrong[0] = 2;
    assert!(parse_relay_data_frame(&wrong).is_none());
    assert!(encode_relay_data_frame("11111111-2222-1333-8444-555555555555", b"").is_err());
}
#[test]
fn relay_urls_replace_base_path_and_require_http() {
    assert_eq!(relay_websocket_url("https://example.com/base", "server").unwrap(), "wss://example.com/v1/session-relays/server/connect");
    assert!(relay_websocket_url("ftp://example.com", "server").is_err());
}
#[test]
fn host_control_messages_parse_like_upstream() {
    let id = "11111111-2222-4333-8444-555555555555";
    assert_eq!(parse_host_control_message(r#"{"version":1,"type":"ping"}"#), Ok(HostControlMessage::Ping));
    assert_eq!(parse_host_control_message(&format!(r#"{{"version":1,"type":"connection_open","connection_id":"{id}"}}"#)), Ok(HostControlMessage::ConnectionOpen { connection_id: id.to_owned() }));
    assert_eq!(parse_host_control_message(&format!(r#"{{"version":1,"type":"connection_close","connection_id":"{id}","code":4000}}"#)), Ok(HostControlMessage::ConnectionClose { connection_id: id.to_owned(), code: Some(4000) }));
    assert_eq!(parse_host_control_message(r#"{"version":2,"type":"ping"}"#).unwrap_err(), "Unsupported Radius relay control version");
    for invalid in ["not json", "[]", &format!(r#"{{"version":1,"type":"connection_close","connection_id":"{id}","code":999}}"#), r#"{"version":1,"type":"connection_open","connection_id":"bad"}"#, &format!(r#"{{"version":1,"type":"other","connection_id":"{id}"}}"#)] {
        assert_eq!(parse_host_control_message(invalid).unwrap_err(), "Invalid Radius relay control message", "{invalid}");
    }
}
struct FakeClient { connected: std::sync::atomic::AtomicBool, attempts: std::sync::Mutex<Vec<tokio::time::Instant>> }
#[async_trait::async_trait]
impl RadiusReconnectClient for FakeClient {
    fn connected(&self) -> bool { self.connected.load(std::sync::atomic::Ordering::SeqCst) }
    fn disconnected(&self) -> bool { !self.connected() }
    async fn reconnect(&self) -> Result<(), String> {
        let mut attempts = self.attempts.lock().unwrap();
        attempts.push(tokio::time::Instant::now());
        if attempts.len() == 1 { return Err("temporary failure".to_owned()); }
        self.connected.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn disconnect(&self, _reason: &str) { self.connected.store(false, std::sync::atomic::Ordering::SeqCst); }
}
/// senpi "reconnects an established client and restores its selected Session": the first attempt runs after 1 s and
/// fails, the second runs 2 s later and succeeds, then the remembered session is reattached. The clock is paused, so the
/// backoff is measured on tokio's virtual time and the test never sleeps.
#[tokio::test(start_paused = true)]
async fn client_reconnect_backs_off_and_restores_the_selected_session() {
    let client = std::sync::Arc::new(FakeClient { connected: std::sync::atomic::AtomicBool::new(true), attempts: std::sync::Mutex::default() });
    let (reattached, restored) = tokio::sync::oneshot::channel::<String>();
    let reattached = std::sync::Arc::new(std::sync::Mutex::new(Some(reattached)));
    let reattach: RadiusReattach = std::sync::Arc::new(move |session_id| {
        if let Some(sender) = reattached.lock().unwrap().take() { let _ = sender.send(session_id); }
        Box::pin(std::future::ready(Ok(())))
    });
    let reconnect = RadiusClientReconnect::new(client.clone(), Some("demo-1".to_owned()), reattach);
    let start = tokio::time::Instant::now();
    client.disconnect("dropped");
    reconnect.on_connection_state_change(true);
    reconnect.on_connection_state_change(true);
    assert_eq!(restored.await.unwrap(), "demo-1");
    let attempts = client.attempts.lock().unwrap().clone();
    assert_eq!(attempts.iter().map(|at| (*at - start).as_millis()).collect::<Vec<_>>(), [1_000, 3_000]);
    reconnect.dispose().await;
}
#[tokio::test(start_paused = true)]
async fn detaching_while_connected_forgets_the_session_and_dispose_stops_the_loop() {
    let client = std::sync::Arc::new(FakeClient { connected: std::sync::atomic::AtomicBool::new(true), attempts: std::sync::Mutex::default() });
    let reattach: RadiusReattach = std::sync::Arc::new(|session_id| Box::pin(std::future::ready(Err(format!("unexpected reattach {session_id}")))));
    let reconnect = RadiusClientReconnect::new(client.clone(), Some("demo-1".to_owned()), reattach);
    reconnect.on_attachment_change(None);
    client.disconnect("dropped");
    reconnect.on_connection_state_change(true);
    reconnect.dispose().await;
    assert!(client.attempts.lock().unwrap().is_empty(), "dispose aborts the pending backoff before any attempt");
}
