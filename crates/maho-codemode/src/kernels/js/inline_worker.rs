use std::path::{Path, PathBuf};
use crate::kernels::shared::{runtime_asset::{CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset}, subprocess_process::ProcessError};
use super::worker_host::{WorkerHost, JavaScriptKernelMode};

pub fn resolve_inline_worker_entry_path(local_path: Option<&Path>) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(local_path, Path::new("kernels/js/inline-worker-entry.js"))
}

pub fn create_inline_worker(cwd: &Path, parallel_pool_width: u64) -> Result<WorkerHost, ProcessError> {
    let entry = resolve_inline_worker_entry_path(None).map_err(|error|ProcessError::Startup(error.to_string()))?;
    WorkerHost::spawn(&entry, cwd, parallel_pool_width, JavaScriptKernelMode::Inline)
}
