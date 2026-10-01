//! Port of senpi `packages/agent/src/harness/runtime/drive/checkpoint.ts`.

use std::sync::Arc;
use crate::harness::hooks::{HookInvocation, HookName, HookResult};
use crate::harness::events::{HarnessEvent, HarnessEventPayload};
use crate::harness::session::commit::insert_entry;
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::session::values::{branch_tip, operation_preparation, set_value};
use crate::harness::runtime::types::*;
use crate::harness::runtime::transcript::{chain_entries, committed_entry_events};
use super::boundary::*;

pub async fn start_run(lane: &dyn RuntimeDriveLane, drive: &Drive, run: &OperationState) -> Result<ProcedureResult, SessionError> {
    let prompt = settle_operation(lane, run, &drive.context, true, |_, op, reader| async move {
        let OperationIntent::Run { prompt_entry_ids } = op.meta.intent else { return Err(session_invariant_error("Run operation has non-run intent")); };
        let entries = reader.get_entries(prompt_entry_ids.clone(), &drive.context).await?;
        let messages = prompt_entry_ids.iter().map(|id| entries.get(id).and_then(|entry| entry.kind.message()).cloned().ok_or_else(|| session_invariant_error(format!("Run prompt entry {id} is missing its message")))).collect::<Result<Vec<_>, _>>()?;
        Ok(OperationCommand::Return { result: messages })
    }).await?;
    let messages = match prompt { ContinueOperationResult::CancelRequested => return Ok(ProcedureResult::Continue), ContinueOperationResult::Result { value } => value };
    let mut invocation = HookInvocation::new(lane.name(), &drive.operation_id);
    invocation.prompt = messages; invocation.resources = lane.config().resources.clone();
    let hook = lane.hooks().run_with_gate(HookName::BeforeRun, invocation, &drive.gate, &drive.context).await.map_err(|e| session_invariant_error(e.to_string()))?;
    let injected = match hook { HookResult::BeforeRun { messages } => messages, _ => Vec::new() };
    let mut reserved = Vec::new();
    for message in injected {
        if matches!(message.try_as_llm(), Some(maho_ai::types::Message::Assistant(s)) if s.stop_reason == maho_ai::types::StopReason::Pending) { return Err(session_invariant_error("before_run returned a pending assistant message")); }
        reserved.push(NewEntry::message((lane.session().id_generator())(None), None, message));
    }
    let result = settle_operation(lane, run, &drive.context, true, |state, op, _| async move {
        let entries = chain_entries(state.tip_id.clone(), &reserved);
        let trigger = entries.last().map(|e| e.id.clone()).or(state.tip_id).ok_or_else(|| session_invariant_error("Run start has no trigger entry"))?;
        let next = OperationState::Checkpoint(CheckpointOperation { operation: op.state.operation_scope_of(), at: OperationMarker::Checkpoint, checkpoint: CheckpointData { continuation: Continuation::NeedAssistant { overflow_recovery_used: false }, trigger_entry_id: trigger.clone() } });
        let mut writes: Vec<_> = entries.iter().cloned().map(insert_entry).collect();
        if !entries.is_empty() { writes.push(Write::Value(set_value(&branch_tip(lane.name()), serde_json::json!(trigger)))); }
        let (name, id) = (lane.name().to_owned(), drive.operation_id.clone());
        Ok(OperationCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(Arc::new(move |commit| committed_entry_events(&entries, commit, &name, Some(&id), 0).unwrap_or_default())) }, operation_state: Box::new(next), lane: Some(LanePatch { tip_id: Some(Some(trigger)), ..LanePatch::default() }) })
    }).await?;
    Ok(match result { ContinueOperationResult::CancelRequested => ProcedureResult::Continue, ContinueOperationResult::Result { value } => value })
}

enum CheckpointPlan { Continue, Finish(Vec<String>) }

pub async fn run_checkpoint(lane: &dyn RuntimeDriveLane, drive: &Drive, run: &OperationState) -> Result<ProcedureResult, SessionError> {
    let threshold = match lane.prepare_compaction_threshold(drive, run).await? { ContinueOperationResult::CancelRequested => return Ok(ProcedureResult::Continue), ContinueOperationResult::Result { value } => value };
    let planned = settle_operation(lane, run, &drive.context, true, |state, op, reader| async move {
        let OperationState::Checkpoint(current) = &op.state else { return Err(session_invariant_error("Checkpoint procedure requires checkpoint state")); };
        let placement = plan_boundary_inbox(lane, drive, &state, &current.operation, reader.as_ref(), state.tip_id.clone(), threshold.is_none() && matches!(current.checkpoint.continuation, Continuation::MayFinish { .. })).await?;
        let (next, extra, threshold_started) = if let Some(trigger) = &placement.trigger_entry_id {
            (assistant_ready_at_boundary(lane, &state, current.operation.clone(), trigger.clone(), false), Vec::new(), false)
        } else if let Some(threshold) = threshold {
            let next = OperationState::SummaryDeciding(SummaryDecidingOperation { operation: current.operation.clone(), at: OperationMarker::SummaryDeciding, task: SummaryTask { task_id: threshold.task_id.clone(), reason: Some(SummaryTaskReason::Threshold), custom_instructions: None, boundary: ResultBoundary::ResumeCheckpoint { resume_after: current.checkpoint.clone() } } });
            (next, vec![Write::Value(set_value(&operation_preparation(&drive.operation_id, &threshold.task_id), threshold.preparation))], true)
        } else { match current.checkpoint.continuation {
            Continuation::NeedAssistant { overflow_recovery_used } => (assistant_ready_at_boundary(lane, &state, current.operation.clone(), current.checkpoint.trigger_entry_id.clone(), overflow_recovery_used), Vec::new(), false),
            Continuation::MayFinish { .. } => return Ok(OperationCommand::Return { result: CheckpointPlan::Finish(placement.entries.iter().map(|e| e.id.clone()).collect()) }),
        }};
        let mut writes = placement.writes.clone(); writes.extend(extra);
        let patch = LanePatch { tip_id: Some(placement.tip_id.clone()), inbox: Some(placement.inbox.clone()), configuration: None };
        let (name, id) = (lane.name().to_owned(), drive.operation_id.clone());
        Ok(OperationCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(|_| CheckpointPlan::Continue), events: Some(Arc::new(move |commit| { let mut events = boundary_placement_events(&placement, commit, 0, &name, &id); if threshold_started { events.push(HarnessEvent::new(HarnessEventPayload::CompactionStart { run_id: id.clone(), reason: "threshold".to_owned(), started_at: commit.timestamp }, Some(name.clone()))); } events })) }, operation_state: Box::new(next), lane: Some(patch) })
    }).await?;
    match planned {
        ContinueOperationResult::CancelRequested | ContinueOperationResult::Result { value: CheckpointPlan::Continue } => Ok(ProcedureResult::Continue),
        ContinueOperationResult::Result { value: CheckpointPlan::Finish(ids) } => {
            let OperationState::Checkpoint(checkpoint) = run else { return Err(session_invariant_error("Checkpoint finish mediation requires a finish continuation")); };
            let Continuation::MayFinish { include_final_assistant } = checkpoint.checkpoint.continuation else { return Err(session_invariant_error("Checkpoint finish mediation requires a finish continuation")); };
            finish_run_boundary(lane, drive, run, include_final_assistant, &ids, Vec::new()).await
        }
    }
}
