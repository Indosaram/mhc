use super::subprocess_contract::KernelRunInput;
use serde_json::{Value, json};

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

pub fn create_pending_run(
    input: KernelRunInput,
    resolve: tokio::sync::oneshot::Sender<Value>,
) -> PendingRun {
    PendingRun {
        input,
        resolve: Some(resolve),
        timer: None,
        started_at: None,
        settled: false,
        on_message: None,
        on_started: None,
    }
}

pub fn failure_result(run: &PendingRun, message: &str, now_ms: f64) -> Value {
    json!({"type":"result", "cellId":run.input.cell_id, "ok":false,"error":{"message":message},"durationMs":run.started_at.map_or(0.0, |started| ((now_ms - started) + 0.5).floor().max(0.0))})
}

pub fn timeout_result(run: &PendingRun, timeout_ms: u64) -> Value {
    json!({"type":"result", "cellId":run.input.cell_id,"ok":false,"error":{"message":format!("Cell timed out after {timeout_ms}ms")},"durationMs":timeout_ms})
}

pub fn settle_pending_run(run: &mut PendingRun, result: Value) -> bool {
    if run.settled {
        return false;
    }
    run.settled = true;
    if let Some(timer) = run.timer.take() {
        timer.abort();
    }
    if let Some(resolve) = run.resolve.take() {
        let _ = resolve.send(result);
    }
    true
}

impl Drop for PendingRun {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }
}

/// senpi's named run errors; `Display` is the upstream `Error.message`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KernelRunError {
    #[error("Kernel is closed")]
    Closed,
    #[error("Kernel startup failed: {0}")]
    Startup(String),
    #[error("Kernel process did not exit after SIGKILL")]
    Retirement,
    #[error("Kernel exited before completing the cell ({0})")]
    Exited(String),
    #[error("Kernel process error: {0}")]
    Process(String),
    #[error("{}", interrupted_message(.0))]
    CellInterrupted(String),
    #[error("Kernel reset")]
    Reset,
    #[error("Kernel closed")]
    Closing,
}

fn interrupted_message(reason: &str) -> String {
    if reason == "Eval interrupted" {
        reason.to_owned()
    } else {
        format!("Eval interrupted: {reason}")
    }
}

impl KernelRunError {
    /// The upstream `Error.name`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Closed => "KernelClosedError",
            Self::Startup(_) => "KernelStartupError",
            Self::Retirement => "KernelRetirementError",
            Self::Exited(_) => "KernelExitedError",
            Self::Process(_) => "KernelProcessError",
            Self::CellInterrupted(_) => "CellInterruptedError",
            Self::Reset => "KernelResetError",
            Self::Closing => "KernelClosingError",
        }
    }
}

#[cfg(test)]
mod named_error_tests {
    use super::KernelRunError;

    #[test]
    fn named_errors_keep_upstream_names_and_messages() {
        let cases = [
            (
                KernelRunError::Closed,
                "KernelClosedError",
                "Kernel is closed",
            ),
            (
                KernelRunError::Startup("boom".into()),
                "KernelStartupError",
                "Kernel startup failed: boom",
            ),
            (
                KernelRunError::Retirement,
                "KernelRetirementError",
                "Kernel process did not exit after SIGKILL",
            ),
            (
                KernelRunError::Exited("SIGTERM".into()),
                "KernelExitedError",
                "Kernel exited before completing the cell (SIGTERM)",
            ),
            (
                KernelRunError::Process("EPIPE".into()),
                "KernelProcessError",
                "Kernel process error: EPIPE",
            ),
            (
                KernelRunError::CellInterrupted("Eval interrupted".into()),
                "CellInterruptedError",
                "Eval interrupted",
            ),
            (
                KernelRunError::CellInterrupted("stop".into()),
                "CellInterruptedError",
                "Eval interrupted: stop",
            ),
            (KernelRunError::Reset, "KernelResetError", "Kernel reset"),
            (
                KernelRunError::Closing,
                "KernelClosingError",
                "Kernel closed",
            ),
        ];
        for (error, name, message) in cases {
            assert_eq!(error.name(), name);
            assert_eq!(error.to_string(), message);
        }
    }
}
