//! Port of senpi `packages/agent/src/harness/execution/tool-host-call.ts`.

use std::sync::{Arc, Mutex};

use maho_ai::types::{AssistantMessage, BoxFuture, StopReason, Tool, Usage};
use serde_json::json;

use crate::assistant_terminal_state::now_ms;
use crate::execution::prepared::executor_pipeline::PreparationSource;
use crate::execution::prepared::memory_batch_store::open_memory_batch_store;
use crate::execution::prepared::pipeline_prepared::create_prepared_tool_host;
use crate::execution::tool_host::{
    ExecutionScope, ToolHostCall, ToolHostLocalContext, ToolHostOptions, WitnessRecorder,
    no_preflight_carry_fn,
};
use crate::types::{AgentContext, AgentTool, AgentToolCall, AgentToolResult, ToolExecutionMode};

pub type WitnessSink = WitnessRecorder;

fn issuing_message(tool_call: &AgentToolCall) -> AssistantMessage {
    AssistantMessage {
        content: vec![maho_ai::types::ContentBlock::ToolCall(tool_call.clone())],
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        api: "agent-harness".into(),
        provider: "agent-harness".into(),
        model: "agent-harness".into(),
        response_model: None,
        response_id: None,
        thinking_level: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        stop_details: None,
        deferred: None,
        error_message: None,
        error_cause: None,
        provider_diagnostic: None,
        abort_source: None,
        failure_source: None,
        credential_served_recently: None,
        raw_stop_reason: None,
        end_turn: None,
        timing: None,
        timestamp: now_ms(),
    }
}

/// Runs one cleared harness call's body on the in-process prepared `ToolHost` (S19c), as a one-call batch
/// on its own in-memory ledger.
pub async fn run_on_prepared_tool_host<T, F>(
    tool_call: AgentToolCall,
    scope_id: &str,
    record_witness: Option<WitnessSink>,
    run: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> BoxFuture<'static, (T, AgentToolResult)> + Send + 'static,
{
    let settled: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let settled_slot = settled.clone();
    let run_slot = Arc::new(Mutex::new(Some(run)));
    let body = AgentTool {
        label: tool_call.name.clone(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: Tool {
            name: tool_call.name.clone(),
            description: tool_call.name.clone(),
            parameters: json!({}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, _, _, _| {
            let run_fn = run_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            let settled_slot = settled_slot.clone();
            Box::pin(async move {
                let Some(run_fn) = run_fn else {
                    return AgentToolResult {
                        is_error: Some(true),
                        ..AgentToolResult::text("prepared host invoked body more than once")
                    };
                };
                let (outcome, result) = run_fn().await;
                *settled_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outcome);
                result
            })
        }),
    };

    let host = create_prepared_tool_host(
        PreparationSource {
            after_tool_freeze: no_preflight_carry_fn(),
            removed_tool_hints: None,
            resolve_unknown_tool_call: None,
            before_tool_call: None,
            after_tool_call: None,
        },
        open_memory_batch_store(),
    );

    let host_result = host
        .execute(
            ToolHostCall {
                scope: ExecutionScope {
                    id: scope_id.to_owned(),
                    root: String::new(),
                },
                calls: vec![tool_call.clone()],
                mode: ToolExecutionMode::Sequential,
            },
            ToolHostOptions {
                signal: None,
                emit: Arc::new(|_| Box::pin(async {})),
                record_witness,
                local: Some(ToolHostLocalContext {
                    context: AgentContext {
                        system_prompt: String::new(),
                        system_prompt_stable_prefix: None,
                        messages: Vec::new(),
                        tools: Some(vec![body]),
                        declared_tools: None,
                    },
                    assistant_message: issuing_message(&tool_call),
                }),
            },
        )
        .await;

    let taken = settled
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(outcome) = taken {
        return Ok(outcome);
    }
    host_result?;
    Err(format!(
        "agent harness: the prepared host ran no body for {}",
        tool_call.name
    ))
}
