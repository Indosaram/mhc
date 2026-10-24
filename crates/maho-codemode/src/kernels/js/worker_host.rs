use crate::kernels::shared::subprocess_process::{ProcessError, SubprocessProcess};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaScriptKernelMode {
    Worker,
    Inline,
    Process,
}

pub struct WorkerHost {
    pub mode: JavaScriptKernelMode,
    process: SubprocessProcess,
}

/// SAFETY-DIVERGENCE(senpi tools/bash.ts): drop secret-looking variables from the env the eval kernel
/// child receives, only when `env_passthrough` is `Some`.
pub fn scrub_host_env(
    env: &mut std::collections::HashMap<String, String>,
    env_passthrough: Option<&[String]>,
) {
    if let Some(passthrough) = env_passthrough {
        let mut scrubbed: std::collections::BTreeMap<_, _> = env.drain().collect();
        maho_process_env::env_scrub::scrub_secrets(&mut scrubbed, passthrough);
        env.extend(scrubbed);
    }
}

pub fn error_from_bridge(error: &Value) -> ProcessError {
    ProcessError::WorkerFailure {
        message: error["message"]
            .as_str()
            .unwrap_or("JavaScript worker failed")
            .into(),
        name: error["name"].as_str().map(str::to_owned),
        stack: error["stack"].as_str().map(str::to_owned),
    }
}

pub fn bridge_error(error: &ProcessError) -> Value {
    match error {
        ProcessError::WorkerFailure {
            message,
            name,
            stack,
        } => {
            let mut value = json!({"message":message});
            if let Some(name) = name {
                value["name"] = json!(name);
            }
            if let Some(stack) = stack {
                value["stack"] = json!(stack);
            }
            value
        }
        ProcessError::ExitedWithStatus(status) => {
            let code = status.code().unwrap_or(-1);
            #[cfg(unix)]
            let signal = {
                use std::os::unix::process::ExitStatusExt;
                status.signal()
            };
            #[cfg(not(unix))]
            let signal: Option<i32> = None;
            let message = match signal {
                Some(signal) => {
                    format!("JavaScript worker exited with signal {signal} (code {code})")
                }
                None => format!("JavaScript worker exited with code {code}"),
            };
            json!({"message":message,"name":"JavaScriptWorkerExitedError"})
        }
        ProcessError::Io(_)
        | ProcessError::Retirement
        | ProcessError::Closed
        | ProcessError::Startup(_)
        | ProcessError::Exited
        | ProcessError::Json(_) => {
            json!({"message":error.to_string(),"name":"Error"})
        }
    }
}

pub fn crashed_result(cell_id: &str, error: &ProcessError, duration_ms: f64) -> Value {
    json!({"type":"result","cellId":cell_id,"ok":false,"error":bridge_error(error),"durationMs":duration_ms})
}

impl WorkerHost {
    pub(crate) fn from_process(process: SubprocessProcess) -> Self {
        Self {
            mode: JavaScriptKernelMode::Process,
            process,
        }
    }
    pub fn spawn(
        entry: &Path,
        cwd: &Path,
        parallel_pool_width: u64,
        mode: JavaScriptKernelMode,
    ) -> Result<Self, ProcessError> {
        Self::spawn_scrubbed(entry, cwd, parallel_pool_width, mode, None)
    }
    /// Like [`Self::spawn`], dropping secret-looking host variables when `env_passthrough` is `Some`.
    pub fn spawn_scrubbed(
        entry: &Path,
        cwd: &Path,
        parallel_pool_width: u64,
        mode: JavaScriptKernelMode,
        env_passthrough: Option<&[String]>,
    ) -> Result<Self, ProcessError> {
        let relay = format!(
            "import {{ Worker }} from 'node:worker_threads'; import {{ createInterface }} from 'node:readline'; const worker = new Worker({}, {{ workerData: {{ cwd: {}, parallelPoolWidth: {} }} }}); worker.on('message', message => process.stdout.write(JSON.stringify(message)+'\\n')); worker.on('error', error => {{ process.stdout.write(JSON.stringify({{type:'init-failed',error:{{message:error.message,name:error.name,stack:error.stack}}}})+'\\n'); process.exitCode=1; }}); worker.on('exit', code => process.stdout.write('', () => process.exit(code))); const input=createInterface({{input:process.stdin}}); input.on('line', line => worker.postMessage(JSON.parse(line))); input.on('close', () => worker.terminate());",
            serde_json::to_string(&entry.to_string_lossy())?,
            serde_json::to_string(&cwd.to_string_lossy())?,
            parallel_pool_width,
        );
        // A senpi worker thread inherits the host `process.env`, extension writes included.
        let mut env = maho_ext_api::process_env::vars()
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        scrub_host_env(&mut env, env_passthrough);
        let process = SubprocessProcess::spawn("bun", &["-e".into(), relay], cwd, &env)?;
        Ok(Self { mode, process })
    }

    pub fn pid(&self) -> Option<u32> {
        self.process.pid()
    }
    pub async fn post_message(&mut self, message: &Value) -> Result<(), ProcessError> {
        self.process.send(message).await.map(|_| ())
    }
    pub async fn next_message(&mut self) -> Result<Value, ProcessError> {
        let message = self.process.next_message().await?;
        if message["type"] == "init-failed" {
            return Err(error_from_bridge(&message["error"]));
        }
        Ok(message)
    }
    pub async fn terminate(&mut self) -> Result<(), ProcessError> {
        self.process
            .terminate("TERM", Duration::from_millis(1500))
            .await
    }

    pub async fn initialize(
        &mut self,
        options: &super::worker_startup::WorkerStartupOptions<'_>,
        signal: &maho_ai::utils::abort::AbortSignal,
    ) -> Result<(), ProcessError> {
        let mut frame = json!({"type":"init","sessionId":options.session_id,"connection":options.connection,"kernelGeneration":options.generation,"hostToolNames":options.host_tool_names,"foreignLanguageNames":options.foreign_language_names});
        if let Some(environment) = options.session_env {
            frame["sessionEnv"] = serde_json::to_value(environment)?;
        }
        if let Some(memory) = options.memory {
            frame["memory"] = serde_json::to_value(memory)?;
        }
        self.post_message(&frame).await?;
        loop {
            let message = tokio::select! {
                biased;
                () = signal.cancelled() => return Err(ProcessError::Startup("JavaScript worker startup was cancelled".into())),
                message = self.next_message() => message?,
            };
            match message["type"].as_str() {
                Some("ready") => return Ok(()),
                Some("init-failed") => {
                    return Err(ProcessError::Startup(
                        message["error"]["message"]
                            .as_str()
                            .unwrap_or("JavaScript worker initialization failed")
                            .into(),
                    ));
                }
                _ => {}
            }
        }
    }
}
