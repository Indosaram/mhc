use serde_json::Value;
use crate::kernels::shared::subprocess_process::ProcessError;
use super::{worker_host::{WorkerHost, JavaScriptKernelMode}, worker_startup::{WorkerStartupOptions, resolve_js_worker_entry_path}, interrupt_bounds::{WorkerRetirement, WORKER_TERMINATE_DEADLINE_MS}};

pub struct WorkerSlot {
    worker: Option<WorkerHost>,
    mode: JavaScriptKernelMode,
    generation: u64,
    child_pids: std::collections::HashSet<u32>,
}

impl Default for WorkerSlot {
    fn default() -> Self { Self {worker:None,mode:JavaScriptKernelMode::Worker,generation:0,child_pids:Default::default()} }
}

impl WorkerSlot {
    pub fn mode(&self) -> JavaScriptKernelMode { self.mode }
    pub fn present(&self) -> bool { self.worker.is_some() }
    pub fn pid(&self) -> Option<u32> { self.worker.as_ref().and_then(WorkerHost::pid) }
    pub fn generation(&self) -> u64 { self.generation }
    pub async fn ensure_ready(&mut self, mut options: WorkerStartupOptions<'_>, signal: &maho_ai::utils::abort::AbortSignal) -> Result<(), ProcessError> {
        if self.worker.is_some() { return Ok(()); }
        if self.generation == 0 { self.generation = 1; }
        options.generation = self.generation;
        if signal.aborted() {return Err(ProcessError::Startup("JavaScript worker startup was cancelled".into()));}
        let primary=resolve_js_worker_entry_path(options.worker_entry,&options.environment).map_err(|error|ProcessError::Startup(error.to_string())).and_then(|entry|WorkerHost::spawn(&entry,options.cwd,options.parallel_pool_width,JavaScriptKernelMode::Worker));
        if let Ok(worker)=primary {
            self.worker=Some(worker);
            let initialized=self.worker.as_mut().expect("published worker").initialize(&options,signal).await;
            if initialized.is_ok() {self.mode=JavaScriptKernelMode::Worker;return Ok(());}
            self.retire().await?;
            if signal.aborted() {return initialized;}
            options.generation=self.generation;
        }
        self.worker=Some(super::inline_worker::create_inline_worker(options.cwd,options.parallel_pool_width,&options.environment)?);
        self.mode=JavaScriptKernelMode::Inline;
        if let Err(error)=self.worker.as_mut().expect("published inline worker").initialize(&options,signal).await {self.retire().await?;return Err(error);}
        Ok(())
    }
    pub async fn post_message(&mut self, message: &Value) -> Result<(), ProcessError> {
        if let Some(worker) = &mut self.worker { worker.post_message(message).await?; }
        Ok(())
    }
    pub async fn next_message(&mut self) -> Result<Value, ProcessError> {
        let message=self.worker.as_mut().ok_or(ProcessError::Closed)?.next_message().await?;
        if message["type"]=="status" && message["event"]["op"]==crate::bridge::reserved::CHILD_LIFECYCLE_OP
            && let Some(pid)=message["event"]["pid"].as_u64().and_then(|pid|u32::try_from(pid).ok()).filter(|pid|*pid>0) {
            match message["event"]["state"].as_str() {
                Some("spawned")=>{self.child_pids.insert(pid);}
                Some("exited")=>{self.child_pids.remove(&pid);}
                _=>{},
            }
        }
        Ok(message)
    }
    pub async fn retire(&mut self) -> Result<WorkerRetirement, ProcessError> {
        self.generation += 1;
        let Some(mut worker) = self.worker.take() else { return Ok(WorkerRetirement::Terminated); };
        let children=self.child_pids.drain().collect::<Vec<_>>();
        if let Some(owner)=worker.pid() {
            super::process_tree_host::terminate_process_trees(&children,super::process_tree_host::TerminateProcessTreesOptions {grace_ms:1000,kill_wait_ms:None,owner_pid:Some(owner)}).await;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(WORKER_TERMINATE_DEADLINE_MS), worker.terminate()).await {
            Ok(result) => { result?; Ok(WorkerRetirement::Terminated) },
            Err(_) => { worker.terminate().await?; Ok(WorkerRetirement::Terminated) },
        }
    }
}
