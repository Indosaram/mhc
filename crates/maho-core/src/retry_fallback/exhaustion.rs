//! Port of senpi `packages/coding-agent/src/core/retry-fallback/exhaustion.ts`.

use super::controller::FallbackReason;

/// One chain rung that failed during a turn's fallback walk: the model it ran on and its provider error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackRungFailure {
    pub model: String,
    pub reason: FallbackReason,
    pub error: String,
}

/// Appends a rung failure unless it repeats the last one (one failure can be offered to the walk twice).
pub fn record_rung_failure(failures: &mut Vec<FallbackRungFailure>, failure: FallbackRungFailure) {
    if let Some(last) = failures.last()
        && last.model == failure.model
        && last.error == failure.error
    {
        return;
    }
    failures.push(failure);
}

/// The exhaustion text every surface shows: the failure that started the walk first, then each later rung
/// (senpi#4084). Without recorded rungs it falls back to the last error, as older events carried only that.
pub fn describe_fallback_exhaustion(last_error: &str, failures: &[FallbackRungFailure]) -> String {
    if failures.is_empty() {
        return last_error.to_owned();
    }
    failures
        .iter()
        .enumerate()
        .map(|(index, failure)| {
            format!(
                "{}{}: {}",
                if index == 0 { "" } else { "then " },
                failure.model,
                failure.error
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
