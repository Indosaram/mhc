use crate::bridge::protocol::BridgeConnectionConfig;
use crate::kernels::session_env::SessionEnvironment;
use std::path::PathBuf;

#[derive(Clone)]
pub struct SubprocessKernelOptions {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Option<SessionEnvironment>,
    pub session_env: Option<SessionEnvironment>,
    pub session_id: String,
    pub connection: BridgeConnectionConfig,
}

#[derive(Clone, Debug)]
pub struct KernelRunInput {
    pub cell_id: String,
    pub code: String,
    pub timeout_ms: Option<u64>,
}
