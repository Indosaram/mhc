pub const CONTINUATION_CAP_BLOCKED_REASON: &str = "continuation cap reached";
pub const REPETITION_BLOCKED_REASON: &str = "repeated assistant output";
pub const LENGTH_EXHAUSTED_BLOCKED_REASON: &str = "output truncation repeated";
pub const UNATTENDED_CONTINUATION_BLOCKED_REASON: &str = "unattended continuation limit reached";
pub const PROVIDER_ERROR_BLOCKED_REASON: &str = "provider error ended the turn (retries exhausted)";
pub const CONTEXT_OVERFLOW_BLOCKED_REASON: &str = "context overflow ended the turn (compaction did not recover)";
pub fn is_mechanical_continuation_block(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| [CONTINUATION_CAP_BLOCKED_REASON, REPETITION_BLOCKED_REASON, LENGTH_EXHAUSTED_BLOCKED_REASON, UNATTENDED_CONTINUATION_BLOCKED_REASON, PROVIDER_ERROR_BLOCKED_REASON, CONTEXT_OVERFLOW_BLOCKED_REASON].contains(&reason))
}
pub fn continuation_cap_recovery_hint(reason: &str) -> String {
    if is_mechanical_continuation_block(Some(reason)) { format!("Goal continuation blocked: {reason}. Send any message to resume.") } else { format!("Goal continuation blocked: {reason}") }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn runtime_guard_reasons_are_mechanical() { let result = [CONTINUATION_CAP_BLOCKED_REASON, REPETITION_BLOCKED_REASON, LENGTH_EXHAUSTED_BLOCKED_REASON, UNATTENDED_CONTINUATION_BLOCKED_REASON, PROVIDER_ERROR_BLOCKED_REASON, CONTEXT_OVERFLOW_BLOCKED_REASON].map(|reason| is_mechanical_continuation_block(Some(reason))); assert!(result.into_iter().all(|value| value)); }
    #[test] fn unknown_reason_and_absence_are_not_mechanical() { let result = [is_mechanical_continuation_block(None), is_mechanical_continuation_block(Some("waiting for credentials"))]; assert_eq!(result, [false, false]); }
}
