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

#[test]
fn idle_failure_retains_watchdog_classification_without_retry_looking_text() {
    use maho_ext_compaction::{speculative::SummaryGenerationError, speculative_summary::SummaryStreamError,
        speculative_job::LiveSummaryFailure, transient_failure::is_transient_summarization_failure};
    for error in [SummaryGenerationError::Stream(SummaryStreamError::IdleTimeout),
        SummaryGenerationError::Stream(SummaryStreamError::DurationBudget), SummaryGenerationError::TotalBudget] {
        let failure = LiveSummaryFailure::from(error);
        assert!(is_transient_summarization_failure(failure.classification, &failure.message));
    }
}

#[test]
fn idle_failure_retains_refusal_metadata_over_retry_looking_text() {
    use maho_ext_compaction::{speculative::SummaryGenerationError, speculative_job::LiveSummaryFailure,
        transient_failure::is_transient_summarization_failure};
    let model = serde_json::from_value(serde_json::json!({"id":"m","name":"m","provider":"faux","api":"faux","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":1000})).unwrap();
    let mut response = maho_ai::utils::lazy::setup_error_message(&model, "503 service unavailable");
    response.stop_details = Some(maho_ai::types::AssistantStopDetails::Sensitive);
    let failure = LiveSummaryFailure::from(SummaryGenerationError::Request(Box::new(response)));
    assert!(!is_transient_summarization_failure(failure.classification, &failure.message));
}
