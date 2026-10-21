pub use crate::kernels::shared::kernel_death::unstarted_result;
use crate::kernels::shared::{
    subprocess_contract::KernelRunInput,
    subprocess_run::{KernelMessageCallback, KernelStartedCallback},
};
use serde_json::{Value, json};
use std::collections::VecDeque;

pub struct PendingJavaScriptRun {
    pub input: KernelRunInput,
    pub prepare: Option<PrepareCell>,
    pub host_executor: Option<crate::tool::types::HostCellExecutor>,
    pub on_started: Option<KernelStartedCallback>,
    pub on_message: Option<KernelMessageCallback>,
    pub started_at_ms: Option<f64>,
    pub settled: bool,
    pub interrupt_result: Option<Value>,
    pub interrupt_ack: Option<tokio::sync::oneshot::Sender<()>>,
    pub settled_by_worker: bool,
    settlement: tokio::sync::watch::Sender<Option<Result<Value, String>>>,
}

pub type PrepareCell = Box<dyn FnOnce() -> Result<String, String> + Send>;

impl PendingJavaScriptRun {
    pub fn settlement(&self) -> tokio::sync::watch::Receiver<Option<Result<Value, String>>> {
        self.settlement.subscribe()
    }
}

#[derive(Default)]
pub struct JavaScriptRunQueue {
    pub memory: Option<super::kernel_memory_bridge::KernelMemoryBridge>,
    queue: VecDeque<PendingJavaScriptRun>,
    active: Option<PendingJavaScriptRun>,
}

pub fn stopped_result(cell_id: &str, message: &str) -> Value {
    json!({"type":"result", "cellId":cell_id,"ok":false,"error":{"message":message},"durationMs":0})
}

impl JavaScriptRunQueue {
    pub fn with_memory(memory: super::kernel_memory_bridge::KernelMemoryBridge) -> Self {
        Self {
            memory: Some(memory),
            ..Self::default()
        }
    }
    pub fn active(&self) -> Option<&PendingJavaScriptRun> {
        self.active.as_ref()
    }
    pub fn active_mut(&mut self) -> Option<&mut PendingJavaScriptRun> {
        self.active.as_mut()
    }
    pub fn has_waiting(&self) -> bool {
        !self.queue.is_empty()
    }
    pub fn set_last_host(&mut self, host: Option<crate::tool::types::HostCellExecutor>) {
        if let Some(run) = self.queue.back_mut() {
            run.host_executor = host;
        }
    }

    pub fn enqueue(
        &mut self,
        input: KernelRunInput,
        on_started: Option<KernelStartedCallback>,
        on_message: Option<KernelMessageCallback>,
    ) -> tokio::sync::watch::Receiver<Option<Result<Value, String>>> {
        self.enqueue_prepared(input, on_started, on_message, None)
    }

    pub fn enqueue_prepared(
        &mut self,
        input: KernelRunInput,
        on_started: Option<KernelStartedCallback>,
        on_message: Option<KernelMessageCallback>,
        prepare: Option<PrepareCell>,
    ) -> tokio::sync::watch::Receiver<Option<Result<Value, String>>> {
        let (settlement, receiver) = tokio::sync::watch::channel(None);
        self.queue.push_back(PendingJavaScriptRun {
            input,
            prepare,
            host_executor: None,
            on_started,
            on_message,
            started_at_ms: None,
            settled: false,
            interrupt_result: None,
            interrupt_ack: None,
            settled_by_worker: false,
            settlement,
        });
        receiver
    }

    pub fn start_next(&mut self, started_at_ms: f64) -> Option<&PendingJavaScriptRun> {
        if self.active.is_some() {
            return None;
        }
        self.active = self.queue.pop_front();
        if let Some(run) = &mut self.active {
            run.started_at_ms = Some(started_at_ms);
            if let Some(callback) = &run.on_started {
                callback();
            }
        }
        self.active.as_ref()
    }

    pub fn remove(&mut self, cell_id: &str, reason: &str) -> bool {
        let Some(index) = self
            .queue
            .iter()
            .position(|run| run.input.cell_id == cell_id)
        else {
            return false;
        };
        let Some(mut run) = self.queue.remove(index) else {
            return false;
        };
        Self::settle(&mut run, unstarted_result(cell_id, reason));
        true
    }

    pub fn snapshot(&self) -> (Option<String>, Vec<String>) {
        (
            self.active.as_ref().map(|run| run.input.cell_id.clone()),
            self.queue
                .iter()
                .map(|run| run.input.cell_id.clone())
                .collect(),
        )
    }

    pub fn duration_ms(run: &PendingJavaScriptRun, finished_at_ms: f64) -> f64 {
        run.started_at_ms.map_or(0.0, |start| {
            ((finished_at_ms - start) + 0.5).floor().max(0.0)
        })
    }

    pub fn release_active(&mut self) -> Option<PendingJavaScriptRun> {
        self.active.take()
    }

    pub fn settle_crashed(
        run: &mut PendingJavaScriptRun,
        error: &crate::kernels::shared::subprocess_process::ProcessError,
        finished_at_ms: f64,
    ) {
        let result = run.interrupt_result.take().unwrap_or_else(|| {
            super::worker_host::crashed_result(
                &run.input.cell_id,
                error,
                Self::duration_ms(run, finished_at_ms),
            )
        });
        Self::settle(run, result);
    }

    pub fn settle(run: &mut PendingJavaScriptRun, result: Value) {
        if run.settled {
            return;
        }
        run.settled = true;
        run.settlement.send_replace(Some(Ok(result)));
    }

    pub fn settle_all(&mut self, message: &str) {
        if let Some(mut active) = self.active.take() {
            let result = active
                .interrupt_result
                .take()
                .unwrap_or_else(|| stopped_result(&active.input.cell_id, message));
            Self::settle(&mut active, result);
        }
        for mut run in self.queue.drain(..) {
            let result = unstarted_result(&run.input.cell_id, message);
            Self::settle(&mut run, result);
        }
    }

    pub fn reject_waiting(&mut self, error: &str) {
        for mut run in self.queue.drain(..) {
            if run.settled {
                continue;
            }
            run.settled = true;
            run.settlement.send_replace(Some(Err(error.into())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: &str) -> KernelRunInput {
        KernelRunInput {
            cell_id: id.into(),
            code: String::new(),
            timeout_ms: None,
        }
    }

    #[test]
    fn removed_cell_is_not_run_and_preparation_is_never_called() {
        let mut queue = JavaScriptRunQueue::default();
        let result = queue.enqueue_prepared(
            input("queued"),
            None,
            None,
            Some(Box::new(|| panic!("removed source resolved"))),
        );
        assert!(queue.remove("queued", "cancelled"));
        assert_eq!(
            result
                .borrow()
                .as_ref()
                .expect("settled")
                .as_ref()
                .expect("result")["kernelState"],
            "not-run"
        );
    }

    #[test]
    fn reset_distinguishes_active_from_unstarted_cells() {
        let mut queue = JavaScriptRunQueue::default();
        let active = queue.enqueue(input("active"), None, None);
        let queued = queue.enqueue(input("queued"), None, None);
        queue.start_next(0.0);
        queue.settle_all("reset");
        assert!(
            active
                .borrow()
                .as_ref()
                .expect("settled")
                .as_ref()
                .expect("result")
                .get("kernelState")
                .is_none()
        );
        assert_eq!(
            queued
                .borrow()
                .as_ref()
                .expect("settled")
                .as_ref()
                .expect("result")["kernelState"],
            "not-run"
        );
    }
}
