use crate::bridge::protocol::BridgeConnectionConfig;
use crate::kernels::session_env::SessionEnvironment;
use crate::kernels::shared::subprocess_contract::{KernelRunInput, SubprocessKernelOptions};
use crate::kernels::shared::subprocess_kernel::SubprocessKernel;
use crate::kernels::shared::subprocess_process::ProcessError;
use serde_json::Value;
use std::path::Path;

pub struct JavaScriptKernel {
    kernel: SubprocessKernel,
}

impl JavaScriptKernel {
    pub async fn start(cwd: &Path, session_id: &str, parallel_pool_width: u64, session_env: Option<SessionEnvironment>) -> Result<Self, ProcessError> {
        let worker = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-entry.js");
        let relay = format!(
            "import {{ Worker }} from 'node:worker_threads'; import {{ createInterface }} from 'node:readline'; const worker = new Worker({}, {{ workerData: {{ cwd: {}, parallelPoolWidth: {} }} }}); worker.on('message', message => process.stdout.write(JSON.stringify(message)+'\\n')); worker.on('error', error => {{ process.stdout.write(JSON.stringify({{type:'init-failed',error:{{message:error.message}}}})+'\\n'); process.exitCode=1; }}); worker.on('exit', code => process.exit(code)); const input=createInterface({{input:process.stdin}}); input.on('line', line => worker.postMessage(JSON.parse(line))); input.on('close', () => worker.terminate());",
            serde_json::to_string(worker)?, serde_json::to_string(&cwd.to_string_lossy())?, parallel_pool_width,
        );
        let kernel = SubprocessKernel::start(SubprocessKernelOptions {
            command: "bun".into(), args: vec!["-e".into(), relay], cwd: cwd.into(), env: None, session_env,
            session_id: session_id.into(), connection: BridgeConnectionConfig {
                port: 1, token: "worker-transport".into(), local_roots: None, artifacts_dir: None, parallel_pool_width: Some(parallel_pool_width),
            },
        }).await?;
        Ok(Self { kernel })
    }

    pub async fn run(&mut self, input: KernelRunInput, on_message: impl FnMut(&Value)) -> Result<Value, ProcessError> {
        self.kernel.run(input, on_message).await
    }
    pub async fn reset(&mut self) -> Result<(), ProcessError> { self.kernel.reset().await }
    pub async fn close(&mut self) -> Result<(), ProcessError> { self.kernel.close().await }
    pub fn pid(&self) -> Option<u32> { self.kernel.pid() }
}
