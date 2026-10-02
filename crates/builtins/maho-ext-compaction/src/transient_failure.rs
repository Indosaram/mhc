//! Metadata takes precedence over retry-looking provider error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummarizationFailure {
    StreamDurationBudget,
    StreamIdleTimeout,
    TotalBudget,
    SummaryRequest { transient: bool },
    OverflowExhausted,
    Other,
}

pub fn is_transient_summarization_failure(error: SummarizationFailure, message: &str) -> bool {
    match error {
        SummarizationFailure::StreamDurationBudget
        | SummarizationFailure::StreamIdleTimeout
        | SummarizationFailure::TotalBudget
        | SummarizationFailure::OverflowExhausted => true,
        SummarizationFailure::SummaryRequest { transient } => transient,
        SummarizationFailure::Other => maho_ai::utils::retry::is_retryable_error_message(message),
    }
}
