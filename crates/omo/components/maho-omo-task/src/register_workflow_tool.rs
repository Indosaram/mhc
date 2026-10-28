//! `register-workflow-tool.ts`: the ONE place the workflow tool is registered, with every runtime
//! capability it exposes wired in.
//!
//! A capability missing here is a verb the model sees in the schema and cannot use, so registration
//! is deliberately exhaustive: wait, cancel, cancelNode, retry, send, amend, and guide.

pub type PauseReasonCallback = Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>;
use std::sync::Arc;

use maho_ext_api::ToolDefinition;

use crate::dag_tool::{DagToolDeps, create_dag_tool};

/// `registerDagTool`: build the tool from the runtime's capabilities.
pub fn register_dag_tool(deps: DagToolDeps) -> ToolDefinition {
    create_dag_tool(deps)
}

/// The runtime capabilities the production registration must supply. Every field is required except
/// the ones a session can legitimately lack (a paused-run verb on a runtime with no pause support).
pub struct WorkflowToolCapabilities {
    pub manager: senpi_task::dag::manager::DagManager,
    pub parent_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub root_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub wait: crate::dag_tool::DagWait,
    pub cancel: crate::dag_tool::DagCancel,
    pub cancel_node: Option<crate::dag_tool::DagCancelNode>,
    pub retry: crate::dag_tool::DagRetry,
    pub send: crate::dag_tool::DagSend,
    pub amend: crate::dag_tool::DagAmend,
    pub guide: Option<crate::dag_tool::DagGuide>,
    pub pause_reason: PauseReasonCallback,
}

/// `registerTaskTools`: the dag tool's dependencies, with guide and cancelNode wired.
pub fn dag_tool_deps(capabilities: WorkflowToolCapabilities) -> DagToolDeps {
    DagToolDeps {
        manager: capabilities.manager,
        parent_session_id: capabilities.parent_session_id,
        root_session_id: capabilities.root_session_id,
        wait: Some(capabilities.wait),
        cancel: Some(capabilities.cancel),
        retry: Some(capabilities.retry),
        send: Some(capabilities.send),
        amend: Some(capabilities.amend),
        cancel_node: capabilities.cancel_node,
        guide: capabilities.guide,
        pause_reason: capabilities.pause_reason,
    }
}
