use super::worker_host::{JavaScriptKernelMode, WorkerHost};
use crate::kernels::shared::{
    runtime_asset::{CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset},
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
    create_inline_worker_with_env(cwd, parallel_pool_width, env_passthrough, &[])
}

pub fn create_inline_worker_with_env(
    cwd: &Path,
    parallel_pool_width: u64,
    env_passthrough: Option<&[String]>,
    extra_env: &[(&str, &str)],
) -> Result<WorkerHost, ProcessError> {
    let entry = resolve_inline_worker_entry_path(None)
        .map_err(|error| ProcessError::Startup(error.to_string()))?;
    WorkerHost::spawn_scrubbed_with_env(
        &entry,
        cwd,
        parallel_pool_width,
        JavaScriptKernelMode::Inline,
        env_passthrough,
        extra_env,
    )
}
