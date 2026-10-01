//! Port of senpi `packages/agent/src/harness/execution/tools.ts`.

use std::sync::Arc;

use maho_ai::types::{ContentBlock, ToolCall, ToolResultMessage, Usage};
use maho_ai::utils::validation::validate_tool_arguments;
use serde_json::{Map, Value};

use crate::harness::context::{Context, with_abort_signal};
use crate::harness::execution::effect_gate::{Gate, GateRefusal};
use crate::harness::session::types::JsonValue;
use crate::harness::types::{AgentHarnessTool, AgentHarnessToolInvocation, AgentHarnessToolUpdateCallback};
use crate::types::AgentToolResult;

pub struct PreparedToolCall<TContext> {
    pub tool_call: ToolCall,
    pub tool: Arc<AgentHarnessTool<TContext>>,
    pub args: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImmediateToolOutcome {
    pub tool_call: ToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub terminate: bool,
}

impl ImmediateToolOutcome {
    pub const fn kind(&self) -> &'static str {
        "immediate"
    }
}

pub enum ToolCallPreparation<TContext> {
    Prepared(PreparedToolCall<TContext>),
    Immediate(ImmediateToolOutcome),
}

impl<TContext> ToolCallPreparation<TContext> {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Prepared(_) => "prepared",
            Self::Immediate(_) => "immediate",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeToolDecision {
    pub args: Option<Map<String, Value>>,
    pub block: Option<BlockDecision>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockDecision {
    pub reason: String,
    pub terminate: Option<bool>,
}

pub struct ClearedToolCall<TContext> {
    pub tool_call: ToolCall,
    pub tool: Arc<AgentHarnessTool<TContext>>,
    pub args: Map<String, Value>,
}

pub enum ClearToolCallOutcome<TContext> {
    Cleared(ClearedToolCall<TContext>),
    Immediate(ImmediateToolOutcome),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutedToolCall {
    pub result: AgentToolResult,
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterToolPatch {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedToolCall {
    pub tool_call: ToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub terminate: bool,
}

pub fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult::text(message)
}

fn immediate_error(tool_call: ToolCall, message: &str, terminate: bool) -> ImmediateToolOutcome {
    ImmediateToolOutcome {
        tool_call,
        result: create_error_tool_result(message),
        is_error: true,
        terminate,
    }
}

pub fn prepare_tool_call<TContext>(
    call: ToolCall,
    tools: &[Arc<AgentHarnessTool<TContext>>],
) -> ToolCallPreparation<TContext> {
    let Some(tool) = tools.iter().find(|candidate| candidate.name() == call.name).cloned() else {
        return ToolCallPreparation::Immediate(immediate_error(
            call.clone(),
            &format!("Tool {} is unavailable", quote(&call.name)),
            false,
        ));
    };

    let prepared_arguments = match &tool.prepare_arguments {
        Some(prepare) => Some(prepare(Value::Object(call.arguments.clone()))),
        None => None,
    };

    let prepared_call = match prepared_arguments {
        Some(prepared) => {
            let arguments = match prepared {
                Value::Object(map) => map,
                _ => call.arguments.clone(),
            };
            ToolCall { arguments, ..call.clone() }
        }
        None => call.clone(),
    };

    match validate_tool_arguments(&tool.tool, &prepared_call) {
        Ok(args) => ToolCallPreparation::Prepared(PreparedToolCall {
            tool_call: call,
            tool,
            args: match args {
                Value::Object(map) => map,
                _ => Map::new(),
            },
        }),
        Err(error) => ToolCallPreparation::Immediate(immediate_error(call, &error.to_string(), false)),
    }
}

pub fn apply_before_tool_decision<TContext>(
    prepared: PreparedToolCall<TContext>,
    decision: Option<BeforeToolDecision>,
) -> ClearToolCallOutcome<TContext> {
    if let Some(block) = decision.as_ref().and_then(|decision| decision.block.clone()) {
        return ClearToolCallOutcome::Immediate(immediate_error(
            prepared.tool_call,
            &block.reason,
            block.terminate.unwrap_or(false),
        ));
    }

    let Some(args) = decision.and_then(|decision| decision.args) else {
        return ClearToolCallOutcome::Cleared(ClearedToolCall {
            tool_call: prepared.tool_call,
            tool: prepared.tool,
            args: prepared.args,
        });
    };

    let replacement = ToolCall { arguments: args, ..prepared.tool_call.clone() };
    match validate_tool_arguments(&prepared.tool.tool, &replacement) {
        Ok(validated) => ClearToolCallOutcome::Cleared(ClearedToolCall {
            tool_call: prepared.tool_call,
            tool: prepared.tool,
            args: match validated {
                Value::Object(map) => map,
                _ => Map::new(),
            },
        }),
        Err(error) => {
            ClearToolCallOutcome::Immediate(immediate_error(prepared.tool_call, &error.to_string(), false))
        }
    }
}

pub async fn execute_tool_call<TContext: Clone>(
    call: ClearedToolCall<TContext>,
    gate: &Gate,
    on_update: AgentHarnessToolUpdateCallback,
    tool_context: TContext,
    invocation: Arc<dyn AgentHarnessToolInvocation>,
    context: Context,
) -> Result<ExecutedToolCall, GateRefusal> {
    gate.admit(|| ())?;
    let admitted_context = with_abort_signal(gate.signal(), &context);
    if let Some(signal) = admitted_context.abort_signal().filter(|signal| signal.aborted()) {
        return Err(GateRefusal::Closed(
            signal.reason().map_or_else(|| "The operation was aborted".to_owned(), |reason| reason.message),
        ));
    }
    let accepting_updates = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let update_flag = accepting_updates.clone();
    let update_callback: AgentHarnessToolUpdateCallback = Arc::new(move |partial, options| {
        if update_flag.load(std::sync::atomic::Ordering::SeqCst) {
            on_update(partial, options);
        }
    });
    let execute = call.tool.execute.clone();
    let tool_call_id = call.tool_call.id.clone();
    let arguments = Value::Object(call.args.clone());
    let result = execute(tool_call_id, arguments, update_callback, tool_context, invocation, admitted_context).await;
    accepting_updates.store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(match result {
        Ok(result) => ExecutedToolCall { result, is_error: false },
        Err(error) => ExecutedToolCall { result: create_error_tool_result(&error), is_error: true },
    })
}

pub fn finalize_tool_call<TContext>(
    call: &ClearedToolCall<TContext>,
    executed: ExecutedToolCall,
    patch: Option<AfterToolPatch>,
) -> FinalizedToolCall {
    let executed_is_error = executed.is_error;
    let is_error = patch.as_ref().and_then(|patch| patch.is_error).unwrap_or(executed_is_error);
    let result = match patch {
        Some(patch) => AgentToolResult {
            content: patch.content.unwrap_or(executed.result.content),
            details: patch.details.unwrap_or(executed.result.details),
            usage: patch.usage.or(executed.result.usage),
            added_tool_names: executed.result.added_tool_names,
            terminate: patch.terminate.or(executed.result.terminate),
            is_error: executed.result.is_error,
        },
        None => executed.result,
    };
    FinalizedToolCall {
        tool_call: call.tool_call.clone(),
        is_error,
        terminate: result.terminate == Some(true),
        result,
    }
}

pub fn tool_result_from_message(message: &ToolResultMessage, terminate: bool) -> AgentToolResult {
    AgentToolResult {
        content: message.content.clone(),
        details: message.details.clone().unwrap_or(Value::Object(Map::new())),
        usage: message.usage,
        added_tool_names: message.added_tool_names.clone(),
        terminate: if terminate { Some(true) } else { None },
        is_error: None,
    }
}

pub fn create_tool_result_message(call: &FinalizedToolCall) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.tool_call.id.clone(),
        tool_name: call.tool_call.name.clone(),
        content: call.result.content.clone(),
        details: Some(call.result.details.clone()),
        usage: call.result.usage,
        added_tool_names: call.result.added_tool_names.clone().filter(|names| !names.is_empty()),
        is_error: call.is_error,
        timestamp: now_ms(),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis() as i64)
}

fn quote(name: &str) -> String {
    serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""))
}
