use maho_server::server::{listener::*, types::ServerFuture, errors::ServerError};
use maho_server::server::testing::{create_test_server, TestServerOptions};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
struct Listener { closes: Arc<AtomicUsize>, fail_close: bool }
impl ServerListener for Listener {
    fn start(&self, _accept: ByteConnectionAcceptor) -> ServerFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn close(&self) -> ServerFuture<'_, ()> {
        Box::pin(async move { self.closes.fetch_add(1, Ordering::SeqCst); if self.fail_close { Err(ServerError::new("internal_error", "close failure")) } else { Ok(()) } })
    }
}
#[tokio::test]
async fn listener_shutdown_admits_all_closes_before_waiting_for_completion() {
    struct ConcurrentListener(Arc<tokio::sync::Barrier>);
    impl ServerListener for ConcurrentListener {
        fn start(&self,_:ByteConnectionAcceptor)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {self.0.wait().await;Ok(())})}
    }
    let admitted=Arc::new(tokio::sync::Barrier::new(2));
    let listeners=ServerListeners::new(vec![Arc::new(ConcurrentListener(admitted.clone())),Arc::new(ConcurrentListener(admitted))]);
    tokio::time::timeout(std::time::Duration::from_secs(2),listeners.close()).await.unwrap().unwrap();
}

#[tokio::test]
async fn listener_close_attempts_every_listener_even_after_failure() {
    let closes = Arc::new(AtomicUsize::new(0));
    let listeners = ServerListeners::new(vec![Arc::new(Listener { closes:closes.clone(), fail_close:true }),Arc::new(Listener { closes:closes.clone(), fail_close:false })]);
    assert!(listeners.close().await.is_err());
    assert_eq!(closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn server_start_rolls_back_already_started_listeners_on_failure() {
    struct StartedListener(Arc<AtomicUsize>);
    struct FailingListener;
    impl ServerListener for StartedListener {
        fn start(&self,_:ByteConnectionAcceptor)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {self.0.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    impl ServerListener for FailingListener {
        fn start(&self,_:ByteConnectionAcceptor)->ServerFuture<'_,()> {Box::pin(async {Err(ServerError::new("internal_error","listener start failed"))})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    let closes=Arc::new(AtomicUsize::new(0));
    let test=create_test_server(TestServerOptions {listeners:vec![Arc::new(StartedListener(closes.clone())),Arc::new(FailingListener)],..Default::default()}).unwrap();
    let error=test.server.start().await.unwrap_err();
    assert_eq!(error.message,"listener start failed");
    assert_eq!(closes.load(Ordering::SeqCst),1,"only the already-started listener is rolled back");
}
