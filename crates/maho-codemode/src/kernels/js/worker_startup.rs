use super::{
    inline_worker::create_inline_worker,
    worker_host::{JavaScriptKernelMode, WorkerHost},
};
use crate::{
    bridge::protocol::{BridgeConnectionConfig, KernelMemoryThresholds},
    kernels::shared::{
        runtime_asset::{CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset},
        subprocess_process::ProcessError,
    },
};
use std::path::{Path, PathBuf};

pub fn resolve_js_worker_entry_path(
    local_path: Option<&Path>,
) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(local_path, Path::new("kernels/js/worker-entry.js"))
}

/// senpi `spawnWorker` (worker-startup.ts:51): `options.workerEntryUrl ?? resolveJsWorkerEntryUrl()`; an
/// explicit entry is spawned as given, so a missing one falls back to the inline worker.
pub fn explicit_or_resolved_worker_entry(
    options: &WorkerStartupOptions<'_>,
) -> Result<PathBuf, ProcessError> {
    match options.worker_entry {
        Some(entry) => Ok(entry.to_path_buf()),
        None => resolve_js_worker_entry_path(None)
            .map_err(|error| ProcessError::Startup(error.to_string())),
    }
}

pub struct WorkerStartupOptions<'a> {
    pub isolation: crate::config::feature_settings::JsIsolation,
    pub process_startup_deadline: std::time::Duration,
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

pub async fn start_worker_with_inline_fallback(
    options: &WorkerStartupOptions<'_>,
    signal: &maho_ai::utils::abort::AbortSignal,
) -> Result<WorkerHost, ProcessError> {
    if signal.aborted() {
        return Err(ProcessError::Startup(
            "JavaScript worker startup was cancelled".into(),
        ));
    }
    if options.isolation == crate::config::feature_settings::JsIsolation::Process {
        let mut worker = super::process_worker::spawn_process_worker(options).await?;
        let initialized = with_startup_deadline(
            worker.initialize(options, signal),
            options.process_startup_deadline,
        )
        .await;
        match initialized {
            Ok(()) => return Ok(worker),
            Err(error) => {
                worker.terminate().await?;
                return Err(error);
            }
        }
    }
    let primary = explicit_or_resolved_worker_entry(options).and_then(|entry| {
        WorkerHost::spawn(
            &entry,
            options.cwd,
            options.parallel_pool_width,
            JavaScriptKernelMode::Worker,
        )
    });
    if let Ok(mut worker) = primary {
        match worker.initialize(options, signal).await {
            Ok(()) => return Ok(worker),
            Err(error) => {
                worker.terminate().await?;
                if signal.aborted() {
                    return Err(error);
                }
            }
        }
    }
    if signal.aborted() {
        return Err(ProcessError::Startup(
            "JavaScript worker startup was cancelled".into(),
        ));
    }
    let mut worker = create_inline_worker(options.cwd, options.parallel_pool_width)?;
    if let Err(error) = worker.initialize(options, signal).await {
        worker.terminate().await?;
        return Err(error);
    }
    Ok(worker)
}

async fn with_startup_deadline(
    ready: impl std::future::Future<Output = Result<(), ProcessError>>,
    deadline: std::time::Duration,
) -> Result<(), ProcessError> {
    tokio::time::timeout(deadline, ready).await.map_err(|_| ProcessError::Startup(format!(
        "JavaScript kernel process did not become ready within {}s; it was stopped, and the next cell starts a fresh one",
        deadline.as_secs_f64()
    )))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn wedged_process_readiness_expires_at_startup_deadline() {
        let before = tokio::time::Instant::now();
        let deadline = std::time::Duration::from_secs(30);
        let result = with_startup_deadline(std::future::pending(), deadline).await;
        assert!(matches!(result, Err(ProcessError::Startup(_))));
        assert_eq!(before.elapsed(), deadline);
    }
}
