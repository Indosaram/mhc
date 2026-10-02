use std::collections::VecDeque;
use serde_json::{Value, json};
use crate::kernels::shared::{subprocess_contract::KernelRunInput, subprocess_run::{KernelMessageCallback, KernelStartedCallback}};

pub struct PendingJavaScriptRun {
    pub input: KernelRunInput,
    pub on_started: Option<KernelStartedCallback>,
    pub on_message: Option<KernelMessageCallback>,
    pub started_at_ms: Option<f64>,
    pub settled: bool,
    pub interrupt_result: Option<Value>,
    pub interrupt_ack: Option<tokio::sync::oneshot::Sender<()>>,
    pub settled_by_worker: bool,
    settlement: tokio::sync::watch::Sender<Option<Result<Value, String>>>,
}

impl PendingJavaScriptRun {
    pub fn settlement(&self) -> tokio::sync::watch::Receiver<Option<Result<Value, String>>> { self.settlement.subscribe() }
}

#[derive(Default)]
pub struct JavaScriptRunQueue {
    queue: VecDeque<PendingJavaScriptRun>,
    active: Option<PendingJavaScriptRun>,
}

pub fn stopped_result(cell_id: &str, message: &str) -> Value {
    json!({"type":"result", "cellId":cell_id,"ok":false,"error":{"message":message},"durationMs":0})
}

impl JavaScriptRunQueue {
    pub fn active(&self) -> Option<&PendingJavaScriptRun> { self.active.as_ref() }
    pub fn active_mut(&mut self) -> Option<&mut PendingJavaScriptRun> { self.active.as_mut() }
    pub fn has_waiting(&self) -> bool { !self.queue.is_empty() }

    pub fn enqueue(&mut self, input: KernelRunInput, on_started: Option<KernelStartedCallback>, on_message: Option<KernelMessageCallback>) -> tokio::sync::watch::Receiver<Option<Result<Value, String>>> {
        let (settlement, receiver) = tokio::sync::watch::channel(None);
        self.queue.push_back(PendingJavaScriptRun { input, on_started, on_message, started_at_ms: None, settled: false, interrupt_result: None, interrupt_ack: None, settled_by_worker: false, settlement });
        receiver
    }

    pub fn start_next(&mut self, started_at_ms: f64) -> Option<&PendingJavaScriptRun> {
        if self.active.is_some() { return None; }
        self.active = self.queue.pop_front();
        if let Some(run) = &mut self.active {
            run.started_at_ms = Some(started_at_ms);
            if let Some(callback) = &run.on_started { callback(); }
        }
        self.active.as_ref()
    }

    pub fn remove(&mut self, cell_id: &str, reason: &str) -> bool {
        let Some(index) = self.queue.iter().position(|run| run.input.cell_id == cell_id) else { return false; };
        let Some(mut run) = self.queue.remove(index) else { return false; };
        Self::settle(&mut run, stopped_result(cell_id, reason));
        true
    }

    pub fn snapshot(&self) -> (Option<String>, Vec<String>) {
        (self.active.as_ref().map(|run| run.input.cell_id.clone()), self.queue.iter().map(|run| run.input.cell_id.clone()).collect())
    }

    pub fn duration_ms(run: &PendingJavaScriptRun, finished_at_ms: f64) -> f64 {
        run.started_at_ms.map_or(0.0, |start| ((finished_at_ms - start) + 0.5).floor().max(0.0))
    }

    pub fn release_active(&mut self) -> Option<PendingJavaScriptRun> { self.active.take() }

    pub fn settle(run: &mut PendingJavaScriptRun, result: Value) {
        if run.settled { return; }
        run.settled = true;
        run.settlement.send_replace(Some(Ok(result)));
    }

    pub fn settle_all(&mut self, message: &str) {
        if let Some(mut active) = self.active.take() {
            let result = active.interrupt_result.take().unwrap_or_else(|| stopped_result(&active.input.cell_id, message));
            Self::settle(&mut active, result);
        }
        for mut run in self.queue.drain(..) {
            let result = stopped_result(&run.input.cell_id, message);
            Self::settle(&mut run, result);
        }
    }

    pub fn reject_waiting(&mut self, error: &str) {
        for mut run in self.queue.drain(..) {
            if run.settled { continue; }
            run.settled = true;
            run.settlement.send_replace(Some(Err(error.into())));
        }
    }
}
