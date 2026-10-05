use maho_server::server::{testing::*,types::*,unix::UnixServer,errors::ServerError};
use serde_json::json;
use std::{sync::Arc,time::Duration};

#[tokio::test]
async fn failed_service_attachment_closes_handshaking_connection() {
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct Host;
    struct Connection(Arc<AtomicUsize>);
    impl ServerHost for Host {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,_:&'a str)->ServerFuture<'a,serde_json::Value> {Box::pin(async {Err(ServerError::session_not_found(None))})}
        fn open_session(&self,_:serde_json::Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async {Err(ServerError::session_not_found(None))})}
    }
    impl RoutedServerServiceHost for Host {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {
            Box::pin(async {Err(ServerError::new("internal_error","service attachment failed"))})
        }
    }
    impl ByteConnection for Connection {
        fn closed(&self)->bool {false}
        fn send<'a>(&'a self,_:&'a [u8])->ServerFuture<'a,()> {Box::pin(async {Ok(())})}
        fn close<'a>(&'a self,_:Option<&'a [u8]>)->ServerFuture<'a,()> {Box::pin(async move {self.0.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    let closes=Arc::new(AtomicUsize::new(0));
    let server=maho_server::server::Server::new(Arc::new(Host),"00000000-0000-4000-8000-000000000001".into(),None,None).unwrap();
    let (sender,inbound)=tokio::sync::mpsc::channel(1);let (_shutdown,signal)=tokio::sync::watch::channel(false);
    sender.send(Ok(maho_server::protocol::codec::encode_client_message(&json!({"type":"hello","version":8}),maho_server::protocol::framing::DEFAULT_MAX_FRAME_LENGTH).unwrap())).await.unwrap();
    let error=server.serve(Arc::new(Connection(closes.clone())),inbound,signal).await.unwrap_err();
    assert_eq!(error.message,"service attachment failed");
    assert_eq!(closes.load(Ordering::SeqCst),1);
}

#[tokio::test]
async fn failed_hello_send_releases_services_and_closes_connection() {
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct Host(Arc<AtomicUsize>);
    struct Services(Arc<AtomicUsize>);
    struct Connection(Arc<AtomicUsize>);
    impl ServerHost for Host {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,_:&'a str)->ServerFuture<'a,serde_json::Value> {Box::pin(async {Err(ServerError::session_not_found(None))})}
        fn open_session(&self,_:serde_json::Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async {Err(ServerError::session_not_found(None))})}
    }
    impl RoutedServerServiceHost for Host {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {
            Box::pin(async move {Ok(Arc::new(Services(self.0.clone())) as Arc<dyn RoutedServerServiceAttachment>)})
        }
    }
    impl RoutedServerServiceAttachment for Services {
        fn invoke_service<'a>(&'a self,_:serde_json::Value,_:Publisher,_:Context)->ServerFuture<'a,Option<serde_json::Value>> {Box::pin(async {Ok(None)})}
        fn release(&self)->ServerFuture<'_,()> {Box::pin(async move {self.0.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    impl ByteConnection for Connection {
        fn closed(&self)->bool {false}
        fn send<'a>(&'a self,_:&'a [u8])->ServerFuture<'a,()> {Box::pin(async {Err(ServerError::new("internal_error","hello send failed"))})}
        fn close<'a>(&'a self,_:Option<&'a [u8]>)->ServerFuture<'a,()> {Box::pin(async move {self.0.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    let releases=Arc::new(AtomicUsize::new(0));let closes=Arc::new(AtomicUsize::new(0));
    let server=maho_server::server::Server::new(Arc::new(Host(releases.clone())),"00000000-0000-4000-8000-000000000001".into(),None,None).unwrap();
    let (sender,inbound)=tokio::sync::mpsc::channel(1);let (_shutdown,signal)=tokio::sync::watch::channel(false);
    sender.send(Ok(maho_server::protocol::codec::encode_client_message(&json!({"type":"hello","version":8}),maho_server::protocol::framing::DEFAULT_MAX_FRAME_LENGTH).unwrap())).await.unwrap();
    let error=server.serve(Arc::new(Connection(closes.clone())),inbound,signal).await.unwrap_err();
    assert_eq!(error.message,"hello send failed");
    assert_eq!(releases.load(Ordering::SeqCst),1);
    assert_eq!(closes.load(Ordering::SeqCst),1);
}

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

#[tokio::test]
async fn deferred_keeps_first_resolution_for_all_waiters() {
    let deferred=Deferred::default();deferred.resolve(1);deferred.resolve(2);
    assert_eq!(deferred.wait().await,1);assert_eq!(deferred.wait().await,1);
}

#[tokio::test]
async fn client_failure_rejects_only_registered_waiters_and_later_receive_resolves_new_waiter() {
    struct Channel;
    impl WireChannel for Channel {
        fn send<'a>(&'a self,_:&'a [u8])->ServerFuture<'a,()> {Box::pin(async {Ok(())})}
        fn send_fragmented<'a>(&'a self,_:&'a [u8],_:usize)->ServerFuture<'a,()> {Box::pin(async {Ok(())})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    let client=ProtocolTestClient::new(Arc::new(Channel));
    let first=client.next(|message|message["type"]=="hello");client.fail(ServerError::new("test","transient"));
    assert_eq!(first.await.unwrap_err().code,"test");
    let second=client.next(|message|message["type"]=="hello");
    client.receive(&maho_server::protocol::codec::encode_server_message(&json!({"type":"hello","version":8,"serverId":"00000000-0000-4000-8000-000000000001"}),maho_server::protocol::framing::DEFAULT_MAX_FRAME_LENGTH).unwrap());
    assert_eq!(second.await.unwrap()["type"],"hello");
    let closed=client.next(|message|message["type"]=="response");client.mark_closed();assert!(closed.await.is_err());
}

#[tokio::test]
async fn native_server_aggregates_owned_listeners_and_reports_errors_to_the_observer() {
    use maho_server::server::listener::ServerListener;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct Host;
    struct Services;
    struct Connection;
    impl ServerHost for Host {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,_:&'a str)->ServerFuture<'a,serde_json::Value> {Box::pin(async {Err(ServerError::session_not_found(None))})}
        fn open_session(&self,_:serde_json::Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async {Err(ServerError::session_not_found(None))})}
    }
    impl RoutedServerServiceHost for Host {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {Box::pin(async {Ok(Arc::new(Services) as Arc<dyn RoutedServerServiceAttachment>)})}
    }
    impl RoutedServerServiceAttachment for Services {
        fn invoke_service<'a>(&'a self,_:serde_json::Value,_:Publisher,_:Context)->ServerFuture<'a,Option<serde_json::Value>> {Box::pin(async {Ok(None)})}
        fn release(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    impl ByteConnection for Connection {
        fn closed(&self)->bool {false}
        fn send<'a>(&'a self,_:&'a [u8])->ServerFuture<'a,()> {Box::pin(async {Err(ServerError::new("internal_error","hello send failed"))})}
        fn close<'a>(&'a self,_:Option<&'a [u8]>)->ServerFuture<'a,()> {Box::pin(async {Ok(())})}
    }
    struct CountingListener { starts:Arc<AtomicUsize>, closes:Arc<AtomicUsize> }
    impl ServerListener for CountingListener {
        fn start(&self,_:ByteConnectionAcceptor)->ServerFuture<'_,()> {Box::pin(async move {self.starts.fetch_add(1,Ordering::SeqCst);Ok(())})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {self.closes.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    let starts=Arc::new(AtomicUsize::new(0));let closes=Arc::new(AtomicUsize::new(0));
    let errors:Arc<Mutex<Vec<String>>>=Arc::new(Mutex::new(Vec::new()));let captured=errors.clone();
    let test=create_test_server(TestServerOptions {
        host:Some(Arc::new(Host)),
        listeners:vec![Arc::new(CountingListener {starts:starts.clone(),closes:closes.clone()})],
        on_error:Some(Arc::new(move |error:ServerError| captured.lock().unwrap().push(error.message))),
        ..Default::default()
    }).unwrap();
    test.server.start().await.unwrap();
    assert_eq!(starts.load(Ordering::SeqCst),1);
    let (sender,inbound)=tokio::sync::mpsc::channel(1);let (_shutdown,signal)=tokio::sync::watch::channel(false);
    sender.send(Ok(maho_server::protocol::codec::encode_client_message(&json!({"type":"hello","version":8}),maho_server::protocol::framing::DEFAULT_MAX_FRAME_LENGTH).unwrap())).await.unwrap();
    let error=test.server.serve(Arc::new(Connection),inbound,signal).await.unwrap_err();
    assert_eq!(error.message,"hello send failed");
    assert_eq!(errors.lock().unwrap().as_slice(),["hello send failed".to_string()]);
    test.server.close().await.unwrap();
    assert_eq!(closes.load(Ordering::SeqCst),1);
}
