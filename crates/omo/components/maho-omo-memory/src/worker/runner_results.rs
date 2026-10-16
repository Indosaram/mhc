use memory_core::reflection::{machine::ReflectionOutcome, worktree::ReflectionFinalizeResult};

pub fn failure_reason(result: &ReflectionFinalizeResult) -> Option<&'static str> {
    match result.status {
        ReflectionOutcome::DirtyUncommitted => Some("completion_validation"),
        ReflectionOutcome::ParentDirty | ReflectionOutcome::MergeConflict => Some("integration_failed"),
        ReflectionOutcome::Failed => {
            let detail = result.detail.as_deref().unwrap_or("").to_lowercase();
            Some(if detail.starts_with("secret_like_content:") { "secret_like_content" } else if ["git administration", "recorded launch sha", "changed paths", "no head commit", "escapes the memory repository"].iter().any(|part| detail.contains(part)) { "completion_validation" } else { "integration_failed" })
        }
        ReflectionOutcome::Merged | ReflectionOutcome::NoChanges | ReflectionOutcome::TimedOut => None,
    }
}

pub fn cleanup_succeeded(result: &ReflectionFinalizeResult) -> bool { result.cleanup.worktree_removed && result.cleanup.branch_removed }
pub fn error_message(error: &dyn std::fmt::Display) -> String { error.to_string() }
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_like_content_failure_has_distinct_reason() {
        let result = ReflectionFinalizeResult {
            status: ReflectionOutcome::Failed,
            detail: Some("secret_like_content: detected credential".into()),
            cleanup: memory_core::reflection::worktree::ReflectionCleanupReceipt { worktree_removed: true, branch_removed: true },
        };
        assert_eq!(failure_reason(&result), Some("secret_like_content"));
    }
}
