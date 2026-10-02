use serde_json::Value;
use crate::kernels::shared::subprocess_process::ProcessError;
use super::{worker_host::{WorkerHost, JavaScriptKernelMode}, worker_startup::{WorkerStartupOptions, start_worker_with_inline_fallback}, interrupt_bounds::{WorkerRetirement, WORKER_TERMINATE_DEADLINE_MS}};

pub struct WorkerSlot {
    worker: Option<WorkerHost>,
    mode: JavaScriptKernelMode,
    generation: u64,
}

impl Default for WorkerSlot {
    fn default() -> Self { Self {worker:None,mode:JavaScriptKernelMode::Worker,generation:0} }
}

impl WorkerSlot {
    pub fn mode(&self) -> JavaScriptKernelMode { self.mode }
    pub fn present(&self) -> bool { self.worker.is_some() }
    pub fn generation(&self) -> u64 { self.generation }
    pub async fn ensure_ready(&mut self, mut options: WorkerStartupOptions<'_>, signal: &maho_ai::utils::abort::AbortSignal) -> Result<(), ProcessError> {
        if self.worker.is_some() { return Ok(()); }
        if self.generation == 0 { self.generation = 1; }
        options.generation = self.generation;
        let worker = start_worker_with_inline_fallback(&options, signal).await?;
        self.mode = worker.mode;
        self.worker = Some(worker);
        Ok(())
    }
    pub async fn post_message(&mut self, message: &Value) -> Result<(), ProcessError> {
        if let Some(worker) = &mut self.worker { worker.post_message(message).await?; }
        Ok(())
    }
    pub async fn next_message(&mut self) -> Result<Value, ProcessError> { self.worker.as_mut().ok_or(ProcessError::Closed)?.next_message().await }
    pub async fn retire(&mut self) -> Result<WorkerRetirement, ProcessError> {
        self.generation += 1;
        let Some(mut worker) = self.worker.take() else { return Ok(WorkerRetirement::Terminated); };
        match tokio::time::timeout(std::time::Duration::from_millis(WORKER_TERMINATE_DEADLINE_MS), worker.terminate()).await {
            Ok(result) => { result?; Ok(WorkerRetirement::Terminated) },
            Err(_) => Ok(WorkerRetirement::Abandoned),
        }
    }
}
