//! Port of senpi `packages/agent/src/harness/runtime/drive/deferred.ts`.

use std::sync::Arc;
use maho_ai::types::{DeferredHandle, Message, StopReason};
use crate::harness::events::{HarnessEvent, HarnessEventPayload};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::session::values::{delete_list, pending_assistant_frames};
use crate::harness::context::Context;
use crate::harness::runtime::types::*;
use super::response::{publish_configuration_failure, publish_response};

pub async fn read_deferred_source_handle(reader: &dyn SessionReader, deferred: &DeferredScope, context: &Context) -> Result<DeferredHandle, SessionError> {
    let entries = reader.get_entries(vec![deferred.source_entry_id.clone()], context).await?;
    let missing = || session_invariant_error(format!("Deferred source {} is missing its assistant handle", deferred.source_entry_id));
    let entry = entries.get(&deferred.source_entry_id).ok_or_else(missing)?;
    let Some(Message::Assistant(message)) = entry.kind.message().and_then(|m| m.try_as_llm()) else { return Err(missing()); };
    if message.stop_reason != StopReason::Deferred { return Err(missing()); }
    let handle = message.deferred.clone().ok_or_else(missing)?;
    let identity = &deferred.configuration.model;
    if handle.id.is_empty() || handle.provider != identity.provider || handle.model_id != identity.model_id || handle.api != message.api { return Err(session_invariant_error(format!("Deferred source {} has an invalid handle", deferred.source_entry_id))); }
    Ok(handle)
}

pub async fn run_deferred(lane: &dyn RuntimeDriveLane, drive: &Drive, expected: &OperationState) -> Result<ProcedureResult, SessionError> {
    let (scope, recovery) = match expected { OperationState::DeferredSuspended(s) => (&s.deferred, false), OperationState::DeferredEffectPending(s) => (&s.deferred, true), _ => return Err(session_invariant_error("Deferred procedure requires deferred state")) };
    let source = settle_operation(lane, expected, &drive.context, true, |_, _, reader| async move { Ok(OperationCommand::Return { result: read_deferred_source_handle(reader.as_ref(), scope, &drive.context).await? }) }).await?;
    let source = match source { ContinueOperationResult::CancelRequested => return Ok(ProcedureResult::Continue), ContinueOperationResult::Result { value } => value };
    if *drive.deferred_permits.lock().unwrap_or_else(|e| e.into_inner()) == 0 { return Ok(ProcedureResult::Waiting { outcome: crate::harness::agent_harness::DriveOutcome::WaitingDeferred { operation_id: drive.operation_id.clone(), deferred: source } }); }
    let Some(model) = lane.models().get_model(&scope.configuration.model.provider, &scope.configuration.model.model_id) else { return publish_configuration_failure(lane, drive, expected, OperationError { code: "model_unavailable".into(), message: "The configured model is unavailable in this process".into(), details: Some(serde_json::json!(scope.configuration.model)) }).await; };
    let poll = if recovery { scope.poll } else { scope.poll.saturating_add(1) };
    let mut invocation = crate::harness::hooks::HookInvocation::new(lane.name(), &drive.operation_id);
    invocation.model = Some(model.clone()); invocation.step = Some("deferred".into()); invocation.attempt = Some(u32::try_from(poll).map_err(|e| session_invariant_error(e.to_string()))?);
    let mut base_options = scope.stream_options.clone(); base_options.deferred = Some(maho_ai::types::DeferredOption::Enabled(false)); invocation.stream_options = base_options.clone();
    let mut stream_options = match lane.hooks().run_with_gate(crate::harness::hooks::HookName::BeforeRequest, invocation, &drive.gate, &drive.context).await.map_err(|e| session_invariant_error(e.to_string()))? { crate::harness::hooks::HookResult::BeforeRequest { stream_options } => crate::harness::hooks::apply_stream_options_patch(&base_options, &stream_options), _ => base_options };
    stream_options.deferred = Some(maho_ai::types::DeferredOption::Enabled(false));
    let at = super::retry::now_ms();
    let mut next_scope = scope.clone(); next_scope.poll = poll;
    let pending = OperationState::DeferredEffectPending(DeferredEffectPendingOperation { deferred: next_scope, response_entry_id: (lane.session().id_generator())(Some(at)), usage_id: (lane.session().id_generator())(Some(at)), at: OperationMarker::DeferredEffectPending });
    let intent = pending.clone();
    let committed = settle_operation(lane, expected, &drive.context, true, |_, _, _| async move {
        let writes = match expected { OperationState::DeferredEffectPending(s) => vec![Write::List(delete_list(&pending_assistant_frames(&drive.operation_id, &s.response_entry_id)))], _ => Vec::new() };
        let mut resume = HarnessEvent::new(HarnessEventPayload::RunResume { run_id: drive.operation_id.clone() }, Some(lane.name().into())); resume.recovery = recovery.then_some(true);
        let mut start = HarnessEvent::new(HarnessEventPayload::TurnStart { run_id: drive.operation_id.clone(), turn_id: format!("{}:poll:{poll}", scope.step_id) }, Some(lane.name().into())); start.recovery = recovery.then_some(true);
        Ok(OperationCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(|_| ()), events: Some(Arc::new(move |_| vec![resume.clone(), start.clone()])) }, operation_state: Box::new(pending), lane: None })
    }).await?;
    if matches!(committed, ContinueOperationResult::CancelRequested) { return Ok(ProcedureResult::Continue); }
    *drive.deferred_permits.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
    let mut options = maho_ai::types::DeferredFetchOptions::default();
    options.request.signal = Some(drive.gate.signal());
    options.wait = Some(0);
    options.request.timeout_ms = stream_options.timeout_ms;
    options.request.max_retries = stream_options.max_retries;
    options.request.max_retry_delay_ms = stream_options.max_retry_delay_ms;
    options.request.headers = stream_options.headers.as_ref().map(|headers| headers.iter().map(|(key,value)| (key.clone(), value.as_str().map(str::to_owned))).collect());
    let stream = drive.gate.admit(|| lane.models().stream_deferred(&model, &source, Some(options), maho_ai::models::ModelsRequestTransforms::default())).map_err(|e| session_invariant_error(e.to_string()))?;
    let response = super::response::consume_response(lane, drive, &intent, &stream, recovery).await?;
    publish_response(lane, drive, &intent, response, recovery).await
}
