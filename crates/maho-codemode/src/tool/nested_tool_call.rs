use super::{
    call_capture::{cap_code_points, tool_call_result_details, tool_call_result_preview},
    nested_tool_output::CellToolUpdateSink,
    tool_result_marshal::{marshal_tool_result, tool_result_is_error},
};
use crate::bridges::output_bridge::OutputExecuteTool;
use maho_agent::types::AgentToolUpdateCallback;
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{AgentToolResult, ContentBlock, ExecuteToolError, ExecuteToolOptions};
use serde_json::Value;
use std::sync::Arc;

pub struct ResolvedToolReply {
    pub value: Value,
    pub tool_call_ok: bool,
    pub result_preview: Option<String>,
    pub error_text: Option<String>,
    /// The result's details, kept only for a tool whose own `renderResult` draws its nested row.
    pub details: Option<Value>,
}

struct DropNestedOnDrop<'a> {
    live: Option<&'a dyn CellToolUpdateSink>,
    call_id: &'a str,
}

impl Drop for DropNestedOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(live) = self.live {
            live.nested_tool_settled(self.call_id);
        }
    }
}

/// Runs an ordinary host tool for a JS cell; its partial results stream into the cell's live update
/// (`#6167`).
pub async fn run_nested_tool(
    executor: &dyn OutputExecuteTool,
    tool_name: &str,
    call_id: &str,
    args: Value,
    signal: AbortSignal,
    live: Option<Arc<dyn CellToolUpdateSink>>,
    keep_details: bool,
) -> Result<ResolvedToolReply, ExecuteToolError> {
    let on_update = live.as_ref().map(|sink| {
        let sink = Arc::clone(sink);
        let call_id = call_id.to_owned();
        Arc::new(move |partial: AgentToolResult| {
            sink.nested_tool_update(&call_id, &partial);
        }) as AgentToolUpdateCallback
    });
    let _drop_guard = DropNestedOnDrop {
        live: live.as_deref(),
        call_id,
    };
    let result = executor
        .execute_tool(
            tool_name,
            args,
            ExecuteToolOptions {
                signal: Some(signal),
                on_update,
                ..Default::default()
            },
        )
        .await?;
    let tool_call_ok = !tool_result_is_error(&result);
    let details = if keep_details {
        tool_call_result_details(&result)
    } else {
        None
    };
    if tool_call_ok {
        let result_preview = tool_call_result_preview(&result);
        return Ok(ResolvedToolReply {
            value: marshal_tool_result(&result),
            tool_call_ok,
            result_preview,
            error_text: None,
            details,
        });
    }
    let error_text = result.content.iter().find_map(|part| match part {
        ContentBlock::Text(content) => Some(cap_code_points(
            &crate::host_sdk::sanitize_terminal_label(&content.text),
            512,
        )),
        _ => None,
    });
    Ok(ResolvedToolReply {
        value: marshal_tool_result(&result),
        tool_call_ok,
        result_preview: None,
        error_text,
        details,
    })
}
