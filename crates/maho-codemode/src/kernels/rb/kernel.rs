use crate::kernels::shared::subprocess_contract::SubprocessKernelOptions;
use crate::kernels::shared::subprocess_kernel::SubprocessKernel;
use crate::kernels::shared::subprocess_process::ProcessError;
use std::path::{Path, PathBuf};

pub fn resolve_ruby_runner_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kernels/rb/runner.rb")
}

pub struct RubyKernel;

impl RubyKernel {
    pub async fn start(mut options: SubprocessKernelOptions) -> Result<SubprocessKernel, ProcessError> {
        if options.command.is_empty() { options.command = "ruby".into(); }
        options.args = vec![resolve_ruby_runner_path().to_string_lossy().into_owned()];
        SubprocessKernel::start(options).await
    }
}
