use crate::kernels::shared::subprocess_contract::SubprocessKernelOptions;
use crate::kernels::shared::subprocess_kernel::SubprocessKernel;
use crate::kernels::shared::subprocess_process::ProcessError;
use std::path::{Path, PathBuf};

pub fn resolve_ruby_runner_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kernels/rb/runner.rb")
}

pub fn resolve_ruby_runner_path_with(local_path: Option<&Path>, environment: &crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetEnvironment<'_>) -> Result<PathBuf,crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetMissingError> {
    crate::kernels::shared::runtime_asset::require_codemode_runtime_asset(local_path.unwrap_or(&resolve_ruby_runner_path()),Path::new("kernels/rb/runner.rb"),environment)
}

pub struct RubyKernel;

impl RubyKernel {
    pub async fn start(mut options: SubprocessKernelOptions) -> Result<SubprocessKernel, ProcessError> {
        if options.command.is_empty() { options.command = "ruby".into(); }
        let executable=std::env::current_exe()?;
        let runner=resolve_ruby_runner_path_with(None,&crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&executable}).map_err(|error|ProcessError::Startup(error.to_string()))?;
        options.args = vec![runner.to_string_lossy().into_owned()];
        SubprocessKernel::start(options).await
    }
}
