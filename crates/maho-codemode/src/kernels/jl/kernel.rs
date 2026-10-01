use crate::kernels::shared::subprocess_contract::SubprocessKernelOptions;
use crate::kernels::shared::subprocess_kernel::SubprocessKernel;
use crate::kernels::shared::subprocess_process::ProcessError;
use std::path::{Path, PathBuf};

pub fn resolve_julia_runner_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kernels/jl/runner.jl")
}

pub struct JuliaKernel;

impl JuliaKernel {
    pub fn arguments() -> Vec<String> {
        let mut args: Vec<String> = ["--startup-file=no", "--history-file=no", "--color=no", "--compile=min", "--optimize=0"].into_iter().map(String::from).collect();
        args.push(resolve_julia_runner_path().to_string_lossy().into_owned());
        args
    }
    pub async fn start(mut options: SubprocessKernelOptions) -> Result<SubprocessKernel, ProcessError> {
        if options.command.is_empty() { options.command = "julia".into(); }
        options.args = Self::arguments();
        SubprocessKernel::start(options).await
    }
}
