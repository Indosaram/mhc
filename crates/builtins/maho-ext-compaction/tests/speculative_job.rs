use maho_ai::utils::abort::AbortController;
use maho_ext_compaction::speculative_job::{JobSettlement, track_speculative_job};

#[tokio::test]
async fn settlement_is_shared_and_completion_follows_result() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let job = track_speculative_job(4, "snapshot", AbortController::new(), async move { receiver.await.unwrap() }, 100);
    assert!(!job.completed());
    sender.send(JobSettlement::<u64, String> { result: Some(7), error: None }).unwrap();
    let first = job.settled().await;
    let second = job.settled().await;
    assert_eq!(first.result, Some(7));
    assert_eq!(second.result, Some(7));
    assert!(job.completed());
}

#[tokio::test]
async fn failed_generation_preserves_error_without_result() {
    let job = track_speculative_job(4, (), AbortController::new(), async { JobSettlement::<u64, _> { result: None, error: Some("failed") } }, 100);
    let outcome = job.settled().await;
    assert_eq!(outcome.error, Some("failed"));
    assert_eq!(outcome.result, None);
}
