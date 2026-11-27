//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-tool-call.ts`.

use std::sync::Arc;

use maho_agent::execution::plane::NestedRequestFn;
use maho_agent::tool_arguments::prepare_agent_tool_call_arguments;
use maho_agent::types::{AgentTool, AgentToolResult};
use maho_ai::types::{BoxFuture, ContentBlock, ToolCall};
use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::validation::validate_tool_arguments;
use maho_ext_api::{ToolCallEventResult, ToolResultEventResult};
use serde_json::{Map, Value};

fn error_result(message: impl Into<String>) -> AgentToolResult {
    let mut res = AgentToolResult::text(message);
    res.is_error = Some(true);
    res
}

pub struct BeforeToolCallArgs<'a> {
    pub tool_name: &'a str,
    pub tool_call_id: &'a str,
    pub input: &'a mut Value,
    pub nested: Option<NestedRequestFn>,
}

pub struct AfterToolCallArgs<'a> {
    pub tool_name: &'a str,
    pub tool_call_id: &'a str,
    pub input: &'a Value,
    pub result: &'a AgentToolResult,
    pub is_error: bool,
    pub nested: Option<NestedRequestFn>,
}

/// The extensions' tool handlers a call runs around its tool, as a session runs them
/// (`agent-session.ts`).
pub trait LooplessToolHandlers: Send + Sync {
    fn before_tool_call<'a>(
        &'a self,
        event: BeforeToolCallArgs<'a>,
    ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>>;

    fn after_tool_call<'a>(
        &'a self,
        event: AfterToolCallArgs<'a>,
    ) -> BoxFuture<'a, Result<Option<ToolResultEventResult>, String>>;
}

pub struct LooplessToolCallRequest {
    pub tool_call_id: String,
    pub params: Value,
    pub signal: Option<AbortSignal>,
    /// The running call's requests back to the loop that owns it (on a hand).
    pub nested: Option<NestedRequestFn>,
}

fn with_override(
    mut result: AgentToolResult,
    override_res: ToolResultEventResult,
) -> AgentToolResult {
    if let Some(content) = override_res.content {
        if let Ok(val) = serde_json::to_value(&content)
            && let Ok(blocks) = serde_json::from_value::<Vec<ContentBlock>>(val)
        {
            result.content = blocks;
        }
        result.structured_content = None;
    }
    if let Some(details) = override_res.details {
        result.details = details;
    }
    if let Some(usage) = override_res.usage
        && let Ok(val) = serde_json::to_value(usage)
        && let Ok(u) = serde_json::from_value(val)
    {
        result.usage = Some(u);
    }
    result.is_error = Some(
        override_res
            .is_error
            .unwrap_or_else(|| result.is_error == Some(true)),
    );
    result
}

/// Runs one tool call the way the agent loop runs a model-issued one (`runToolCall`,
/// `agent-loop.ts`): argument preparation and schema validation; then, when `handlers` are given,
/// the `tool_call` handlers, where a block, an abort or a handler that throws stops the call
/// before the tool runs (fail-closed, as a session does); the tool; and the `tool_result`
/// handlers. Invalid arguments, a blocked call and a failing tool come back as an `isError`
/// result, never an Err (`senpi#5413`).
pub async fn run_loopless_tool_call(
    tool: &AgentTool,
    call: LooplessToolCallRequest,
    handlers: Option<&Arc<dyn LooplessToolHandlers>>,
) -> AgentToolResult {
    let tool_call_id = call.tool_call_id;
    let signal = call.signal;
    let raw_map: Map<String, Value> = match call.params {
        Value::Object(map) => map,
        other => {
            let mut m = Map::new();
            m.insert("value".to_owned(), other);
            m
        }
    };
    let raw_call = ToolCall {
        id: tool_call_id.clone(),
        name: tool.tool.name.clone(),
        arguments: raw_map,
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    };
    let prepared = match prepare_agent_tool_call_arguments(tool, &raw_call) {
        Ok(p) => p,
        Err(err) => return error_result(err),
    };
    let mut args = match validate_tool_arguments(&tool.tool, &prepared) {
        Ok(v) => v,
        Err(err) => return error_result(err.to_string()),
    };

    if let Some(handlers) = handlers {
        let before = match handlers
            .before_tool_call(BeforeToolCallArgs {
                tool_name: &tool.tool.name,
                tool_call_id: &tool_call_id,
                input: &mut args,
                nested: call.nested.clone(),
            })
            .await
        {
            Ok(b) => b,
            Err(err) => return error_result(err),
        };
        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return error_result("Operation aborted");
        }
        if let Some(before) = before
            && before.block == Some(true)
        {
            let mut blocked = error_result(
                before
                    .reason
                    .unwrap_or_else(|| "Tool execution was blocked".to_owned()),
            );
            if before.terminate == Some(true) {
                blocked.terminate = Some(true);
            }
            return blocked;
        }
    }

    let result = (tool.execute)(tool_call_id.clone(), args.clone(), signal, None).await;
    let Some(handlers) = handlers else {
        return result;
    };
    let is_error = result.is_error == Some(true);
    match handlers
        .after_tool_call(AfterToolCallArgs {
            tool_name: &tool.tool.name,
            tool_call_id: &tool_call_id,
            input: &args,
            result: &result,
            is_error,
            nested: call.nested,
        })
        .await
    {
        Ok(Some(override_res)) => with_override(result, override_res),
        Ok(None) => result,
        Err(err) => error_result(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::Tool;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn echo_tool(ran: Arc<AtomicBool>) -> AgentTool {
        AgentTool {
            label: "echo".to_owned(),
            prepare_arguments: None,
            execute: Arc::new(move |_, params, _, _| {
                let ran = Arc::clone(&ran);
                Box::pin(async move {
                    ran.store(true, Ordering::SeqCst);
                    AgentToolResult::text(params["msg"].as_str().unwrap_or("").to_owned())
                })
            }),
            replay: None,
            execution_mode: None,
            tool: Tool {
                name: "echo".to_owned(),
                description: "echo".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": { "msg": { "type": "string" } },
                    "required": ["msg"]
                }),
                freeform: None,
                output_schema: None,
                constrained_sampling: None,
            },
        }
    }

    struct BlockingHandlers;
    impl LooplessToolHandlers for BlockingHandlers {
        fn before_tool_call<'a>(
            &'a self,
            _event: BeforeToolCallArgs<'a>,
        ) -> BoxFuture<'a, Result<Option<ToolCallEventResult>, String>> {
            Box::pin(async {
                Ok(Some(ToolCallEventResult {
                    block: Some(true),
                    reason: Some("denied".to_owned()),
                    terminate: Some(true),
                }))
            })
        }
        fn after_tool_call<'a>(
            &'a self,
            _event: AfterToolCallArgs<'a>,
        ) -> BoxFuture<'a, Result<Option<ToolResultEventResult>, String>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[tokio::test]
    async fn validates_arguments_and_stops_on_block() {
        let ran = Arc::new(AtomicBool::new(false));
        let tool = echo_tool(Arc::clone(&ran));

        let invalid = run_loopless_tool_call(
            &tool,
            LooplessToolCallRequest {
                tool_call_id: "1".into(),
                params: json!({}),
                signal: None,
                nested: None,
            },
            None,
        )
        .await;
        assert_eq!(invalid.is_error, Some(true));
        assert!(!ran.load(Ordering::SeqCst));

        let handlers: Arc<dyn LooplessToolHandlers> = Arc::new(BlockingHandlers);
        let blocked = run_loopless_tool_call(
            &tool,
            LooplessToolCallRequest {
                tool_call_id: "2".into(),
                params: json!({"msg": "hi"}),
                signal: None,
                nested: None,
            },
            Some(&handlers),
        )
        .await;
        assert_eq!(blocked.is_error, Some(true));
        assert_eq!(blocked.terminate, Some(true));
        assert!(!ran.load(Ordering::SeqCst));

        let ok = run_loopless_tool_call(
            &tool,
            LooplessToolCallRequest {
                tool_call_id: "3".into(),
                params: json!({"msg": "hi"}),
                signal: None,
                nested: None,
            },
            None,
        )
        .await;
        assert!(ok.is_error.is_none());
        assert!(ran.load(Ordering::SeqCst));
    }
}
