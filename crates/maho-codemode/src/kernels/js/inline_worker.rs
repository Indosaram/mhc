use super::worker_host::{JavaScriptKernelMode, WorkerHost};
use crate::kernels::shared::{
    runtime_asset::{require_codemode_runtime_asset, CodemodeRuntimeAssetMissingError},
    subprocess_process::ProcessError,
};
use std::path::{Path, PathBuf};

pub fn resolve_inline_worker_entry_path(
    local_path: Option<&Path>,
) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(local_path, Path::new("kernels/js/inline-worker-entry.js"))
}

pub fn create_inline_worker(
    cwd: &Path,
    parallel_pool_width: u64,
) -> Result<WorkerHost, ProcessError> {
    create_inline_worker_scrubbed(cwd, parallel_pool_width, None)
}

pub fn create_inline_worker_scrubbed(
    cwd: &Path,
    parallel_pool_width: u64,
    env_passthrough: Option<&[String]>,
) -> Result<WorkerHost, ProcessError> {
    let entry = resolve_inline_worker_entry_path(None)
        .map_err(|error| ProcessError::Startup(error.to_string()))?;
    WorkerHost::spawn_scrubbed(
        &entry,
        cwd,
        parallel_pool_width,
        JavaScriptKernelMode::Inline,
        env_passthrough,
    )
}
