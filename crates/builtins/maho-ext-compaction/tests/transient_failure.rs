use maho_ext_compaction::transient_failure::{SummarizationFailure, is_transient_summarization_failure};

#[test]
fn refusal_metadata_overrides_retryable_text() {
    let error = SummarizationFailure::SummaryRequest { transient: false };
    let transient = is_transient_summarization_failure(error, "503 service unavailable");
    assert!(!transient);
}

#[test]
fn infrastructure_budgets_degrade_without_retryable_text() {
    for error in [SummarizationFailure::StreamDurationBudget, SummarizationFailure::StreamIdleTimeout,
        SummarizationFailure::TotalBudget, SummarizationFailure::OverflowExhausted] {
        let transient = is_transient_summarization_failure(error, "unrecognized failure");
        assert!(transient);
    }
}

#[test]
fn unknown_errors_use_shared_retry_classification() {
    let transient = is_transient_summarization_failure(SummarizationFailure::Other, "503 service unavailable");
    assert!(transient);
}

#[test]
fn provider_metadata_can_mark_non_retryable_text_transient() {
    let transient = is_transient_summarization_failure(SummarizationFailure::SummaryRequest { transient: true }, "unrecognized failure");
    assert!(transient);
}
