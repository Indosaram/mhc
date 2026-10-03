use std::sync::atomic::{AtomicI32,Ordering};
use tokio::sync::OnceCell;

#[derive(Default)]
pub struct RpcShutdown { code:AtomicI32,disposed:OnceCell<()> }
/// Builds the process shutdown entry shared by EOF, signals and transport failures
/// (senpi `createRpcShutdown`).
pub fn create_rpc_shutdown()->RpcShutdown{RpcShutdown::default()}
impl RpcShutdown {
    /// Every caller joins disposal; the first nonzero code survives a later EOF.
    pub async fn shutdown<F,Fut>(&self,exit_code:i32,dispose:F) -> i32
    where F:FnOnce() -> Fut,Fut:std::future::Future<Output=()> {
        match self.code.compare_exchange(0,exit_code,Ordering::SeqCst,Ordering::SeqCst) { Ok(_) | Err(_) => {} }
        self.disposed.get_or_init(dispose).await;
        self.code.load(Ordering::SeqCst)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    async fn joined(first_code:i32,second_code:i32) {
        let shutdown = RpcShutdown::default();
        let (started_tx,started_rx) = tokio::sync::oneshot::channel();
        let (release_tx,release_rx) = tokio::sync::oneshot::channel();
        let first = shutdown.shutdown(first_code,|| async { started_tx.send(()).unwrap(); release_rx.await.unwrap(); });
        let second = async { started_rx.await.unwrap(); let second = shutdown.shutdown(second_code,|| async { panic!("disposal ran twice") }); tokio::pin!(second); let mut first_poll = std::pin::pin!(std::future::poll_fn(|cx| match second.as_mut().poll(cx) { std::task::Poll::Pending => std::task::Poll::Ready(()),std::task::Poll::Ready(_) => panic!("exited before disposal") })); (&mut first_poll).await; release_tx.send(()).unwrap(); second.await };
        let (first,second) = tokio::join!(first,second);
        assert_eq!((first,second),(1,1));
    }
    #[tokio::test] async fn failure_then_eof_joins_disposal() { joined(1,0).await; }
    #[tokio::test] async fn eof_then_failure_preserves_failure() { joined(0,1).await; }
}
