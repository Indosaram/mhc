use maho_ext_api::ExecuteToolOptions;
use serde_json::Value;
use crate::bridge::reserved::{RESERVED_AGENT_TOOL, RESERVED_OUTPUT_TOOL, RESERVED_SCHEMA_TOOL};
use super::{agent_bridge::{AgentBridge, AgentBridgeError, AgentBridgeOptions, AgentStatusEmitter}, output_bridge::{OutputBridgeError, OutputExecuteTool, run_eval_output}, schema_bridge::{EvalSchemaToolInfo, SchemaError, run_eval_schema}};

pub struct ReservedDispatchContext<'a> {
    pub call_id: &'a str,
    pub args: &'a Value,
    pub executor: &'a dyn OutputExecuteTool,
    pub task_tool_name: &'a str,
    pub task_output_tool_name: &'a str,
    pub tools: Option<&'a [EvalSchemaToolInfo]>,
    pub execute_options: ExecuteToolOptions,
    pub emit_status: Option<AgentStatusEmitter>,
    pub agent_bridge: &'a AgentBridge,
}

#[derive(Debug, thiserror::Error)]
pub enum ReservedDispatchError {
    #[error("tool_schema() unavailable: this session does not expose a tool catalog")]
    SchemaUnavailable,
    #[error("runReservedTool received a non-reserved tool name: {0}")]
    NotReserved(String),
    #[error(transparent)] Agent(#[from] AgentBridgeError),
    #[error(transparent)] Output(#[from] OutputBridgeError),
    #[error(transparent)] Schema(#[from] SchemaError),
    #[error(transparent)] Serialize(#[from] serde_json::Error),
}

pub fn is_reserved_tool_name(name: &str) -> bool { matches!(name, RESERVED_AGENT_TOOL | RESERVED_OUTPUT_TOOL | RESERVED_SCHEMA_TOOL) }

pub async fn run_reserved_tool(name: &str, context: ReservedDispatchContext<'_>) -> Result<Value, ReservedDispatchError> {
    match name {
        RESERVED_AGENT_TOOL => Ok(context.agent_bridge.run(context.args, AgentBridgeOptions { call_id:context.call_id,task_tool_name:context.task_tool_name,executor:context.executor,tools:context.tools,execute_options:context.execute_options,emit_status:context.emit_status }).await?),
        RESERVED_OUTPUT_TOOL => Ok(serde_json::to_value(run_eval_output(context.args.clone(),context.task_output_tool_name,context.executor,context.execute_options).await?)?),
        RESERVED_SCHEMA_TOOL => Ok(serde_json::to_value(run_eval_schema(context.args,context.tools.ok_or(ReservedDispatchError::SchemaUnavailable)?)?)?),
        _ => Err(ReservedDispatchError::NotReserved(name.into())),
    }
}
