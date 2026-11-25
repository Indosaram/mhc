//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-runtime.ts`.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use maho_agent::execution::plane::NestedRequestFn;
use maho_agent::types::AgentToolResult;
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use maho_core::agent_session_runtime::AgentSessionLaunchProfile;
use maho_core::agent_session_services::AgentSessionRuntimeDiagnostic;
use maho_core::execution::hand_wake_route::HandWake;
use maho_core::runtime::bootstrap::ToolDefinitionEntry;
use serde_json::Value;

use crate::hand_file_ops::HandFileGuards;
use crate::loopless_brain_calls::LooplessCallFacts;
use crate::loopless_loop_hooks::LooplessLoopHooks;

pub type LooplessExecuteFn = Arc<
    dyn Fn(
            String,
            String,
            Value,
            Option<AbortSignal>,
            Option<NestedRequestFn>,
            Option<LooplessCallFacts>,
        ) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

pub type LooplessDisposeFn = Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// A session worker's runtime with the agent loop removed (brain/hand split, `senpi#5413`): the
/// same built-in and extension tools a full session serves, executed one call at a time, with no
/// agent loop, no model binding and no transcript.
#[derive(Clone)]
pub struct LooplessRuntime {
    pub cwd: String,
    /// Every tool this runtime serves: built-ins first, then extension tools, a later name
    /// replacing an earlier one.
    pub definitions: HashMap<String, ToolDefinitionEntry>,
    /// What building the runtime reported: settings, extension load and `project_trust` handler
    /// diagnostics.
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    /// Runs one tool call: argument preparation and schema validation as the loop does, then the
    /// tool. Refuses every call unless built with an execute grant.
    pub execute: LooplessExecuteFn,
    /// Runs this runtime's extensions' loop hooks for a loop that runs elsewhere (S33), or answers
    /// `describe_tools` on any runtime (`senpi#6749`).
    pub run_loop_hooks: LooplessLoopHooks,
    /// What a device file operation (S40) is checked against: this runtime's `tool_call` handlers
    /// and filesystem policy. Present only when its grant runs the handlers.
    pub file_guards: Option<Arc<dyn HandFileGuards>>,
    /// Marks its extension contexts stale, then closes the tool work and the tool child it still
    /// runs.
    pub dispose: LooplessDisposeFn,
}

impl LooplessRuntime {
    pub async fn execute(
        &self,
        name: impl Into<String>,
        tool_call_id: impl Into<String>,
        params: Value,
        signal: Option<AbortSignal>,
        nested: Option<NestedRequestFn>,
        facts: Option<LooplessCallFacts>,
    ) -> Result<AgentToolResult, String> {
        (self.execute)(
            name.into(),
            tool_call_id.into(),
            params,
            signal,
            nested,
            facts,
        )
        .await
    }

    pub async fn run_loop_hooks(
        &self,
        event: impl Into<String>,
        payload: Value,
    ) -> Result<Value, String> {
        (self.run_loop_hooks)(event.into(), payload).await
    }

    pub async fn dispose(&self) -> Result<(), String> {
        (self.dispose)().await
    }
}

#[derive(Clone)]
pub struct LooplessRuntimeProfile {
    pub cwd: String,
    pub launch_profile: Option<AgentSessionLaunchProfile>,
    /// On a hand, this runtime's scope's wake outbox (H3): what its work sends to the loop after
    /// the call that started it ended goes there.
    pub wake: Option<Arc<dyn Fn(HandWake) + Send + Sync>>,
}

impl LooplessRuntimeProfile {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            launch_profile: None,
            wake: None,
        }
    }
}

pub type LooplessRuntimeFactory = Arc<
    dyn Fn(LooplessRuntimeProfile) -> BoxFuture<'static, Result<LooplessRuntime, String>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LooplessToolNotFoundError {
    pub tool_name: String,
}

impl LooplessToolNotFoundError {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
        }
    }
}

impl fmt::Display for LooplessToolNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "loopless_tool_not_found: {}", self.tool_name)
    }
}

impl std::error::Error for LooplessToolNotFoundError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_not_found_error_formats_expected_prefix() {
        let err = LooplessToolNotFoundError::new("bash");
        assert_eq!(err.to_string(), "loopless_tool_not_found: bash");
    }
}
