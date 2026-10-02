use std::{path::PathBuf, time::Duration};
use tokio::sync::oneshot;
use serde_json::Value;
use crate::{bridge::protocol::BridgeConnectionConfig, kernels::{session_env::SessionEnvironment, shared::subprocess_run::{KernelMessageCallback, KernelStartedCallback}}};

pub struct PythonKernelStartOptions {
    pub interpreter_path: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub connection: BridgeConnectionConfig,
    pub env: Option<SessionEnvironment>,
    pub session_env: Option<SessionEnvironment>,
    pub startup_timeout: Option<Duration>,
    pub on_message: Option<KernelMessageCallback>,
}

pub struct PythonKernelRunOptions {
    pub cell_id: String,
    pub code: String,
    pub timeout_ms: Option<u64>,
    pub on_started: Option<KernelStartedCallback>,
    pub on_message: Option<KernelMessageCallback>,
}

pub struct PendingRun {
    pub input: PythonKernelRunOptions,
    pub resolve: oneshot::Sender<Result<Value, String>>,
    pub started_at: Option<tokio::time::Instant>,
    pub interrupt_reason: Option<String>,
    pub resolve_state_retained: Option<oneshot::Sender<bool>>,
}
