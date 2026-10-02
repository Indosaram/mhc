use std::{path::Path, time::Duration};
use serde_json::{Value, json};
use crate::kernels::shared::subprocess_process::{SubprocessProcess, ProcessError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaScriptKernelMode { Worker, Inline }

pub struct WorkerHost {
    pub mode: JavaScriptKernelMode,
    process: SubprocessProcess,
}

impl WorkerHost {
    pub fn spawn(entry: &Path, cwd: &Path, parallel_pool_width: u64, mode: JavaScriptKernelMode) -> Result<Self, ProcessError> {
        let relay = format!(
            "import {{ Worker }} from 'node:worker_threads'; import {{ createInterface }} from 'node:readline'; const worker = new Worker({}, {{ workerData: {{ cwd: {}, parallelPoolWidth: {} }} }}); worker.on('message', message => process.stdout.write(JSON.stringify(message)+'\\n')); worker.on('error', error => {{ process.stdout.write(JSON.stringify({{type:'init-failed',error:{{message:error.message,name:error.name,stack:error.stack}}}})+'\\n'); process.exitCode=1; }}); worker.on('exit', code => process.exit(code)); const input=createInterface({{input:process.stdin}}); input.on('line', line => worker.postMessage(JSON.parse(line))); input.on('close', () => worker.terminate());",
            serde_json::to_string(&entry.to_string_lossy())?, serde_json::to_string(&cwd.to_string_lossy())?, parallel_pool_width,
        );
        let env = std::env::vars().collect();
        let process = SubprocessProcess::spawn("bun", &["-e".into(),relay], cwd, &env)?;
        Ok(Self { mode, process })
    }

    pub fn pid(&self) -> Option<u32> { self.process.pid() }
    pub async fn post_message(&mut self, message: &Value) -> Result<(), ProcessError> { self.process.send(message).await.map(|_|()) }
    pub async fn next_message(&mut self) -> Result<Value, ProcessError> { self.process.next_message().await }
    pub async fn terminate(&mut self) -> Result<(), ProcessError> { self.process.terminate("TERM", Duration::from_millis(1500)).await }

    pub async fn initialize(&mut self, options: &super::worker_startup::WorkerStartupOptions<'_>, signal: &maho_ai::utils::abort::AbortSignal) -> Result<(), ProcessError> {
        let mut frame=json!({"type":"init","sessionId":options.session_id,"connection":options.connection,"kernelGeneration":options.generation,"hostToolNames":options.host_tool_names,"foreignLanguageNames":options.foreign_language_names});
        if let Some(environment)=options.session_env {frame["sessionEnv"]=serde_json::to_value(environment)?;}
        self.post_message(&frame).await?;
        loop {
            let message = tokio::select! {
                biased;
                () = signal.cancelled() => return Err(ProcessError::Startup("JavaScript worker startup was cancelled".into())),
                message = self.next_message() => message?,
            };
            match message["type"].as_str() {
                Some("ready") => return Ok(()),
                Some("init-failed") => return Err(ProcessError::Startup(message["error"]["message"].as_str().unwrap_or("JavaScript worker initialization failed").into())),
                _ => {}
            }
        }
    }
}
