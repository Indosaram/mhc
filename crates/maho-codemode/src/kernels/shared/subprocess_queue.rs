use std::collections::VecDeque;
use serde_json::Value;
use super::{subprocess_contract::KernelRunInput, subprocess_run::*};

#[derive(Default)]
pub struct SubprocessRunQueue {
    queue: VecDeque<PendingRun>,
    active: Option<PendingRun>,
    pending_calls: VecDeque<Value>,
    call_waiters: VecDeque<tokio::sync::oneshot::Sender<Value>>,
}

impl SubprocessRunQueue {
    pub fn active(&self) -> Option<&PendingRun> { self.active.as_ref() }
    pub fn release_active(&mut self) -> Option<PendingRun> { self.active.take() }

    pub fn enqueue(&mut self, input: KernelRunInput, on_message: Option<KernelMessageCallback>, on_started: Option<KernelStartedCallback>) -> tokio::sync::oneshot::Receiver<Value> {
        let (resolve, receiver) = tokio::sync::oneshot::channel();
        let mut run = create_pending_run(input, resolve);
        run.on_message = on_message;
        run.on_started = on_started;
        self.queue.push_back(run);
        receiver
    }

    pub fn start_next(&mut self, started_at: f64) -> Option<&PendingRun> {
        if self.active.is_some() { return None; }
        self.active = self.queue.pop_front();
        if let Some(run) = &mut self.active {
            run.started_at = Some(started_at);
            if let Some(callback) = &run.on_started { callback(); }
        }
        self.active.as_ref()
    }

    pub fn remove(&mut self, cell_id: &str, reason: &str, now_ms: f64) -> bool {
        let Some(index) = self.queue.iter().position(|run| run.input.cell_id == cell_id) else { return false; };
        let Some(mut run) = self.queue.remove(index) else { return false; };
        let result = failure_result(&run, reason, now_ms);
        settle_pending_run(&mut run, result);
        true
    }

    pub fn snapshot(&self) -> (Option<String>, Vec<String>) {
        (self.active.as_ref().map(|run| run.input.cell_id.clone()), self.queue.iter().map(|run| run.input.cell_id.clone()).collect())
    }

    pub fn settle_all(&mut self, error: &str, now_ms: f64) {
        let runs = self.active.take().into_iter().chain(self.queue.drain(..));
        for mut run in runs {
            let result = failure_result(&run, error, now_ms);
            settle_pending_run(&mut run, result);
        }
    }

    pub fn clear_tool_calls(&mut self) { self.pending_calls.clear(); self.call_waiters.clear(); }

    pub fn next_tool_call(&mut self) -> tokio::sync::oneshot::Receiver<Value> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if let Some(message) = self.pending_calls.pop_front() { let _ = sender.send(message); }
        else { self.call_waiters.push_back(sender); }
        receiver
    }

    pub fn push_tool_call(&mut self, message: Value) {
        if let Some(waiter) = self.call_waiters.pop_front() { let _ = waiter.send(message); }
        else {
            self.pending_calls.push_back(message);
            if self.pending_calls.len() > 256 { self.pending_calls.pop_front(); }
        }
    }

    pub fn handle_message(&mut self, message: Value, fallback: Option<&KernelMessageCallback>) -> bool {
        match message["type"].as_str() {
            Some("result") => {
                let Some(run) = self.active.as_ref() else { return false; };
                if message["cellId"] != run.input.cell_id { return false; }
                if let Some(callback) = run.on_message.as_ref().or(fallback) { callback(&message); }
                if let Some(mut run) = self.active.take() { settle_pending_run(&mut run, message); }
                true
            }
            Some("tool-call") => {
                let Some(run) = self.active.as_ref() else { return false; };
                if let Some(callback) = run.on_message.as_ref().or(fallback) { callback(&message); }
                self.push_tool_call(message);
                false
            }
            Some("text" | "display" | "log" | "phase" | "status") => {
                if let Some(run) = self.active.as_ref() && let Some(callback) = run.on_message.as_ref().or(fallback) { callback(&message); }
                false
            }
            _ => { if let Some(callback) = fallback { callback(&message); } false }
        }
    }
}
