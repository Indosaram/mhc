use std::path::{Path, PathBuf};
use crate::{bridge::protocol::BridgeConnectionConfig, kernels::shared::{runtime_asset::{CodemodeRuntimeAssetEnvironment, CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset}, subprocess_process::ProcessError}};
use super::{worker_host::{WorkerHost, JavaScriptKernelMode}, inline_worker::create_inline_worker};

pub fn resolve_js_worker_entry_path(local_path: Option<&Path>, environment: &CodemodeRuntimeAssetEnvironment<'_>) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(local_path.unwrap_or(Path::new(concat!(env!("CARGO_MANIFEST_DIR"),"/assets/kernels/js/worker-entry.js"))), Path::new("kernels/js/worker-entry.js"), environment)
}

pub struct WorkerStartupOptions<'a> {
    pub cwd: &'a Path,
    pub session_id: &'a str,
    pub parallel_pool_width: u64,
    pub connection: &'a BridgeConnectionConfig,
    pub generation: u64,
    pub host_tool_names: &'a [String],
    pub foreign_language_names: &'a [String],
    pub worker_entry: Option<&'a Path>,
    pub environment: CodemodeRuntimeAssetEnvironment<'a>,
}

pub async fn start_worker_with_inline_fallback(options: &WorkerStartupOptions<'_>, signal: &maho_ai::utils::abort::AbortSignal) -> Result<WorkerHost, ProcessError> {
    let primary = resolve_js_worker_entry_path(options.worker_entry, &options.environment).map_err(|error|ProcessError::Startup(error.to_string()))
        .and_then(|entry|WorkerHost::spawn(&entry, options.cwd, options.parallel_pool_width, JavaScriptKernelMode::Worker));
    if let Ok(mut worker) = primary {
        match worker.initialize(options.session_id, options.connection, options.generation, options.host_tool_names, options.foreign_language_names, signal).await {
            Ok(()) => return Ok(worker),
            Err(error) => { worker.terminate().await?; if signal.aborted() { return Err(error); } }
        }
    }
    if signal.aborted() { return Err(ProcessError::Startup("JavaScript worker startup was cancelled".into())); }
    let mut worker = create_inline_worker(options.cwd, options.parallel_pool_width, &options.environment)?;
    if let Err(error) = worker.initialize(options.session_id, options.connection, options.generation, options.host_tool_names, options.foreign_language_names, signal).await {
        worker.terminate().await?;
        return Err(error);
    }
    Ok(worker)
}
