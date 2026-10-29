#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceApplyEditConcurrentPhase {
    Applying,
    Settled,
}

const APPLYING_REASON: &str =
    "workspace/applyEdit is already in progress for this workspace mutation";
const SETTLED_REASON: &str = "workspace/applyEdit was already handled for this workspace mutation";

pub const CANONICAL_CONCURRENT_WORKSPACE_APPLY_EDIT_FAILURE_REASON: &str =
    "workspace/applyEdit concurrent for this workspace mutation";

pub fn workspace_apply_edit_concurrent_failure_reason(
    phase: WorkspaceApplyEditConcurrentPhase,
) -> &'static str {
    match phase {
        WorkspaceApplyEditConcurrentPhase::Applying => APPLYING_REASON,
        WorkspaceApplyEditConcurrentPhase::Settled => SETTLED_REASON,
    }
}

pub fn canonicalize_workspace_apply_edit_failure_reason(reason: &str) -> String {
    if reason == APPLYING_REASON || reason == SETTLED_REASON {
        CANONICAL_CONCURRENT_WORKSPACE_APPLY_EDIT_FAILURE_REASON.to_string()
    } else {
        reason.to_string()
    }
}
