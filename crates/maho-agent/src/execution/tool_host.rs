//! Port of senpi `packages/agent/src/execution/tool-host.ts`.

use std::sync::Arc;

use maho_ai::types::{AssistantMessage, BoxFuture, ToolResultMessage};
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AgentContext, AgentEvent, AgentToolCall, ToolExecutionMode};

use super::device_files::DeviceFiles;

/// Brain-minted identity and execution root; the executor treats the identity as opaque.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExecutionScope {
    pub id: String,
    pub root: String,
}

/// The raw model calls, before name resolution, argument preparation, validation or hooks.
#[derive(Debug, Clone)]
pub struct ToolHostCall {
    pub scope: ExecutionScope,
    pub calls: Vec<AgentToolCall>,
    pub mode: ToolExecutionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPipelineWitness {
    pub pipeline: &'static str,
    pub tool_call_id: String,
}

#[derive(Debug, Clone)]
pub struct ToolHostResult {
    pub messages: Vec<ToolResultMessage>,
    pub terminate: bool,
}

pub type WitnessRecorder =
    Arc<dyn Fn(ExecutionPipelineWitness) -> BoxFuture<'static, ()> + Send + Sync>;

/// The loop's own view of the batch, for a host that runs in the loop's process and runs
/// today's preparation steps (the prepared pipeline). A remote host never reads it.
#[derive(Clone)]
pub struct ToolHostLocalContext {
    pub context: AgentContext,
    pub assistant_message: AssistantMessage,
}

pub struct ToolHostOptions {
    pub signal: Option<AbortSignal>,
    /// Preserve tool events and ordered result-message events. The loop appends the returned messages to context.
    pub emit: Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>,
    /// Receipt metadata only: never an AgentEvent, tool result, provider payload or EQ observable.
    pub record_witness: Option<WitnessRecorder>,
    pub local: Option<ToolHostLocalContext>,
}

/// The loop had a tool batch to run on its default pipeline but no `batch_store_factory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "agent loop: no batchStoreFactory configured for the prepared pipeline; pass one (or a toolHost)"
)]
pub struct BatchStoreMissingError;

impl BatchStoreMissingError {
    pub fn code(&self) -> &'static str {
        "batch_store_missing"
    }
}

/// The loop had a tool batch to run on its default pipeline but no `after_tool_freeze`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "agent loop: no afterToolFreeze configured for the prepared pipeline; pass one (or a toolHost)"
)]
pub struct PreflightCarryMissingError;

impl PreflightCarryMissingError {
    pub fn code(&self) -> &'static str {
        "preflight_carry_missing"
    }
}

/// The package entry's default `after_tool_freeze`: for hosts whose `before_tool_call` binds nothing to the arguments object.
pub fn no_preflight_carry(_prepared: &Value, _frozen: &mut Value) {}

pub type AfterToolFreeze = Arc<dyn Fn(&Value, &mut Value) + Send + Sync>;

pub fn no_preflight_carry_fn() -> AfterToolFreeze {
    Arc::new(no_preflight_carry)
}

pub trait ToolHost: Send + Sync {
    fn execute(
        &self,
        call: ToolHostCall,
        options: ToolHostOptions,
    ) -> BoxFuture<'_, Result<ToolHostResult, String>>;

    /// The files of the device this host runs calls on, for `scope`; `None`: the loop's own machine.
    fn device_files(&self, _scope: &ExecutionScope) -> Option<Arc<dyn DeviceFiles>> {
        None
    }
}
