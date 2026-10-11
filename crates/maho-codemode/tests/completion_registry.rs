use std::{future::Future, sync::Arc};
use maho_codemode::bridges::handles::CompletionHandleRegistry;
use maho_ext_api::{EvalHandleOutcome, EvalHandlePhase};

fn deadline() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_secs_f64()*1000.0+60_000.0 }

#[tokio::test]
async fn committed_completion_wakes_watch_and_close_ends_pending_stream() {
    let registry=Arc::new(CompletionHandleRegistry::default());
    let (release, released)=tokio::sync::oneshot::channel();
    let reference=registry.start(deadline(),move |_|Box::pin(async move { released.await.expect("completion release"); Ok(serde_json::json!("settled")) })).expect("start");
    let mut watch=registry.watch(std::slice::from_ref(&reference)).expect("watch");
    assert_eq!(watch.initial[0].phase,EvalHandlePhase::Pending);
    assert_eq!(registry.output_text(&reference).expect("pending output"), "");
    release.send(()).expect("release");
    let update=tokio::time::timeout(std::time::Duration::from_secs(5),std::future::poll_fn(|cx|watch.updates.as_mut().poll_next(cx))).await.expect("update bound").expect("update");
    assert_eq!(update.phase,EvalHandlePhase::Succeeded);
    assert!(matches!(registry.result(&reference).expect("result"),EvalHandleOutcome::Fulfilled{value,..} if value=="settled"));
    let next=std::future::poll_fn(|cx|watch.updates.as_mut().poll_next(cx));
    tokio::pin!(next);
    assert!(std::future::poll_fn(|cx|std::task::Poll::Ready(next.as_mut().poll(cx))).await.is_pending());
    (watch.close)();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5),next).await.expect("close wake bound").is_none());
    registry.dispose();
}

#[tokio::test]
async fn disposal_closes_registered_watch_and_drops_active_completion_future() {
    struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Dropped { fn drop(&mut self){if let Some(sender)=self.0.take(){sender.send(()).expect("drop receiver");}} }
    let registry=Arc::new(CompletionHandleRegistry::default());
    let (entered,enter)=tokio::sync::oneshot::channel();
    let (dropped,drop_received)=tokio::sync::oneshot::channel();
    let reference=registry.start(deadline(),move |_|Box::pin(async move {let _guard=Dropped(Some(dropped));entered.send(()).expect("entered receiver");std::future::pending().await})).expect("start");
    let mut watch=registry.watch(std::slice::from_ref(&reference)).expect("watch");
    tokio::time::timeout(std::time::Duration::from_secs(5),enter).await.expect("enter bound").expect("entered");
    registry.dispose();
    tokio::time::timeout(std::time::Duration::from_secs(5),drop_received).await.expect("drop bound").expect("dropped");
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5),std::future::poll_fn(|cx|watch.updates.as_mut().poll_next(cx))).await.expect("dispose wake bound").is_none());
    assert_eq!(registry.result(&reference).expect_err("disposed reference").code,"eval_handle_stale");
}
