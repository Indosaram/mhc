//! Port of senpi `packages/agent/src/harness/runtime/drive/generation.ts`.

use std::sync::Arc;
use crate::harness::events::{HarnessEvent, HarnessEventPayload};
use crate::harness::hooks::{HookInvocation, HookName, HookResult, apply_stream_options_patch};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::runtime::types::*;
use crate::harness::runtime::transcript::read_bounded_context;
use super::response::{publish_configuration_failure, publish_response};

pub async fn run_retry_wait(lane: &dyn RuntimeDriveLane, drive: &Drive, generation: &OperationState) -> Result<ProcedureResult, SessionError> {
    let OperationState::AssistantRetryWait(wait) = generation else { return Err(session_invariant_error("Retry wait requires assistant.retry_wait")); };
    if super::retry::now_ms() < wait.retry_wait.not_before {
        if !drive.wait_for_retry { return Ok(ProcedureResult::Waiting { outcome: crate::harness::agent_harness::DriveOutcome::WaitingRetry { operation_id: drive.operation_id.clone(), not_before: wait.retry_wait.not_before } }); }
        drive.gate.admit(|| ()).map_err(|e| session_invariant_error(e.to_string()))?;
        super::retry::wait_until(wait.retry_wait.not_before, &drive.gate.signal()).await.map_err(|e| session_invariant_error(e.message))?;
    }
    let result = settle_operation(lane, generation, &drive.context, true, |_, op, _| async move {
        let OperationState::AssistantRetryWait(s) = op.state else { return Err(session_invariant_error("Retry wait state changed")); };
        let event = HarnessEvent::new(HarnessEventPayload::RetryStart { run_id: drive.operation_id.clone(), step: s.assistant.generation_context.step_id.clone(), attempt: s.retry_wait.next_attempt }, Some(lane.name().into()));
        let next = OperationState::AssistantReady(AssistantReadyOperation { operation: s.operation, assistant: s.assistant, next_attempt: s.retry_wait.next_attempt, at: OperationMarker::AssistantReady });
        Ok(OperationCommand::Commit { decision: CommitDecision { writes: Vec::new(), materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(Arc::new(move |_| vec![event.clone()])) }, operation_state: Box::new(next), lane: None })
    }).await?;
    Ok(match result { ContinueOperationResult::CancelRequested => ProcedureResult::Continue, ContinueOperationResult::Result { value } => value })
}

pub async fn run_generation(lane: &dyn RuntimeDriveLane, drive: &Drive, generation: &OperationState) -> Result<ProcedureResult, SessionError> {
    if matches!(generation, OperationState::AssistantRetryWait(_)) { return run_retry_wait(lane, drive, generation).await; }
    let OperationState::AssistantReady(ready) = generation else { return Err(session_invariant_error("Generation requires assistant.ready")); };
    let identity = &ready.assistant.generation_context.configuration.model;
    let Some(model) = lane.models().get_model(&identity.provider, &identity.model_id) else { return publish_configuration_failure(lane, drive, generation, OperationError { code: "model_unavailable".into(), message: "The configured model is unavailable in this process".into(), details: Some(serde_json::json!(identity)) }).await; };
    let config = lane.config();
    let mut tools = Vec::new(); let mut missing = Vec::new();
    for name in &ready.assistant.generation_context.configuration.active_tool_names {
        match config.tools.iter().find(|tool| tool.name() == name) { Some(tool) => tools.push(tool.tool.clone()), None => missing.push(name.clone()) }
    }
    if !missing.is_empty() { return publish_configuration_failure(lane, drive, generation, OperationError { code: "configured_tools_unavailable".into(), message: "One or more configured tools are unavailable in this process".into(), details: Some(serde_json::json!({"tools":missing})) }).await; }
    let build_options = crate::harness::session::context::SessionContextBuildOptions { entry_projectors: config.entry_projectors.clone() };
    let mut messages = match read_bounded_context(lane, drive, generation, &build_options).await? { ContinueOperationResult::CancelRequested => return Ok(ProcedureResult::Continue), ContinueOperationResult::Result { value } => value };
    let mut system_prompt = match &config.system_prompt { Some(prompt) => prompt(&drive.context).await, None => String::new() };
    let mut invocation = HookInvocation::new(lane.name(), &drive.operation_id);
    invocation.model = Some(model.clone()); invocation.step = Some("assistant".into()); invocation.attempt = Some(ready.next_attempt); invocation.stream_options = ready.assistant.generation_context.stream_options.clone();
    let stream_options = match lane.hooks().run_with_gate(HookName::BeforeRequest, invocation, &drive.gate, &drive.context).await.map_err(|e| session_invariant_error(e.to_string()))? { HookResult::BeforeRequest { stream_options } => apply_stream_options_patch(&ready.assistant.generation_context.stream_options, &stream_options), _ => ready.assistant.generation_context.stream_options.clone() };
    let at = super::retry::now_ms();
    let pending = OperationState::AssistantEffectPending(AssistantEffectPendingOperation { operation: ready.operation.clone(), assistant: ready.assistant.clone(), attempt: ready.next_attempt, response_entry_id: (lane.session().id_generator())(Some(at)), usage_id: (lane.session().id_generator())(Some(at)), intended_output_limit: model.max_tokens, context_window: model.context_window, at: OperationMarker::AssistantEffectPending });
    let intent = pending.clone();
    let first_attempt = ready.next_attempt == 1;
    let result = settle_operation(lane, generation, &drive.context, true, |_, _, _| async move {
        let next = pending.clone();
        let event = HarnessEvent::new(HarnessEventPayload::TurnStart { run_id: drive.operation_id.clone(), turn_id: ready.assistant.generation_context.step_id.clone() }, Some(lane.name().into()));
        Ok(OperationCommand::Commit { decision: CommitDecision { writes: Vec::new(), materialize: Arc::new(move |_| next.clone()), events: Some(Arc::new(move |_| if first_attempt { vec![event.clone()] } else { Vec::new() })) }, operation_state: Box::new(pending), lane: None })
    }).await?;
    if matches!(result, ContinueOperationResult::CancelRequested) { return Ok(ProcedureResult::Continue); }
    let mut transform = HookInvocation::new(lane.name(), &drive.operation_id);
    transform.messages = messages.clone(); transform.system_prompt = system_prompt.clone();
    if let HookResult::TransformContext { messages: replacement, system_prompt: prompt } = lane.hooks().run_with_gate(HookName::TransformContext, transform, &drive.gate, &drive.context).await.map_err(|e| session_invariant_error(e.to_string()))? {
        if let Some(replacement) = replacement { messages = replacement; }
        if let Some(prompt) = prompt { system_prompt = prompt; }
    }
    let mut options = maho_ai::types::SimpleStreamOptions { deferred: stream_options.deferred, ..Default::default() };
    options.stream.transport = stream_options.transport;
    options.stream.cache_retention = stream_options.cache_retention;
    options.stream.metadata = stream_options.metadata.clone();
    options.stream.request.timeout_ms = stream_options.timeout_ms;
    options.stream.request.max_retries = stream_options.max_retries;
    options.stream.request.max_retry_delay_ms = stream_options.max_retry_delay_ms;
    options.stream.request.headers = stream_options.headers.as_ref().map(|headers| headers.iter().map(|(key,value)| (key.clone(), value.as_str().map(str::to_owned))).collect());
    options.reasoning = match ready.assistant.generation_context.configuration.thinking_level {
        maho_ai::types::ModelThinkingLevel::Off => None,
        maho_ai::types::ModelThinkingLevel::Minimal => Some(maho_ai::types::ThinkingLevel::Minimal),
        maho_ai::types::ModelThinkingLevel::Low => Some(maho_ai::types::ThinkingLevel::Low),
        maho_ai::types::ModelThinkingLevel::Medium => Some(maho_ai::types::ThinkingLevel::Medium),
        maho_ai::types::ModelThinkingLevel::High => Some(maho_ai::types::ThinkingLevel::High),
        maho_ai::types::ModelThinkingLevel::Xhigh => Some(maho_ai::types::ThinkingLevel::Xhigh),
        maho_ai::types::ModelThinkingLevel::Max => Some(maho_ai::types::ThinkingLevel::Max),
    };
    options.stream.request.signal = Some(drive.gate.signal());
    options.stream.session_id = Some(format!("{}:{}", lane.session().metadata().id, lane.name()));
    let provider_messages = (config.to_provider_messages)(messages, &drive.context).await;
    let context = maho_ai::types::Context { messages: provider_messages, system_prompt: Some(system_prompt), tools: Some(tools) };
    let stream = drive.gate.admit(|| lane.models().stream_simple(&model, &context, Some(options), maho_ai::models::ModelsRequestTransforms::default())).map_err(|e| session_invariant_error(e.to_string()))?;
    let response = super::response::consume_response(lane, drive, &intent, &stream, false).await?;
    publish_response(lane, drive, &intent, response, false).await
}
