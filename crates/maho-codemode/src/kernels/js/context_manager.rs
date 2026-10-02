use crate::bridge::protocol::BridgeConnectionConfig;
use crate::kernels::session_env::SessionEnvironment;
use crate::kernels::shared::subprocess_contract::{KernelRunInput, SubprocessKernelOptions};
use crate::kernels::shared::subprocess_kernel::SubprocessKernel;
use crate::kernels::shared::subprocess_process::ProcessError;
use serde_json::Value;
use std::path::Path;
use super::local_module_loader::{LocalModuleLoader, LocalModuleLoaderOptions, PREPARED_CELL_PREFIX};

pub struct JavaScriptKernel {
    kernel: SubprocessKernel,
    loader: LocalModuleLoader,
}

impl JavaScriptKernel {
    pub async fn start(cwd: &Path, session_id: &str, parallel_pool_width: u64, session_env: Option<SessionEnvironment>) -> Result<Self, ProcessError> {
        let loader = LocalModuleLoader::new(&LocalModuleLoaderOptions { cwd:cwd.into(),local_roots:None,artifacts_dir:None })?;
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
        Ok(Self { kernel, loader })
    }

    pub async fn run(&self, mut input: KernelRunInput, on_message: impl FnMut(&Value)) -> Result<Value, ProcessError> {
        if !input.code.starts_with(PREPARED_CELL_PREFIX) { input.code = self.loader.prepare_cell(&input.code); }
        self.kernel.run(input, on_message).await
    }
    pub async fn run_with_callbacks(&self, mut input: super::super::shared::subprocess_contract::KernelRunInput, on_message: Option<crate::kernels::shared::subprocess_run::KernelMessageCallback>, on_started: Option<crate::kernels::shared::subprocess_run::KernelStartedCallback>) -> Result<Value,ProcessError> {
        if !input.code.starts_with(PREPARED_CELL_PREFIX) { input.code=self.loader.prepare_cell(&input.code); }
        self.kernel.run_with_callbacks(input,on_message,on_started).await
    }
    pub fn queue_snapshot(&self)->(Option<String>,Vec<String>) {self.kernel.queue_snapshot()}
    pub async fn cancel_queued(&self,id:&str,reason:&str)->bool {self.kernel.cancel_queued(id,reason).await}
    pub fn deliver_tool_reply(&self,message:Value)->Result<(),String> {self.kernel.deliver_tool_reply(message)}
    pub async fn reset(&self) -> Result<(), ProcessError> { self.kernel.reset().await }
    pub async fn close(&self) -> Result<(), ProcessError> { self.kernel.close().await }
    pub fn pid(&self) -> Option<u32> { self.kernel.pid() }
}
