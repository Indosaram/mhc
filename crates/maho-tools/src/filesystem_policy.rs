//! Extension policy contracts and canonical paths for existing and new targets.
use std::{path::{Path, PathBuf}, sync::Arc};
use serde::{Deserialize, Serialize};
use crate::definition::{ToolError, ToolFuture};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemOperation { Read, Enumerate, Write }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemPolicyRequest {
    pub operation: FilesystemOperation,
    pub canonical_path: PathBuf,
    pub tool_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesystemPolicyDecision { Allow, Deny { reason: String } }
pub type FilesystemPolicyChecker = Arc<dyn Fn(FilesystemPolicyRequest) -> ToolFuture<'static, FilesystemPolicyDecision> + Send + Sync>;
#[derive(Clone)]
pub struct FilesystemPolicy {
    pub check: FilesystemPolicyChecker,
    pub denied_roots: Option<Vec<PathBuf>>,
}
pub fn compose_filesystem_policies(policies: Vec<FilesystemPolicy>) -> Option<FilesystemPolicyChecker> {
    if policies.is_empty() { return None; }
    let policies = Arc::new(policies);
    Some(Arc::new(move |request| {
        let policies = Arc::clone(&policies);
        Box::pin(async move {
            for policy in policies.iter() {
                let decision = (policy.check)(request.clone()).await?;
                if matches!(decision, FilesystemPolicyDecision::Deny { .. }) { return Ok(decision); }
            }
            Ok(FilesystemPolicyDecision::Allow)
        })
    }))
}

fn canonicalize_missing(path: &Path, depth: usize) -> std::io::Result<PathBuf> {
    if depth > 40 { return Err(std::io::Error::from_raw_os_error(40)); }
    match std::fs::canonicalize(path) {
        Ok(path) => return Ok(path),
        Err(error) if crate::bounded_realpath::is_missing_path_error(&error) => {},
        Err(error) => return Err(error),
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => {
            let target = std::fs::read_link(path)?;
            return canonicalize_missing(&path.parent().unwrap_or(Path::new("/")).join(target), depth + 1);
        }
        Ok(_) => {},
        Err(error) if crate::bounded_realpath::is_missing_path_error(&error) => {},
        Err(error) => return Err(error),
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok(canonicalize_missing(parent, depth)?.join(name)),
        _ => Ok(path.to_path_buf()),
    }
}
pub async fn canonicalize_filesystem_path(path: &Path) -> Result<PathBuf, ToolError> {
    let path = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let target = path.clone();
    let task = tokio::task::spawn_blocking(move || canonicalize_missing(&target, 0));
    match crate::bounded_realpath::with_resolution_deadline(task).await {
        Some(result) => Ok(result.map_err(|e| ToolError::Message(e.to_string()))??),
        None => Ok(crate::path_utils::realpath_without_open_strict(&path)?),
    }
}
pub async fn check_filesystem_policy(checker: Option<&FilesystemPolicyChecker>, path: &Path, operation: FilesystemOperation, tool: &str) -> Result<(), ToolError> {
    if let Some(check) = checker {
        let request = FilesystemPolicyRequest { operation, canonical_path: canonicalize_filesystem_path(path).await?, tool_name: tool.into() };
        match check(request).await? {
            FilesystemPolicyDecision::Allow => {},
            FilesystemPolicyDecision::Deny { reason } => return Err(ToolError::Message(reason)),
        }
    }
    Ok(())
}
