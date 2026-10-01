//! Port of senpi `packages/agent/src/harness/runtime/drive/response.ts`.

use std::sync::Arc;
use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use crate::harness::events::{HarnessEvent, HarnessEventPayload, RunEndPayload};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::session::commit::{insert_entry, insert_usage};
use crate::harness::session::values::{branch_tip, delete_list, pending_assistant_frames, operation_preparation, set_value};
use crate::harness::runtime::types::*;
use super::terminal::{operation_cleanup_writes, operation_result_record};

pub async fn consume_response(lane: &dyn RuntimeDriveLane, drive: &Drive, intent: &OperationState, stream: &maho_ai::types::AssistantMessageEventStream, recovery: bool) -> Result<AssistantMessage, SessionError> {
    use maho_ai::types::AssistantMessageEvent;
    use maho_ai::utils::assistant_message_frame::AssistantMessageFrameEncoder;
    let response_id = match intent { OperationState::AssistantEffectPending(s) => &s.response_entry_id, OperationState::DeferredEffectPending(s) => &s.response_entry_id, _ => return Err(session_invariant_error("Assistant response requires an effect-pending state")) };
    let mut encoder = AssistantMessageFrameEncoder::new();
    let progress = crate::harness::runtime::progress::open_frame_progress(lane.progress_lane(), drive, response_id);
    let mut started = false;
    while let Some(event) = stream.next().await.map_err(|e| session_invariant_error(e.message))? {
        match &event {
            AssistantMessageEvent::Start { .. } if started => return Err(session_invariant_error("Assistant message stream emitted more than one start event")),
            AssistantMessageEvent::Start { .. } => started = true,
            AssistantMessageEvent::Error { .. } => {},
            _ if !started => return Err(session_invariant_error("Assistant message stream emitted an event before start")),
            _ => {},
        }
        let frame = encoder.encode(&event).map_err(|e| session_invariant_error(e.to_string()))?;
        if let Some(frame) = &frame { progress.write(frame.clone()); }
        let source_event = event.clone();
        let (message, start) = match event {
            AssistantMessageEvent::Start { partial } => (partial, true),
            AssistantMessageEvent::TextStart { partial, .. } | AssistantMessageEvent::TextDelta { partial, .. } | AssistantMessageEvent::TextEnd { partial, .. } | AssistantMessageEvent::ThinkingStart { partial, .. } | AssistantMessageEvent::ThinkingDelta { partial, .. } | AssistantMessageEvent::ThinkingEnd { partial, .. } | AssistantMessageEvent::ToolcallStart { partial, .. } | AssistantMessageEvent::ToolcallDelta { partial, .. } | AssistantMessageEvent::ToolcallEnd { partial, .. } => (partial, false),
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => continue,
        };
        let payload = if start { HarnessEventPayload::MessageStart { run_id: Some(drive.operation_id.clone()), message: crate::types::AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(message))) } } else { HarnessEventPayload::MessageUpdate { run_id: drive.operation_id.clone(), message: crate::types::AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(message))), event: Box::new(source_event), frame } };
        let mut event = HarnessEvent::new(payload, Some(lane.name().into())); event.recovery = recovery.then_some(true);
        lane.emit(vec![event], &drive.context).await;
    }
    progress.seal(); progress.drain().await?;
    let mut message = stream.result().await.map_err(|e| session_invariant_error(e.message))?;
    if message.stop_reason == StopReason::Pending { return Err(session_invariant_error("Assistant stream settled with a pending stop reason")); }
    let mut invocation = crate::harness::hooks::HookInvocation::new(lane.name(), &drive.operation_id); invocation.message = Some(message.clone());
    match lane.hooks().run_with_gate(crate::harness::hooks::HookName::AfterResponse, invocation, &drive.gate, &drive.context).await {
        Ok(crate::harness::hooks::HookResult::AfterResponse { message: replacement }) => message = *replacement,
        Ok(_) => {},
        Err(crate::harness::hooks::HookRunError::Gate(crate::harness::execution::effect_gate::GateRefusal::Abort(abort))) => abort.cancellation.wait().await,
        Err(error) => return Err(session_invariant_error(error.to_string())),
    }
    let mut end = HarnessEvent::new(HarnessEventPayload::MessageEnd { run_id: Some(drive.operation_id.clone()), message: crate::types::AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(message.clone()))), entry_id: Some(response_id.clone()) }, Some(lane.name().into())); end.recovery = recovery.then_some(true); lane.emit(vec![end], &drive.context).await;
    Ok(message)
}

pub async fn publish_configuration_failure(lane: &dyn RuntimeDriveLane, drive: &Drive, capability: &OperationState, error: OperationError) -> Result<ProcedureResult, SessionError> {
    let result = settle_operation(lane, capability, &drive.context, true, |state, op, reader| async move {
        if state.tip_id.is_none() { return Err(session_invariant_error("Failed run has no Branch tip")); }
        let record = operation_result_record(&op.meta, TerminalStatus::Failed, state.tip_id.clone(), Some(error.clone()))?;
        let writes = operation_cleanup_writes(reader.as_ref(), &drive.operation_id, &op.state, &drive.context).await?;
        let end = HarnessEvent::new(HarnessEventPayload::RunEnd(RunEndPayload { run_id: drive.operation_id.clone(), from_tip_id: op.meta.source_tip_id, tip_id: state.tip_id, ended_at: record.ended_at, status: "failed".into(), error: Some(error) }), Some(lane.name().into()));
        let outcome = record.clone();
        Ok(OperationCommand::Finish { decision: Box::new(FinishDecision { writes, record, lane: None, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: outcome.clone() }), events: Some(Arc::new(move |_| vec![end.clone()])) }) })
    }).await?;
    Ok(match result { ContinueOperationResult::CancelRequested => ProcedureResult::Continue, ContinueOperationResult::Result { value } => value })
}

fn provider_error(source: &str, message: &AssistantMessage) -> OperationError {
    OperationError { code: "assistant_error".into(), message: message.error_message.clone().unwrap_or_else(|| format!("{source} request ended with {}", serde_json::to_value(message.stop_reason).unwrap_or_default().as_str().unwrap_or_default())), details: None }
}

pub async fn publish_response(lane: &dyn RuntimeDriveLane, drive: &Drive, intent: &OperationState, response: AssistantMessage, recovery: bool) -> Result<ProcedureResult, SessionError> {
    let overflow = match intent { OperationState::AssistantEffectPending(s) => maho_ai::utils::overflow::is_context_overflow(&response, Some(s.context_window)) || maho_ai::utils::overflow::is_recoverable_length(&response, s.intended_output_limit), _ => false };
    let recovery_used = matches!(intent, OperationState::AssistantEffectPending(s) if s.assistant.generation_context.overflow_recovery_used);
    let overflow_preparation = if overflow && !recovery_used { lane.prepare_overflow_compaction(drive, intent).await? } else { None };
    let result = settle_operation(lane, intent, &drive.context, false, |state, op, reader| async move {
        let (id, usage_id, configuration, turn_id, source) = match &op.state {
            OperationState::AssistantEffectPending(s) => (s.response_entry_id.clone(), s.usage_id.clone(), s.assistant.generation_context.configuration.clone(), s.assistant.generation_context.step_id.clone(), "Assistant"),
            OperationState::DeferredEffectPending(s) => (s.response_entry_id.clone(), s.usage_id.clone(), s.deferred.configuration.clone(), format!("{}:poll:{}", s.deferred.step_id, s.deferred.poll), "Deferred"),
            _ => return Err(session_invariant_error("Response settlement requires an effect-pending state")),
        };
        let mut message = response;
        let mut scope = op.state.operation_scope_of(); scope.latest_assistant_entry_id = Some(id.clone());
        let checkpoint = |scope| OperationState::Checkpoint(CheckpointOperation { operation: scope, at: OperationMarker::Checkpoint, checkpoint: CheckpointData { continuation: Continuation::MayFinish { include_final_assistant: true }, trigger_entry_id: id.clone() } });
        let mut failure = None;
        let next = if matches!(scope.control, Control::CancelRequested { .. }) {
            message.stop_reason = StopReason::Aborted;
            message.error_message.get_or_insert_with(|| format!("{source} request was cancelled"));
            Some(checkpoint(scope))
        } else if message.stop_reason == StopReason::Aborted { return Err(session_invariant_error(format!("{source} response is aborted while durable control is running"))); }
        else if overflow {
            message.stop_reason = StopReason::Error;
            message.error_message.get_or_insert_with(|| "Assistant request exceeded the context window".into());
            match (&op.state, &overflow_preparation) {
                (OperationState::AssistantEffectPending(s), Some(preparation)) if !s.assistant.generation_context.overflow_recovery_used => Some(OperationState::SummaryDeciding(SummaryDecidingOperation { operation: scope, at: OperationMarker::SummaryDeciding, task: SummaryTask { task_id: preparation.task_id.clone(), reason: Some(SummaryTaskReason::Overflow), custom_instructions: None, boundary: ResultBoundary::ResumeCheckpoint { resume_after: CheckpointData { continuation: Continuation::NeedAssistant { overflow_recovery_used: true }, trigger_entry_id: s.assistant.generation_context.trigger_entry_id.clone() } } } })),
                _ => { failure = Some(provider_error(source, &message)); None }
            }
        } else { match message.stop_reason {
            StopReason::Deferred => {
                let valid = message.deferred.as_ref().is_some_and(|h| !h.id.is_empty() && h.provider == configuration.model.provider && h.model_id == configuration.model.model_id && h.api == message.api);
                if !valid && matches!(op.state, OperationState::AssistantEffectPending(_)) { message.stop_reason = StopReason::Error; message.error_message = Some("Provider returned an invalid deferred handle".into()); failure = Some(provider_error(source, &message)); None }
                else { let (step_id, poll, stream_options) = match &op.state { OperationState::AssistantEffectPending(s) => (s.assistant.generation_context.step_id.clone(), 0, s.assistant.generation_context.stream_options.clone()), OperationState::DeferredEffectPending(s) => (s.deferred.step_id.clone(), s.deferred.poll, s.deferred.stream_options.clone()), _ => return Err(session_invariant_error("Invalid deferred response intent")) }; Some(OperationState::DeferredSuspended(DeferredSuspendedOperation { deferred: DeferredScope { operation: scope, step_id, source_entry_id: id.clone(), poll, configuration: configuration.clone(), stream_options }, at: OperationMarker::DeferredSuspended })) }
            }
            StopReason::Error => {
                match &op.state {
                    OperationState::AssistantEffectPending(s) if (recovery || maho_ai::utils::retry::is_retryable_assistant_error(&message)) && s.attempt < s.assistant.generation_context.retry_policy.max_attempts => {
                        let retry = &s.assistant.generation_context.retry_policy;
                        let policy = maho_ai::utils::retry::RetryPolicy { enabled: true, max_retries: retry.max_attempts.saturating_sub(1), base_delay_ms: retry.base_delay_ms, max_agent_delay_ms: Some(retry.max_agent_delay_ms), random: None };
                        Some(OperationState::AssistantRetryWait(AssistantRetryWaitOperation { operation: scope, assistant: s.assistant.clone(), retry_wait: RetryWait { next_attempt: s.attempt.saturating_add(1), not_before: super::retry::retry_not_before_now(&policy, s.attempt), error_message: message.error_message.clone().unwrap_or_else(|| "Assistant request failed".into()) }, at: OperationMarker::AssistantRetryWait }))
                    }
                    _ => { failure = Some(provider_error(source, &message)); None }
                }
            }
            StopReason::Stop | StopReason::Length | StopReason::ToolUse => {
                let mut calls = Vec::new();
                let timestamp = uuid_v7_timestamp(&id)?;
                for (source_index, block) in message.content.iter().enumerate() { if matches!(block, ContentBlock::ToolCall(_)) { calls.push(ToolCall::Planned { source_index, result_entry_id: (lane.session().id_generator())(Some(timestamp)) }); } }
                if !calls.is_empty() { Some(OperationState::Tools(ToolsOperation { operation: scope, at: OperationMarker::Tools, batch: ToolBatch { assistant_entry_id: id.clone(), configuration, turn_id: turn_id.clone(), calls } })) }
                else if message.stop_reason == StopReason::ToolUse { message.stop_reason = StopReason::Error; message.error_message = Some("Provider reported tool use without any tool calls".into()); failure = Some(provider_error(source, &message)); None }
                else { Some(checkpoint(scope)) }
            }
            StopReason::Pending => return Err(session_invariant_error("Assistant message settled with a pending stop reason")),
            StopReason::Aborted => return Err(session_invariant_error("Unexpected aborted response")),
        }};
        let entry = NewEntry::message(&id, state.tip_id, crate::types::AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(message.clone()))));
        let usage = NewUsageRow { id: usage_id, usage: message.usage, entry_id: Some(id.clone()), adjustment: false, details: None };
        let mut writes = vec![insert_entry(entry.clone()), insert_usage(usage.clone()), Write::Value(set_value(&branch_tip(lane.name()), serde_json::json!(id)))];
        let record = match failure { Some(error) => { writes.extend(operation_cleanup_writes(reader.as_ref(), &drive.operation_id, &op.state, &drive.context).await?); Some(operation_result_record(&op.meta, TerminalStatus::Failed, Some(id.clone()), Some(error))?) }, None => { writes.push(Write::List(delete_list(&pending_assistant_frames(&drive.operation_id, &id)))); None } };
        if matches!(next, Some(OperationState::SummaryDeciding(_)))
            && let Some(preparation) = overflow_preparation { writes.push(Write::Value(set_value(&operation_preparation(&drive.operation_id, &preparation.task_id), preparation.preparation))); }
        let name = lane.name().to_owned();
        let run_id = drive.operation_id.clone();
        let event_state = next.clone(); let event_current = op.state.clone(); let event_record = record.clone(); let from = op.meta.source_tip_id.clone();
        let events: CommitEventsFn = Arc::new(move |commit| {
            let entry = Entry { id: entry.id.clone(), parent_id: entry.parent_id.clone(), kind: entry.kind.clone(), seq: commit.seqs[0], timestamp: commit.timestamp };
            let mut event = HarnessEvent::new(HarnessEventPayload::EntryAdded { entry: Box::new(entry) }, Some(name.clone())); event.recovery = recovery.then_some(true);
            let mut events = vec![event, HarnessEvent::new(HarnessEventPayload::Usage { lane: name.clone(), row: UsageRow { id: usage.id.clone(), usage: usage.usage, entry_id: usage.entry_id.clone(), adjustment: false, seq: commit.seqs[1], details: None }, totals: commit.stats.usage }, Some(name.clone()))];
            if let OperationState::AssistantEffectPending(current) = &event_current {
                if !recovery && current.attempt > 1 && !matches!(event_state, Some(OperationState::AssistantRetryWait(_))) {
                    let success = !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted);
                    events.push(HarnessEvent::new(HarnessEventPayload::RetryEnd { run_id: run_id.clone(), step: turn_id.clone(), attempt: current.attempt, success, final_error: if success { None } else { Some(message.error_message.clone().unwrap_or_else(|| "Assistant request failed".into())) } }, Some(name.clone())));
                }
                if !recovery && let Some(OperationState::AssistantRetryWait(wait)) = &event_state {
                    let retry = &current.assistant.generation_context.retry_policy;
                    let policy = maho_ai::utils::retry::RetryPolicy { enabled: true, max_retries: retry.max_attempts.saturating_sub(1), base_delay_ms: retry.base_delay_ms, max_agent_delay_ms: Some(retry.max_agent_delay_ms), random: None };
                    events.push(HarnessEvent::new(HarnessEventPayload::RetryScheduled { run_id: run_id.clone(), step: turn_id.clone(), attempt: wait.retry_wait.next_attempt, max_attempts: retry.max_attempts, delay_ms: maho_ai::utils::retry::retry_delay_ms(&policy, current.attempt), not_before: wait.retry_wait.not_before, error_message: wait.retry_wait.error_message.clone() }, Some(name.clone())));
                }
                if !recovery && !matches!(event_state, Some(OperationState::Tools(_) | OperationState::AssistantRetryWait(_))) { events.push(HarnessEvent::new(HarnessEventPayload::TurnEnd { run_id: run_id.clone(), turn_id: turn_id.clone(), message: Box::new(message.clone()), tool_results: Vec::new() }, Some(name.clone()))); }
                if matches!(event_state, Some(OperationState::SummaryDeciding(_))) { events.push(HarnessEvent::new(HarnessEventPayload::CompactionStart { run_id: run_id.clone(), reason: "overflow".into(), started_at: commit.timestamp }, Some(name.clone()))); }
            } else if !matches!(event_state, Some(OperationState::Tools(_))) {
                let mut event = HarnessEvent::new(HarnessEventPayload::TurnEnd { run_id: run_id.clone(), turn_id: turn_id.clone(), message: Box::new(message.clone()), tool_results: Vec::new() }, Some(name.clone())); event.recovery = recovery.then_some(true); events.push(event);
            }
            if (matches!(event_current, OperationState::DeferredEffectPending(_)) || !recovery)
                && let (Some(OperationState::DeferredSuspended(s)), Some(handle)) = (&event_state, &message.deferred) {
                    let mut event = HarnessEvent::new(HarnessEventPayload::RunSuspend { run_id: run_id.clone(), deferred: handle.clone(), poll: u32::try_from(s.deferred.poll).unwrap_or(u32::MAX) }, Some(name.clone())); event.recovery = (recovery && matches!(event_current, OperationState::DeferredEffectPending(_))).then_some(true); events.push(event);
            }
            if let Some(record) = &event_record { events.push(HarnessEvent::new(HarnessEventPayload::RunEnd(RunEndPayload { run_id: run_id.clone(), from_tip_id: from.clone(), tip_id: record.tip_id.clone(), ended_at: record.ended_at, status: "failed".into(), error: record.error.clone() }), Some(name.clone()))); }
            events
        });
        let patch = Some(LanePatch { tip_id: Some(Some(id)), ..LanePatch::default() });
        match record { Some(record) => { let outcome = record.clone(); Ok(OperationCommand::Finish { decision: Box::new(FinishDecision { writes, record, lane: patch, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: outcome.clone() }), events: Some(events) }) }) }, None => Ok(OperationCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(events) }, operation_state: Box::new(next.ok_or_else(|| session_invariant_error("Response settlement is missing its next state"))?), lane: patch }) }
    }).await?;
    Ok(match result { ContinueOperationResult::Result { value } => value, ContinueOperationResult::CancelRequested => ProcedureResult::Continue })
}

fn uuid_v7_timestamp(id: &str) -> Result<i64, SessionError> {
    let digits = format!("{}{}", id.get(..8).unwrap_or_default(), id.get(9..13).unwrap_or_default());
    i64::from_str_radix(&digits, 16).map_err(|_| session_invariant_error(format!("Invalid reserved UUIDv7 {id}")))
}
