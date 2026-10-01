//! Port of senpi `packages/agent/src/harness/runtime/drive/recovery.ts`.

use maho_ai::types::{AssistantMessage, StopReason, Usage};
use maho_ai::utils::assistant_message_frame::reduce_assistant_message_frames;
use crate::harness::events::{HarnessEvent, HarnessEventPayload};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{LaneModelRef, OperationState};
use crate::harness::runtime::progress::read_assistant_frames;
use crate::harness::runtime::types::*;
use super::response::publish_response;

pub fn interrupted_assistant_message(identity: &LaneModelRef, partial: Option<AssistantMessage>, timestamp: i64) -> AssistantMessage {
    let mut message = partial.unwrap_or_else(|| AssistantMessage { content: Vec::new(), api: "unknown".into(), provider: identity.provider.clone(), model: identity.model_id.clone(), usage: Usage::default(), stop_reason: StopReason::Error, timestamp, response_model: None, response_id: None, provider_thinking_level: None, diagnostics: None, stop_details: None, deferred: None, error_message: None, abort_source: None, raw_stop_reason: None, end_turn: None });
    message.usage = Usage::default(); message.stop_reason = StopReason::Error;
    message.error_message = Some("Assistant request was interrupted. The preceding content is the latest committed partial; newer live output may be missing and the external outcome is unknown.".into());
    message
}

async fn recover(lane: &dyn RuntimeDriveLane, drive: &Drive, effect: &OperationState, continuing: bool) -> Result<ProcedureResult, SessionError> {
    let (response_id, identity) = match effect { OperationState::AssistantEffectPending(s) => (&s.response_entry_id, &s.assistant.generation_context.configuration.model), OperationState::DeferredEffectPending(s) => (&s.response_entry_id, &s.deferred.configuration.model), _ => return Err(session_invariant_error("Recovery requires an effect-pending state")) };
    let frames = settle_operation(lane, effect, &drive.context, continuing, |_, _, reader| async move {
        Ok(OperationCommand::Return { result: read_assistant_frames(reader.as_ref(), &drive.operation_id, response_id, &drive.context).await? })
    }).await?;
    let frames = match frames { ContinueOperationResult::CancelRequested => return Ok(ProcedureResult::Continue), ContinueOperationResult::Result { value } => value };
    let partial = reduce_assistant_message_frames(frames.iter()).map_err(|e| session_invariant_error(e.to_string()))?;
    let message = interrupted_assistant_message(identity, partial, super::retry::now_ms());
    let mut start = HarnessEvent::new(HarnessEventPayload::MessageStart { run_id: Some(drive.operation_id.clone()), message: crate::types::AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(message.clone()))) }, Some(lane.name().into()));
    let mut end = HarnessEvent::new(HarnessEventPayload::MessageEnd { run_id: Some(drive.operation_id.clone()), message: crate::types::AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(message.clone()))), entry_id: Some(response_id.clone()) }, Some(lane.name().into()));
    start.recovery = Some(true); end.recovery = Some(true);
    lane.emit(vec![start, end], &drive.context).await;
    publish_response(lane, drive, effect, message, true).await
}

pub async fn recover_assistant_generation(lane: &dyn RuntimeDriveLane, drive: &Drive, generation: &OperationState) -> Result<ProcedureResult, SessionError> { recover(lane, drive, generation, true).await }
pub async fn recover_cancelled_assistant_effect(lane: &dyn RuntimeDriveLane, drive: &Drive, effect: &OperationState) -> Result<ProcedureResult, SessionError> { recover(lane, drive, effect, false).await }
