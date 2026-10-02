use maho_server::server::{testing::*,types::*,unix::UnixServer,errors::ServerError};
use serde_json::json;
use std::{sync::Arc,time::Duration};

#[tokio::test]
async fn testing_facades_drive_real_socket_attach_fragmentation_and_session_calls() {
    tokio::time::timeout(Duration::from_secs(10),async {
        let directory=tempfile::tempdir().unwrap();let path=directory.path().join("test.sock");
        let host=Arc::new(TestServerHost::default());host.seed(None,None).await.unwrap();
        let test=create_test_server(TestServerOptions {host:Some(host.clone()),..Default::default()}).unwrap();
        let id=test.server.server_id.clone();let mut listener=UnixServer::start(test.server.clone(),path.clone()).await.unwrap();
        let client=connect_unix_test_client(&path).await.unwrap();assert_eq!(client.hello(None).await.unwrap()["serverId"],id);
        assert_eq!(client.attach(&id,"session-1").await.unwrap()["result"],json!(null));
        let harness=host.latest_harness("session-1").await.unwrap();assert_eq!(harness.state.lock().await.attached_clients,1);
        let call=json!({"serviceId":"echo","member":"echo","args":["input"]});
        harness.state.lock().await.next_service_result=Some(json!({"value":7}));
        assert_eq!(client.request_session_service(&id,"session-1",call.clone(),None).await.unwrap()["result"],json!({"value":7}));
        let index=client.messages().len();
        client.send_fragmented_message(&json!({"type":"request","id":"fragmented","target":{"serverId":id},"call":{"serviceId":"pi.session-management","member":"detach","args":[]}}),3).await.unwrap();
        assert_eq!(client.next_from(index,|message|message["type"]=="response" && message["id"]=="fragmented").await.unwrap()["result"],json!(null));
        assert_eq!(harness.state.lock().await.attachment_release_count,1);
        assert_eq!(harness.state.lock().await.service_calls,vec![call]);
        client.close().await.unwrap();client.wait_for_close().await;assert!(client.closed());listener.close().await.unwrap();assert!(!path.exists());
        assert_eq!(harness.state.lock().await.close_count,1);
    }).await.unwrap();
}

#[tokio::test]
async fn testing_host_open_service_close_gates_and_release_failure_keep_native_lifetimes() {
    tokio::time::timeout(Duration::from_secs(10),async {
        let host=Arc::new(TestServerHost::default());let metadata=host.seed(None,None).await.unwrap();
        assert_eq!(metadata.created_at,1);
        let gate=host.gate_next_open_session().await;
        let opening={let host=host.clone();tokio::spawn(async move {host.open_session(serde_json::to_value(metadata).unwrap()).await})};
        gate.entered.wait().await;assert!(!opening.is_finished());gate.release.resolve(());let handle=opening.await.unwrap().unwrap();
        let harness=host.latest_harness("session-1").await.unwrap();let attachment=handle.attach_client().await.unwrap();
        let gate=harness.gate_next_service_call().await;
        let invocation={let attachment=attachment.clone();tokio::spawn(async move {
            let (_,cancelled)=tokio::sync::watch::channel(false);
            attachment.invoke_service(json!({"serviceId":"test","member":"run","args":[]}),Arc::new(|_,_|Box::pin(async {Ok(())})),Context {cancelled}).await
        })};
        gate.entered.wait().await;assert!(!invocation.is_finished());gate.release.resolve(());assert_eq!(invocation.await.unwrap().unwrap(),Some(json!({"ok":true})));
        harness.state.lock().await.fail_attachment_release=Some(ServerError::new("test","release"));assert!(attachment.release().await.is_err());
        assert_eq!(harness.state.lock().await.attached_clients,1);harness.state.lock().await.fail_attachment_release=None;
        attachment.release().await.unwrap();attachment.release().await.unwrap();assert_eq!(harness.state.lock().await.attachment_release_count,2);
        let gate=harness.gate_next_close().await;let closing=tokio::spawn(async move {handle.close().await});
        gate.entered.wait().await;assert!(!closing.is_finished());gate.release.resolve(());closing.await.unwrap().unwrap();harness.closed.wait().await;
        let handle=host.open_session(host.resolve_session("session-1").await.unwrap()).await.unwrap();handle.close().await.unwrap();
    }).await.unwrap();
}
