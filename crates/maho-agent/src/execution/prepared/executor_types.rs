//! Port of senpi `packages/agent/src/execution/prepared/executor-types.ts`.

use std::sync::Arc;

use maho_ai::types::{AssistantMessage, BoxFuture};
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;

use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentTool, AgentToolCall,
    AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext, BeforeToolCallResult,
};

use super::super::ledger::ExecutionLedger;
use super::super::ledger_types::{ExecutionEntry, ExecutionKey, FrozenCall};

pub trait PreparationPipeline: Send + Sync {
    fn resolve(
        &self,
        call: &AgentToolCall,
        context: &AgentContext,
    ) -> BoxFuture<'_, Result<Option<AgentTool>, String>>;

    fn before_tool_call(
        &self,
        context: BeforeToolCallContext,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Option<BeforeToolCallResult>, String>>;

    fn after_tool_call(
        &self,
        context: AfterToolCallContext,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Option<AfterToolCallResult>, String>>;

    fn removed_tool_hint(&self, name: &str) -> Option<String>;

    /// Synchronous and must never panic: carries facts attached to the prepared arguments object
    /// over to the frozen copy the tool will execute.
    fn after_freeze(&self, prepared: &Value, frozen: &mut Value);
}

pub trait ConsentClassifier: Send + Sync {
    fn needs_consent(&self, name: &str, args: &Value) -> bool;
}

pub struct PreparedExecutorOptions {
    pub ledger: Arc<dyn ExecutionLedger>,
    pub executor: String,
    pub pipeline: Arc<dyn PreparationPipeline>,
    pub clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub consent: Option<Arc<dyn ConsentClassifier>>,
}

#[derive(Clone)]
pub struct PrepareRequest {
    pub issuer: String,
    pub generation: u64,
    pub workspace: String,
    pub scope: String,
    pub call: AgentToolCall,
    pub command_id: Option<String>,
    pub assistant_message: AssistantMessage,
    pub context: Arc<AgentContext>,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub reconcilable: Option<bool>,
    pub signal: Option<AbortSignal>,
    pub authorized_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PrepareReply {
    Prepared {
        call: FrozenCall,
        requested_name: Option<String>,
    },
    PreparedAwaitingConsent {
        call: FrozenCall,
        requested_name: Option<String>,
    },
    Blocked {
        result: AgentToolResult,
        requested_name: Option<String>,
    },
    BatchAbort {
        result: AgentToolResult,
    },
    Refused {
        reason: String,
    },
}

pub type OnUpdateForFn = Arc<dyn Fn(&AgentToolCall) -> AgentToolUpdateCallback + Send + Sync>;

#[derive(Clone)]
pub struct ReleaseRequest {
    pub key: ExecutionKey,
    pub generation: u64,
    pub actor_id: String,
    pub signal: Option<AbortSignal>,
    pub on_update: Option<AgentToolUpdateCallback>,
    pub on_update_for: Option<OnUpdateForFn>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ReleaseReply {
    Settled {
        tool_call: AgentToolCall,
        result: AgentToolResult,
        is_error: bool,
        requested_name: Option<String>,
    },
    Aborted {
        result: AgentToolResult,
    },
    Replayed {
        entry: ExecutionEntry,
    },
    Refused {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct ConsentRequest {
    pub key: ExecutionKey,
    pub generation: u64,
    pub actor_id: String,
    pub frozen_digest: String,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ConsentReply {
    Prepared,
    Replayed { entry: ExecutionEntry },
    Refused { reason: String },
}

pub trait PreparedExecutor: Send + Sync {
    fn prepare(&self, request: PrepareRequest) -> BoxFuture<'_, Result<PrepareReply, String>>;
    fn release(&self, request: ReleaseRequest) -> BoxFuture<'_, Result<ReleaseReply, String>>;
    fn consent(&self, request: ConsentRequest) -> BoxFuture<'_, Result<ConsentReply, String>>;
}
