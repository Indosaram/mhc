use std::path::{Path, PathBuf};
use crate::{bridge::protocol::{BridgeConnectionConfig, KernelMemoryThresholds}, kernels::shared::{runtime_asset::{CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset}, subprocess_process::ProcessError}};
use super::{worker_host::{WorkerHost, JavaScriptKernelMode}, inline_worker::create_inline_worker};

pub fn resolve_js_worker_entry_path(local_path: Option<&Path>) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(local_path, Path::new("kernels/js/worker-entry.js"))
}

/// senpi `spawnWorker` (worker-startup.ts:51): `options.workerEntryUrl ?? resolveJsWorkerEntryUrl()`; an
/// explicit entry is spawned as given, so a missing one falls back to the inline worker.
pub fn explicit_or_resolved_worker_entry(options: &WorkerStartupOptions<'_>) -> Result<PathBuf, ProcessError> {
    match options.worker_entry {
        Some(entry) => Ok(entry.to_path_buf()),
        None => resolve_js_worker_entry_path(None).map_err(|error| ProcessError::Startup(error.to_string())),
    }
}

pub struct WorkerStartupOptions<'a> {
    pub cwd: &'a Path,
    pub session_id: &'a str,
    pub parallel_pool_width: u64,
    pub connection: &'a BridgeConnectionConfig,
    pub generation: u64,
    pub host_tool_names: &'a [String],
    pub foreign_language_names: &'a [String],
    pub session_env: Option<&'a crate::kernels::session_env::SessionEnvironment>,
    pub worker_entry: Option<&'a Path>,
    pub memory: Option<&'a KernelMemoryThresholds>,
}

pub async fn start_worker_with_inline_fallback(options: &WorkerStartupOptions<'_>, signal: &maho_ai::utils::abort::AbortSignal) -> Result<WorkerHost, ProcessError> {
    if signal.aborted() { return Err(ProcessError::Startup("JavaScript worker startup was cancelled".into())); }
    let primary = explicit_or_resolved_worker_entry(options)
        .and_then(|entry|WorkerHost::spawn(&entry, options.cwd, options.parallel_pool_width, JavaScriptKernelMode::Worker));
    if let Ok(mut worker) = primary {
        match worker.initialize(options, signal).await {
            Ok(()) => return Ok(worker),
            Err(error) => { worker.terminate().await?; if signal.aborted() { return Err(error); } }
        }
    }
    if signal.aborted() { return Err(ProcessError::Startup("JavaScript worker startup was cancelled".into())); }
    let mut worker = create_inline_worker(options.cwd, options.parallel_pool_width)?;
    if let Err(error) = worker.initialize(options, signal).await {
        worker.terminate().await?;
        return Err(error);
    }
    Ok(worker)
}
