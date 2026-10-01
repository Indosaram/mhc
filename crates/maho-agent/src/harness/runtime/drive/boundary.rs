//! Port of senpi `packages/agent/src/harness/runtime/drive/boundary.ts`.

use std::sync::Arc;
use crate::harness::events::{HarnessEvent, HarnessEventPayload, LaneQueuedItem, RunEndPayload};
use crate::harness::session::commit::insert_entry;
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::session::values::{branch_tip, delete_value, pending_entry, set_value};
use crate::harness::runtime::types::*;
use crate::harness::runtime::types::LaneState;
use crate::harness::runtime::transcript::{committed_entry_events, read_lane_queues, read_bounded_context};
use crate::harness::hooks::{HookInvocation, HookName, HookResult};
use super::terminal::{operation_cleanup_writes, operation_result_record};

pub struct BoundaryFinishPending { pub entry_ids: Vec<String> }
#[derive(Clone)]
pub struct BoundaryPlacement {
    pub entries: Vec<NewEntry>, pub writes: Vec<Write>, pub tip_id: Option<String>, pub inbox: Vec<InboxItem>, pub trigger_entry_id: Option<String>, pub queues: Option<Vec<LaneQueuedItem>>,
}

pub fn normalized_retry_policy(lane: &dyn RuntimeDriveLane) -> NormalizedRetryPolicy {
    let config = lane.config();
    let retry = &config.retry_policy;
    NormalizedRetryPolicy { max_attempts: if retry.enabled { retry.max_retries.saturating_add(1) } else { 1 }, base_delay_ms: retry.base_delay_ms, max_agent_delay_ms: retry.max_agent_delay_ms.unwrap_or(maho_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS) }
}

pub fn assistant_ready_at_boundary(lane: &dyn RuntimeDriveLane, state: &LaneState, scope: OperationScope, trigger_entry_id: String, overflow_recovery_used: bool) -> OperationState {
    let config = lane.config();
    OperationState::AssistantReady(AssistantReadyOperation { operation: scope, at: OperationMarker::AssistantReady, assistant: AssistantGenerationScope { generation_context: GenerationContext { step_id: (lane.session().id_generator())(None), trigger_entry_id, configuration: state.configuration.clone(), stream_options: config.stream_options.clone(), retry_policy: normalized_retry_policy(lane), overflow_recovery_used } }, next_attempt: 1 })
}

pub async fn plan_boundary_inbox(lane: &dyn RuntimeDriveLane, drive: &Drive, state: &LaneState, scope: &OperationScope, reader: &dyn SessionReader, mut tip_id: Option<String>, follow_up_when_no_trigger: bool) -> Result<BoundaryPlacement, SessionError> {
    let mut steer = state.inbox.iter().filter(|i| i.kind == InboxItemKind::Steer);
    let selected_steer: Vec<_> = match scope.settings.steering_mode { crate::types::QueueMode::All => steer.collect(), crate::types::QueueMode::OneAtATime => steer.next().into_iter().collect() };
    let mut selected: Vec<_> = state.inbox.iter().filter(|item| item.kind == InboxItemKind::Write || selected_steer.iter().any(|s| s.entry_id == item.entry_id)).cloned().collect();
    let load = |items: Vec<InboxItem>| async move {
        let mut loaded = Vec::new();
        for item in items {
            let kind = match item.kind { InboxItemKind::Steer => "steer", InboxItemKind::FollowUp => "followUp", InboxItemKind::NextRun => "nextRun", InboxItemKind::Write => "write" };
            let stored = reader.get_value(&pending_entry(&item.entry_id), &drive.context).await?.ok_or_else(|| session_invariant_error(format!("Pending {kind} entry {} is missing its payload", item.entry_id)))?;
            let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|e| session_invariant_error(e.to_string()))?;
            if item.kind != InboxItemKind::Write && !matches!(pending, PendingEntry::Message { .. }) { return Err(session_invariant_error(format!("Queued {kind} entry {} is not a message", item.entry_id))); }
            loaded.push((item, pending));
        }
        Ok::<_, SessionError>(loaded)
    };
    let mut pending = load(selected.clone()).await?;
    let config = lane.config();
    let projects = |value: &PendingEntry| match value { PendingEntry::Message { .. } => true, PendingEntry::Custom { custom_type, .. } => config.entry_projectors.contains_key(custom_type) };
    if follow_up_when_no_trigger && !pending.iter().any(|(_, value)| projects(value)) {
        let mut follow = state.inbox.iter().filter(|i| i.kind == InboxItemKind::FollowUp);
        let taken: Vec<_> = match scope.settings.follow_up_mode { crate::types::QueueMode::All => follow.collect(), crate::types::QueueMode::OneAtATime => follow.next().into_iter().collect() };
        selected.extend(taken.into_iter().cloned());
        selected.sort_by_key(|item| state.inbox.iter().position(|i| i.entry_id == item.entry_id));
        pending = load(selected.clone()).await?;
    }
    let mut entries = Vec::new();
    let mut trigger_entry_id = None;
    for (item, value) in pending {
        if projects(&value) { trigger_entry_id = Some(item.entry_id.clone()); }
        let kind = match value { PendingEntry::Message { payload } => EntryKind::Message { message: payload, terminate: None }, PendingEntry::Custom { custom_type, payload } => EntryKind::Custom { custom_type, data: payload } };
        entries.push(NewEntry { id: item.entry_id.clone(), parent_id: tip_id, kind });
        tip_id = Some(item.entry_id);
    }
    let inbox: Vec<_> = state.inbox.iter().filter(|i| !selected.iter().any(|s| s.entry_id == i.entry_id)).cloned().collect();
    let queues = if selected.is_empty() { None } else { Some(read_lane_queues(reader, &inbox, &drive.context).await?) };
    let mut writes: Vec<_> = entries.iter().cloned().map(insert_entry).collect();
    writes.extend(selected.iter().map(|i| Write::Value(delete_value(&pending_entry(&i.entry_id)))));
    if !entries.is_empty() { writes.push(Write::Value(set_value(&branch_tip(lane.name()), serde_json::json!(tip_id)))); }
    Ok(BoundaryPlacement { entries, writes, tip_id, inbox, trigger_entry_id, queues })
}

pub fn boundary_placement_events(placement: &BoundaryPlacement, commit: &CommitResult, first_write_index: usize, lane: &str, run_id: &str) -> Vec<HarnessEvent> {
    let mut events = committed_entry_events(&placement.entries, commit, lane, Some(run_id), first_write_index).unwrap_or_default();
    if let Some(queues) = &placement.queues { events.push(HarnessEvent::queue_update(queues.clone(), lane)); }
    events
}

pub async fn finish_run_boundary(lane: &dyn RuntimeDriveLane, drive: &Drive, capability: &OperationState, include_final_assistant: bool, planned_entry_ids: &[String], pending_events: Vec<HarnessEvent>) -> Result<ProcedureResult, SessionError> {
    let options = crate::harness::session::context::SessionContextBuildOptions { entry_projectors: lane.config().entry_projectors.clone() };
    let messages = match read_bounded_context(lane, drive, capability, &options).await? { ContinueOperationResult::CancelRequested => return Ok(ProcedureResult::Continue), ContinueOperationResult::Result { value } => value };
    let mut invocation = HookInvocation::new(lane.name(), &drive.operation_id);
    invocation.messages = messages;
    let hook = lane.hooks().run_with_gate(HookName::BeforeRunEnd, invocation, &drive.gate, &drive.context).await.map_err(|e| session_invariant_error(e.to_string()))?;
    let follow_up = match hook { HookResult::BeforeRunEnd { follow_up } => Some(((lane.session().id_generator())(None), follow_up)), _ => None };
    let result = settle_operation(lane, capability, &drive.context, true, |state, op, reader| async move {
        let mut placement = plan_boundary_inbox(lane, drive, &state, &op.state.operation_scope_of(), reader.as_ref(), state.tip_id.clone(), true).await?;
        let plan_current = placement.entries.iter().map(|e| &e.id).eq(planned_entry_ids.iter());
        if placement.trigger_entry_id.is_none() && plan_current
            && let Some((id, text)) = follow_up {
                let message = maho_ai::types::UserMessage { content: maho_ai::types::UserContent::Text(text), timestamp: super::retry::now_ms() };
                let entry = NewEntry::message(&id, placement.tip_id.clone(), crate::types::AgentMessage::from(maho_ai::types::Message::User(message)));
                placement.writes.push(insert_entry(entry.clone()));
                placement.entries.push(entry);
                placement.tip_id = Some(id.clone());
                placement.trigger_entry_id = Some(id);
                placement.writes.push(Write::Value(set_value(&branch_tip(lane.name()), serde_json::json!(placement.tip_id))));
        }
        let patch = Some(LanePatch { tip_id: Some(placement.tip_id.clone()), inbox: Some(placement.inbox.clone()), configuration: None });
        if let Some(trigger) = &placement.trigger_entry_id {
            let next = assistant_ready_at_boundary(lane, &state, op.state.operation_scope_of(), trigger.clone(), false);
            let name = lane.name().to_owned(); let id = drive.operation_id.clone();
            return Ok(OperationCommand::Commit { decision: CommitDecision { writes: placement.writes.clone(), materialize: Arc::new(|_| ProcedureResult::Continue), events: Some(Arc::new(move |commit| { let mut events = pending_events.clone(); events.extend(boundary_placement_events(&placement, commit, 0, &name, &id)); events })) }, operation_state: Box::new(next), lane: patch });
        }
        if placement.tip_id.is_none() { return Err(session_invariant_error("Completed run has no tip")); }
        if include_final_assistant && op.state.operation_scope_of().latest_assistant_entry_id.is_none() { return Err(session_invariant_error("Completed run is missing its final assistant")); }
        let record = operation_result_record(&op.meta, TerminalStatus::Completed, placement.tip_id.clone(), None)?;
        let cleanup = operation_cleanup_writes(reader.as_ref(), &drive.operation_id, &op.state, &drive.context).await?;
        let mut writes = placement.writes.clone(); writes.extend(cleanup);
        let (name, id, result) = (lane.name().to_owned(), drive.operation_id.clone(), record.clone());
        let end = HarnessEvent::new(HarnessEventPayload::RunEnd(RunEndPayload { run_id: id.clone(), from_tip_id: op.meta.source_tip_id, tip_id: placement.tip_id.clone(), ended_at: record.ended_at, status: "completed".to_owned(), error: None }), Some(name.clone()));
        Ok(OperationCommand::Finish { decision: Box::new(FinishDecision { writes, record, lane: patch, materialize: Arc::new(move |_| ProcedureResult::Settled { outcome: result.clone() }), events: Some(Arc::new(move |commit| { let mut events = pending_events.clone(); events.extend(boundary_placement_events(&placement, commit, 0, &name, &id)); events.push(end.clone()); events })) }) })
    }).await?;
    Ok(match result { ContinueOperationResult::CancelRequested => ProcedureResult::Continue, ContinueOperationResult::Result { value } => value })
}
