use std::{sync::Arc, time::Duration};
use maho_codemode::tool::{detached_cell_contract::*, detached_notification_queue::*, types::EvalLanguage};
use maho_ext_api::AgentToolResult;

fn snapshot(id: &str) -> EvalDetachedCellSnapshot {
    EvalDetachedCellSnapshot {cell_id:id.into(),language:EvalLanguage::Js,started_at_ms:0.0,state:EvalDetachedCellState::Completed,queued_behind:None,output_tail:String::new(),result:AgentToolResult::text("ok"),state_retained:None,interrupt_note:None,hard_limit_seconds:None,run_budget_seconds:None}
}

#[tokio::test]
async fn concurrent_snapshots_are_delivered_in_enqueue_order() {
    let (delivered_tx,mut delivered_rx)=tokio::sync::mpsc::unbounded_channel();
    let mut queue=DetachedNotificationQueue::new(Some(Arc::new(move |batch|{delivered_tx.send(batch).unwrap();Ok(())})));
    let (first_tx,first_rx)=tokio::sync::oneshot::channel();
    let (second_started_tx,second_started_rx)=tokio::sync::oneshot::channel();
    queue.enqueue(PendingDetachedNotification{snapshot:Box::new(move ||Box::pin(async move{first_rx.await.unwrap();snapshot("first")})),spill_path:None});
    queue.enqueue(PendingDetachedNotification{snapshot:Box::new(move ||Box::pin(async move{second_started_tx.send(()).unwrap();snapshot("second")})),spill_path:None});
    tokio::time::timeout(Duration::from_secs(2),second_started_rx).await.unwrap().unwrap();
    assert!(delivered_rx.try_recv().is_err());
    first_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2),queue.flush()).await.unwrap().unwrap();
    let batch=tokio::time::timeout(Duration::from_secs(2),delivered_rx.recv()).await.unwrap().unwrap();
    assert_eq!(batch.iter().map(|cell|cell.cell_id.as_str()).collect::<Vec<_>>(),vec!["first","second"]);
}

#[tokio::test]
async fn flush_propagates_current_batch_failure() {
    let (release_tx,release_rx)=tokio::sync::oneshot::channel();
    let queue_notifier=Arc::new(|_|Err("delivery failed".into()));
    let mut queue=DetachedNotificationQueue::new(Some(queue_notifier));
    queue.enqueue(PendingDetachedNotification{snapshot:Box::new(move ||Box::pin(async move{release_rx.await.unwrap();snapshot("cell")})),spill_path:None});
    let flush=queue.flush();
    tokio::pin!(flush);
    assert!(matches!(std::future::poll_fn(|cx|std::task::Poll::Ready(flush.as_mut().poll(cx))).await,std::task::Poll::Pending));
    release_tx.send(()).unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(2),flush).await.unwrap(),Err("delivery failed".into()));
}

use std::future::Future;
