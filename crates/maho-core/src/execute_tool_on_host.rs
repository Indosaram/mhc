//! Port of senpi packages/coding-agent/src/core/execute-tool-on-host.ts.
//!
//! Runs a prepared tool call on the in-process prepared `ToolHost` (`S19a`).

use std::sync::{Arc, Mutex};

use maho_agent::agent_loop::PreparedAgentToolCall;
use maho_agent::execution::prepared::executor_pipeline::PreparationSource;
use maho_agent::execution::prepared::pipeline_prepared::create_prepared_tool_host;
use maho_agent::execution::tool_host::{
    BatchStoreMissingError, ExecutionScope, ToolHostCall, ToolHostLocalContext, ToolHostOptions,
    WitnessRecorder,
};
use maho_agent::types::{
    AfterToolCall, AfterToolCallResult, AgentContext, AgentTool, AgentToolResult,
    AgentToolUpdateCallback, BeforeToolCall, ToolExecutionMode,
};
use maho_ai::types::{AssistantMessage, BoxFuture, ContentBlock, StopReason, Tool, Usage};
use maho_ai::utils::abort::AbortSignal;
use serde_json::json;

use crate::execution::preflight_carry::carry_preflight_facts;

#[derive(Clone, Debug, Default)]
pub struct ExecuteToolHookResult {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<serde_json::Value>,
    /// The error flag after the hooks: the hook's, else the tool's (`senpi#6904`).
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ExecuteToolHostOutcome {
    pub result: AgentToolResult,
    pub hook: Option<ExecuteToolHookResult>,
}

pub type AfterHooksFn = Arc<
    dyn Fn(
            PreparedAgentToolCall,
            AgentToolResult,
            bool,
        ) -> BoxFuture<'static, Result<Option<ExecuteToolHookResult>, String>>
        + Send
        + Sync,
>;

pub struct ExecuteToolHostSession<'a> {
    pub agent: &'a maho_agent::Agent,
    pub after_hooks: AfterHooksFn,
}

#[derive(Clone, Default)]
pub struct ExecuteToolHostOptions {
    pub signal: Option<AbortSignal>,
    pub on_update: Option<AgentToolUpdateCallback>,
}

fn issuing_message(prepared: &PreparedAgentToolCall) -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::ToolCall(prepared.tool_call.clone())],
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        api: "execute-tool".into(),
        provider: "senpi".into(),
        model: "execute-tool".into(),
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
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64),
    }
}

pub async fn run_execute_tool_on_host(
    session: ExecuteToolHostSession<'_>,
    prepared: PreparedAgentToolCall,
    options: ExecuteToolHostOptions,
) -> Result<ExecuteToolHostOutcome, String> {
    let Some(open_store) = session.agent.batch_store_factory() else {
        return Err(BatchStoreMissingError.to_string());
    };
    let tool = prepared.tool.clone();
    let user_signal = options.signal.clone();
    let user_on_update = options.on_update.clone();

    let mut body_tool = Tool {
        name: tool.tool.name.clone(),
        description: tool.tool.description.clone(),
        parameters: json!({}),
        output_schema: tool.tool.output_schema.clone(),
        freeform: tool.tool.freeform.clone(),
        constrained_sampling: tool.tool.constrained_sampling.clone(),
    };
    body_tool.parameters = json!({});

    let body = AgentTool {
        label: tool.label.clone(),
        prepare_arguments: None,
        replay: tool.replay,
        execution_mode: tool.execution_mode,
        tool: body_tool,
        execute: Arc::new(move |tool_call_id, args, _signal, _on_update| {
            let exec = Arc::clone(&tool.execute);
            let sig = user_signal.clone();
            let upd = user_on_update.clone();
            Box::pin(async move { exec(tool_call_id, args, sig, upd).await })
        }),
    };

    let prepared_args = prepared.args.clone();
    let prepared_args_for_before = prepared_args.clone();
    let before_tool_call: BeforeToolCall = Arc::new(move |ctx, _sig| {
        let mut frozen = ctx.args.clone();
        carry_preflight_facts(&prepared_args_for_before, &mut frozen);
        Box::pin(async move { None })
    });

    let settled: Arc<Mutex<Option<ExecuteToolHostOutcome>>> = Arc::new(Mutex::new(None));
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let settled_slot = Arc::clone(&settled);
    let failure_slot = Arc::clone(&failure);
    let after_hooks = Arc::clone(&session.after_hooks);
    let prepared_for_after = PreparedAgentToolCall {
        tool_call: prepared.tool_call.clone(),
        tool: prepared.tool.clone(),
        args: prepared.args.clone(),
    };

    let after_tool_call: AfterToolCall = Arc::new(move |ctx, _sig| {
        let after_hooks = Arc::clone(&after_hooks);
        let settled_slot = Arc::clone(&settled_slot);
        let failure_slot = Arc::clone(&failure_slot);
        let prep = PreparedAgentToolCall {
            tool_call: prepared_for_after.tool_call.clone(),
            tool: prepared_for_after.tool.clone(),
            args: prepared_for_after.args.clone(),
        };
        Box::pin(async move {
            let result = ctx.result.clone();
            let is_error = ctx.is_error;
            match after_hooks(prep, result.clone(), is_error).await {
                Ok(hook) => {
                    *settled_slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(ExecuteToolHostOutcome { result, hook });
                }
                Err(err) => {
                    *failure_slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(err);
                }
            }
            None::<AfterToolCallResult>
        })
    });

    let host = create_prepared_tool_host(
        PreparationSource {
            after_tool_freeze: Arc::new(carry_preflight_facts),
            before_tool_call: Some(before_tool_call),
            after_tool_call: Some(after_tool_call),
            removed_tool_hints: None,
            resolve_unknown_tool_call: None,
        },
        open_store,
    );

    let mut call_with_prepared_args = prepared.tool_call.clone();
    if let serde_json::Value::Object(map) = &prepared.args {
        call_with_prepared_args.arguments = map.clone();
    }
    let scope: ExecutionScope = session.agent.execution_scope();
    let record_witness: Option<WitnessRecorder> = session.agent.on_execution_pipeline();

    host.execute(
        ToolHostCall {
            scope,
            calls: vec![call_with_prepared_args],
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
                assistant_message: issuing_message(&prepared),
            }),
        },
    )
    .await?;

    if let Some(err) = failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        return Err(err);
    }
    settled
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .ok_or_else(|| {
            format!(
                "executeTool: the prepared host settled no result for {}",
                prepared.tool_call.name
            )
        })
}
