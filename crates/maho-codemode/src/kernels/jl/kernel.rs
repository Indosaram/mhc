use crate::kernels::shared::subprocess_contract::SubprocessKernelOptions;
use crate::kernels::shared::subprocess_kernel::SubprocessKernel;
use crate::kernels::shared::subprocess_process::ProcessError;
use std::path::{Path, PathBuf};

pub fn resolve_julia_runner_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kernels/jl/runner.jl")
}

pub fn resolve_julia_runner_path_with(local_path: Option<&Path>, environment: &crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetEnvironment<'_>) -> Result<PathBuf,crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetMissingError> {
    crate::kernels::shared::runtime_asset::require_codemode_runtime_asset(local_path.unwrap_or(&resolve_julia_runner_path()),Path::new("kernels/jl/runner.jl"),environment)
}

pub struct JuliaKernel;

impl JuliaKernel {
    pub fn arguments() -> Vec<String> {
        let mut args: Vec<String> = ["--startup-file=no", "--history-file=no", "--color=no", "--compile=min", "--optimize=0"].into_iter().map(String::from).collect();
        args.push(resolve_julia_runner_path().to_string_lossy().into_owned());
        args
    }
    pub async fn start(options: SubprocessKernelOptions) -> Result<SubprocessKernel, ProcessError> {
        Self::start_with_signal(options,&maho_ai::utils::abort::AbortController::new().signal()).await
    }
    pub async fn start_with_signal(mut options: SubprocessKernelOptions,signal:&maho_ai::utils::abort::AbortSignal) -> Result<SubprocessKernel, ProcessError> {
        if options.command.is_empty() { options.command = "julia".into(); }
        options.args = Self::arguments();
        let executable=std::env::current_exe()?;
        let runner=resolve_julia_runner_path_with(None,&crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&executable}).map_err(|error|ProcessError::Startup(error.to_string()))?;
        *options.args.last_mut().expect("Julia runner argument")=runner.to_string_lossy().into_owned();
        SubprocessKernel::start_with_signal(options,signal).await
    }
}
