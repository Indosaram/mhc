use super::subprocess_contract::{KernelRunInput, SubprocessKernelOptions};
use super::subprocess_process::{ProcessError, SubprocessProcess};
use crate::kernels::session_env::apply_session_environment;
use serde_json::{Value, json};
use std::time::Duration;

pub struct SubprocessKernel {
    options: SubprocessKernelOptions,
    process: Option<SubprocessProcess>,
    closed: bool,
}

impl SubprocessKernel {
    pub async fn start(options: SubprocessKernelOptions) -> Result<Self, ProcessError> {
        let mut kernel = Self { options, process: None, closed: false };
        kernel.spawn_process().await?;
        Ok(kernel)
    }

    async fn spawn_process(&mut self) -> Result<(), ProcessError> {
        let inherited = std::env::vars().collect();
        let env = self.options.env.clone().unwrap_or_else(|| apply_session_environment(&inherited, self.options.session_env.as_ref()));
        let mut process = SubprocessProcess::spawn(&self.options.command, &self.options.args, &self.options.cwd, &env)?;
        process.send(&json!({"type":"init","sessionId":self.options.session_id,"connection":self.options.connection})).await?;
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = process.next_message().await?;
                match message["type"].as_str() {
                    Some("ready") => return Ok(()),
                    Some("init-failed") => return Err(ProcessError::Startup(message["error"]["message"].as_str().unwrap_or("initialization failed").into())),
                    _ => {}
                }
            }
        }).await;
        match ready {
            Ok(Ok(())) => { self.process = Some(process); Ok(()) }
            Ok(Err(error)) => { process.terminate("TERM", Duration::from_millis(1500)).await?; Err(error) }
            Err(_) => { process.terminate("TERM", Duration::from_millis(1500)).await?; Err(ProcessError::Startup("Kernel did not become ready".into())) }
        }
    }

    pub fn pid(&self) -> Option<u32> { self.process.as_ref().and_then(SubprocessProcess::pid) }

    pub async fn run(&mut self, input: KernelRunInput, mut on_message: impl FnMut(&Value)) -> Result<Value, ProcessError> {
        if self.closed { return Err(ProcessError::Closed); }
        let process = self.process.as_mut().ok_or(ProcessError::Closed)?;
        let mut frame = json!({"type":"run","cellId":input.cell_id,"code":input.code});
        if let Some(timeout_ms) = input.timeout_ms { frame["timeoutMs"] = timeout_ms.into(); }
        process.send(&frame).await?;
        let operation = async {
            loop {
                let message = process.next_message().await?;
                on_message(&message);
                if message["type"] == "result" && message["cellId"] == input.cell_id { return Ok(message); }
            }
        };
        if let Some(timeout_ms) = input.timeout_ms {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), operation).await {
                Ok(result) => result,
                Err(_) => {
                    self.reset().await?;
                    Ok(json!({"type":"result","cellId":input.cell_id,"ok":false,"error":{"message":format!("Cell timed out after {timeout_ms}ms")},"durationMs":timeout_ms}))
                }
            }
        } else { operation.await }
    }

    pub async fn reset(&mut self) -> Result<(), ProcessError> {
        if self.closed { return Err(ProcessError::Closed); }
        if let Some(process) = self.process.as_mut() { process.terminate("TERM", Duration::from_millis(1500)).await?; }
        self.process = None;
        self.spawn_process().await
    }

    pub async fn close(&mut self) -> Result<(), ProcessError> {
        self.closed = true;
        if let Some(process) = self.process.as_mut() { process.shutdown(Some(&json!({"type":"close"}))).await?; }
        self.process = None;
        Ok(())
    }
}
