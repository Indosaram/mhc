use maho_server::server::{listener::*, types::ServerFuture, errors::ServerError};
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
