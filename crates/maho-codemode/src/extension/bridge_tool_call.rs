use crate::bridge::reserved::RESERVED_OUTPUT_TOOL;
use crate::bridges::{
    agent_bridge::AgentBridge,
    output_bridge::OutputExecuteTool,
    packages_install::PackagesInstallEnvironments,
    reserved_dispatch::{
        ReservedDispatchContext, execute_tool_error_code, is_reserved_tool_name, run_reserved_tool,
    },
    schema_bridge::EvalToolCatalog,
};
use crate::config::settings::CodemodeTaskTools;
use crate::tool::{
    nested_tool_output::CellToolUpdateSink, tool_result_marshal::marshal_tool_result,
};
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{AgentToolResult, ExecuteToolOptions};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct BridgeToolCallRequest<'a> {
    pub tool_name: &'a str,
    pub args: Value,
    pub call_id: &'a str,
    /// Calling run's secret; absent for a call made outside any run.
    pub cell_token: Option<&'a str>,
    pub signal: AbortSignal,
}

pub struct BridgeToolCallContext<'a> {
    pub executor: &'a dyn OutputExecuteTool,
    pub list_tools: Option<&'a EvalToolCatalog>,
    pub task_tools: &'a CodemodeTaskTools,
    pub agent_bridge: &'a AgentBridge,
    pub environments: Option<&'a dyn PackagesInstallEnvironments>,
    pub tool_updates: Option<Arc<dyn CellToolUpdateSink>>,
}

struct SettledOnDrop {
    call_id: String,
    sink: Option<Arc<dyn CellToolUpdateSink>>,
}

impl Drop for SettledOnDrop {
    fn drop(&mut self) {
        if let Some(sink) = &self.sink {
            sink.nested_tool_settled(&self.call_id);
        }
    }
}

/// Routes a bridge `/call` request to either reserved cell helpers or an ordinary session tool,
/// streaming ordinary tool `on_update` partials to `tool_updates` when bound (`#6167`).
pub async fn route_bridge_tool_call(
    context: BridgeToolCallContext<'_>,
    request: BridgeToolCallRequest<'_>,
) -> Result<Value, Value> {
    if !is_reserved_tool_name(request.tool_name) {
        let on_update = context.tool_updates.as_ref().map(|sink| {
            let sink = Arc::clone(sink);
            let call_id = request.call_id.to_owned();
            Arc::new(move |partial: AgentToolResult| {
                sink.nested_tool_update(&call_id, &partial);
            }) as maho_agent::types::AgentToolUpdateCallback
        });
        let execute_options = ExecuteToolOptions {
            signal: Some(request.signal),
            on_update,
            ..Default::default()
        };
        let _settled = SettledOnDrop {
            call_id: request.call_id.to_owned(),
            sink: context.tool_updates,
        };
        return context
            .executor
            .execute_tool(request.tool_name, request.args, execute_options)
            .await
            .map(|result| marshal_tool_result(&result))
            .map_err(|error| {
                let code = execute_tool_error_code(&error.code);
                json!({"name": "Error", "message": error.to_string(), "code": code})
            });
    }

    let tools = if request.tool_name == RESERVED_OUTPUT_TOOL {
        None
    } else {
        context
            .list_tools
            .map(|list| list())
            .transpose()
            .map_err(|error| json!({"name": "Error", "message": error}))?
    };
    let execute_options = ExecuteToolOptions {
        signal: Some(request.signal),
        ..Default::default()
    };
    run_reserved_tool(
        request.tool_name,
        ReservedDispatchContext {
            call_id: request.call_id,
            args: &request.args,
            executor: context.executor,
            task_tool_name: &context.task_tools.task,
            task_output_tool_name: &context.task_tools.output,
            tools: tools.as_deref(),
            execute_options,
            emit_status: None,
            agent_bridge: context.agent_bridge,
            environments: context.environments,
        },
    )
    .await
    .map_err(|error| {
        let mut payload = json!({"name": "Error", "message": error.to_string()});
        if let Some(code) = error.code() {
            payload["code"] = json!(code);
        }
        payload
    })
}
