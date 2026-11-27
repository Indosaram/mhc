//! Port of `packages/machine-ops/src/policy/filesystem.ts`.

use crate::errors::MachineOpError;
use crate::files::bounded_realpath::{
    is_missing_path_error, symlink_loop_error, with_resolution_deadline,
};
use crate::files::realpath_without_open::realpath_without_open_strict;
use crate::types::OpFuture;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemOperation {
    Read,
    Enumerate,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemPolicyRequest {
    pub operation: FilesystemOperation,
    pub canonical_path: PathBuf,
    pub tool_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilesystemPolicyDecision {
    Allow,
    Deny { reason: String },
}

pub type FilesystemPolicyChecker = Arc<
    dyn Fn(FilesystemPolicyRequest) -> OpFuture<'static, FilesystemPolicyDecision> + Send + Sync,
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

fn canonicalize_missing(path: &Path, depth: usize) -> std::io::Result<PathBuf> {
    if depth > 40 {
        return Err(symlink_loop_error());
    }
    match std::fs::canonicalize(path) {
        Ok(path) => return Ok(path),
        Err(error) if is_missing_path_error(&error) => {}
        Err(error) => return Err(error),
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => {
            let target = std::fs::read_link(path)?;
            return canonicalize_missing(
                &path.parent().unwrap_or(Path::new("/")).join(target),
                depth + 1,
            );
        }
        Ok(_) => {}
        Err(error) if is_missing_path_error(&error) => {}
        Err(error) => return Err(error),
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok(canonicalize_missing(parent, depth)?.join(name)),
        _ => Ok(path.to_path_buf()),
    }
}

pub async fn canonicalize_filesystem_path(path: &Path) -> Result<PathBuf, MachineOpError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let target = path.clone();
    let task = tokio::task::spawn_blocking(move || canonicalize_missing(&target, 0));
    match with_resolution_deadline(task).await {
        Some(Ok(Ok(resolved))) => Ok(resolved),
        Some(Ok(Err(error))) => Err(MachineOpError::from_io(
            &error,
            Some("realpath"),
            Some(&path),
        )),
        Some(Err(join_error)) => Err(MachineOpError::new(
            crate::errors::OpErrorCode::IoError,
            join_error.to_string(),
        )),
        None => realpath_without_open_strict(&path)
            .map_err(|error| MachineOpError::from_io(&error, Some("realpath"), Some(&path))),
    }
}
