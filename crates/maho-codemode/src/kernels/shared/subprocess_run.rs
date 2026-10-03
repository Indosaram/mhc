use serde_json::{Value, json};
use super::subprocess_contract::KernelRunInput;

pub type KernelMessageCallback = std::sync::Arc<dyn Fn(&Value) + Send + Sync>;
pub type KernelStartedCallback = std::sync::Arc<dyn Fn() + Send + Sync>;

pub struct PendingRun {
    pub input: KernelRunInput,
    pub on_message: Option<KernelMessageCallback>,
    pub on_started: Option<KernelStartedCallback>,
    pub started_at: Option<f64>,
    pub settled: bool,
    pub timer: Option<tokio::task::JoinHandle<()>>,
    resolve: Option<tokio::sync::oneshot::Sender<Value>>,
}

pub fn create_pending_run(input: KernelRunInput, resolve: tokio::sync::oneshot::Sender<Value>) -> PendingRun {
    PendingRun { input, resolve: Some(resolve), timer: None, started_at: None, settled: false, on_message: None, on_started: None }
}

pub fn failure_result(run: &PendingRun, message: &str, now_ms: f64) -> Value {
    json!({"type":"result", "cellId":run.input.cell_id, "ok":false,"error":{"message":message},"durationMs":run.started_at.map_or(0.0, |started| ((now_ms - started) + 0.5).floor().max(0.0))})
}

pub fn timeout_result(run: &PendingRun, timeout_ms: u64) -> Value {
    json!({"type":"result", "cellId":run.input.cell_id,"ok":false,"error":{"message":format!("Cell timed out after {timeout_ms}ms")},"durationMs":timeout_ms})
}

pub fn settle_pending_run(run: &mut PendingRun, result: Value) -> bool {
    if run.settled { return false; }
    run.settled = true;
    if let Some(timer) = run.timer.take() { timer.abort(); }
    if let Some(resolve) = run.resolve.take() { let _ = resolve.send(result); }
    true
}

impl Drop for PendingRun {
    fn drop(&mut self) { if let Some(timer) = self.timer.take() { timer.abort(); } }
}
