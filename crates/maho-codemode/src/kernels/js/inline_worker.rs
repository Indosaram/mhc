use std::path::{Path, PathBuf};
use crate::kernels::shared::{runtime_asset::{CodemodeRuntimeAssetEnvironment, CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset}, subprocess_process::ProcessError};
use super::worker_host::{WorkerHost, JavaScriptKernelMode};

pub fn resolve_inline_worker_entry_path(local_path: Option<&Path>, environment: &CodemodeRuntimeAssetEnvironment<'_>) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(local_path.unwrap_or(Path::new(concat!(env!("CARGO_MANIFEST_DIR"),"/assets/kernels/js/inline-worker-entry.js"))), Path::new("kernels/js/inline-worker-entry.js"), environment)
}

pub fn create_inline_worker(cwd: &Path, parallel_pool_width: u64, environment: &CodemodeRuntimeAssetEnvironment<'_>) -> Result<WorkerHost, ProcessError> {
    let entry = resolve_inline_worker_entry_path(None, environment).map_err(|error|ProcessError::Startup(error.to_string()))?;
    WorkerHost::spawn(&entry, cwd, parallel_pool_width, JavaScriptKernelMode::Inline)
}
