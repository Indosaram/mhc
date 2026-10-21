use maho_ai::utils::event_stream::AssistantMessageEventStream;
use maho_ext_btw::side_query::collect_reply;

#[tokio::test]
async fn establishment_timeout_awaits_stream_close() {
    let stream = AssistantMessageEventStream::assistant();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
        entered_tx.send(()).expect("close subscriber");
        release_rx.await.expect("release");
        Ok(())
    })));
    let task = tokio::spawn(async move { collect_reply(&stream, 0, |_| {}).await });
    let bound = std::time::Duration::from_secs(5);
    tokio::time::timeout(bound, entered_rx).await.expect("close deadline").expect("close");
    assert!(!task.is_finished());
    release_tx.send(()).expect("release subscriber");
    let result = tokio::time::timeout(bound, task).await.expect("result deadline").expect("task");
    assert!(result.is_err());
}
