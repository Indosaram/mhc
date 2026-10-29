//! `runners/rpc/errors.ts`.

/// A senpi RPC command that returned `success: false`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("RPC command {command} failed: {detail}")]
pub struct RpcCommandError {
    pub command: String,
    pub detail: String,
}

impl RpcCommandError {
    pub fn new(command: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            detail: detail.into(),
        }
    }
}
