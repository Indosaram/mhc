//! Extension policy contracts and canonical paths (`maho-machine-ops` re-exports + `ToolFuture` adapter).

use crate::definition::{ToolError, ToolFuture};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub use maho_machine_ops::policy::filesystem::{
    FilesystemOperation, FilesystemPolicyDecision, FilesystemPolicyRequest,
};

pub type FilesystemPolicyChecker = Arc<
    dyn Fn(FilesystemPolicyRequest) -> ToolFuture<'static, FilesystemPolicyDecision> + Send + Sync,
>;

#[derive(Clone)]
pub struct FilesystemPolicy {
    pub check: FilesystemPolicyChecker,
    pub denied_roots: Option<Vec<PathBuf>>,
}

pub fn compose_filesystem_policies(
    policies: Vec<FilesystemPolicy>,
) -> Option<FilesystemPolicyChecker> {
    if policies.is_empty() {
        return None;
    }
    let policies = Arc::new(policies);
    Some(Arc::new(move |request| {
        let policies = Arc::clone(&policies);
        Box::pin(async move {
            for policy in policies.iter() {
                let decision = (policy.check)(request.clone()).await?;
                if matches!(decision, FilesystemPolicyDecision::Deny { .. }) {
                    return Ok(decision);
                }
            }
            Ok(FilesystemPolicyDecision::Allow)
        })
    }))
}

pub async fn canonicalize_filesystem_path(path: &Path) -> Result<PathBuf, ToolError> {
    maho_machine_ops::canonicalize_filesystem_path(path)
        .await
        .map_err(crate::fs_error_code::tool_error_from_machine)
}

pub async fn check_filesystem_policy(
    checker: Option<&FilesystemPolicyChecker>,
    path: &Path,
    operation: FilesystemOperation,
    tool: &str,
) -> Result<(), ToolError> {
    if let Some(check) = checker {
        let request = FilesystemPolicyRequest {
            operation,
            canonical_path: canonicalize_filesystem_path(path).await?,
            tool_name: tool.into(),
        };
        match check(request).await? {
            FilesystemPolicyDecision::Allow => {}
            FilesystemPolicyDecision::Deny { reason } => return Err(ToolError::Message(reason)),
        }
    }
    Ok(())
}
