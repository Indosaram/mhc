//! Port of senpi `packages/agent/src/harness/runtime/drive/tool-placement.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;
use maho_ai::types::{AssistantMessage, ContentBlock, Message, ToolResultMessage};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::*;
use crate::harness::session::commit::{insert_entry, insert_usage};
use crate::harness::session::values::*;
use crate::harness::runtime::types::*;
use crate::harness::events::{HarnessEvent, HarnessEventPayload};

pub struct ToolBatchSource { pub assistant: AssistantMessage, pub calls: BTreeMap<usize, crate::types::AgentToolCall> }

pub async fn read_tool_batch_source(lane: &dyn RuntimeLane, drive: &Drive, batch: &ToolBatch) -> Result<ToolBatchSource, SessionError> {
    let mutation = lane.session().begin_mutation(&drive.context).await?;
    let result = async {
        let entries = mutation.get_entries(vec![batch.assistant_entry_id.clone()], &drive.context).await?;
        let Some(Message::Assistant(assistant)) = entries.get(&batch.assistant_entry_id).and_then(|e| e.kind.message()).and_then(|m| m.try_as_llm()) else { return Err(session_invariant_error("Tool batch assistant entry is invalid")); };
        let mut calls = BTreeMap::new();
        for call in &batch.calls {
            let Some(ContentBlock::ToolCall(block)) = assistant.content.get(call.source_index()) else { return Err(session_invariant_error(format!("Tool call source index {} does not name a tool-call block", call.source_index()))); };
            calls.insert(call.source_index(), block.clone());
        }
        Ok(ToolBatchSource { assistant: assistant.as_ref().clone(), calls })
    }.await;
    mutation.end(&drive.context).await; result
}

pub fn tool_call_for<'a>(sources: &'a ToolBatchSource, call: &ToolCall) -> Result<&'a crate::types::AgentToolCall, SessionError> {
    sources.calls.get(&call.source_index()).ok_or_else(|| session_invariant_error(format!("Tool call source index {} is invalid", call.source_index())))
}

pub fn with_tool_batch(run: &ToolsOperation, batch: ToolBatch) -> ToolsOperation { ToolsOperation { operation: run.operation.clone(), at: OperationMarker::Tools, batch } }

pub async fn materialize_ready(lane: &dyn RuntimeLane, drive: &Drive, capability: &OperationState, sources: &ToolBatchSource, recovery: bool) -> Result<(), SessionError> {
    let OperationState::Tools(run) = capability else { return Err(session_invariant_error("Tool placement requires tools state")); };
    let Some(first) = run.batch.calls.iter().position(|call| !matches!(call, ToolCall::Completed { .. })) else { return Ok(()); };
    let ready: Vec<_> = run.batch.calls.iter().skip(first).take_while(|call| matches!(call, ToolCall::OutcomeReady { .. })).cloned().collect();
    if ready.is_empty() { return Ok(()); }
    let mutation = lane.session().begin_mutation(&drive.context).await?;
    let read = async {
        let mut items = Vec::new();
        for call in &ready {
            let stored = mutation.get_value(&pending_entry(call.result_entry_id()), &drive.context).await?.ok_or_else(|| session_invariant_error(format!("Tool call {} is missing its staged result", call.result_entry_id())))?;
            let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|e| session_invariant_error(e.to_string()))?;
            let PendingEntry::Message { payload } = pending else { return Err(session_invariant_error(format!("Tool call {} is missing its staged result", call.result_entry_id()))); };
            let Some(Message::ToolResult(message)) = payload.try_as_llm() else { return Err(session_invariant_error(format!("Tool call {} is missing its staged result", call.result_entry_id()))); };
            let source = tool_call_for(sources, call)?;
            if message.tool_call_id != source.id || message.tool_name != source.name { return Err(session_invariant_error(format!("Tool call {} has a mismatched staged result", call.result_entry_id()))); }
            items.push((call.clone(), message.clone()));
        }
        Ok(items)
    }.await;
    mutation.end(&drive.context).await;
    let items: Vec<(ToolCall, ToolResultMessage)> = read?;
    let mut events = Vec::new();
    for (call, message) in &items {
        let message = crate::types::AgentMessage::from(Message::ToolResult(message.clone()));
        let mut start = HarnessEvent::new(HarnessEventPayload::MessageStart { run_id: Some(drive.operation_id.clone()), message: message.clone() }, Some(lane.name().into()));
        let mut end = HarnessEvent::new(HarnessEventPayload::MessageEnd { run_id: Some(drive.operation_id.clone()), message, entry_id: Some(call.result_entry_id().to_owned()) }, Some(lane.name().into()));
        start.recovery = recovery.then_some(true); end.recovery = recovery.then_some(true); events.extend([start, end]);
    }
    lane.emit(events, &drive.context).await;
    let mut turn_results = Vec::new();
    if first + items.len() == run.batch.calls.len() {
        let reader = lane.session().begin_mutation(&drive.context).await?;
        let placed = reader.get_entries(run.batch.calls.iter().filter(|call| matches!(call, ToolCall::Completed { .. })).map(|call| call.result_entry_id().to_owned()).collect(), &drive.context).await;
        reader.end(&drive.context).await;
        let placed = placed?;
        for call in &run.batch.calls {
            let message = items.iter().find(|(candidate,_)| candidate.result_entry_id() == call.result_entry_id()).map(|(_,m)|m.clone()).or_else(|| placed.get(call.result_entry_id()).and_then(|entry| entry.kind.message()).and_then(|m| match m.try_as_llm() { Some(Message::ToolResult(m)) => Some(m.clone()), _ => None })).ok_or_else(|| session_invariant_error(format!("Completed tool call {} is missing its result entry",call.result_entry_id())))?;
            turn_results.push(message);
        }
    }
    let committed = settle_operation(lane, capability, &drive.context, false, |state, op, reader| async move {
        let OperationState::Tools(mut current) = op.state else { return Err(session_invariant_error("Tool placement state changed")); };
        let mut writes = Vec::new(); let mut entries = Vec::new(); let mut usage_rows = Vec::new(); let mut parent = state.tip_id;
        let previous_configuration = state.configuration.clone();
        let mut next_configuration = state.configuration;
        for (call, message) in &items {
            let terminate = matches!(call, ToolCall::OutcomeReady { terminate: true, .. });
            let entry = NewEntry { id: call.result_entry_id().to_owned(), parent_id: parent, kind: EntryKind::Message { message: crate::types::AgentMessage::from(Message::ToolResult(message.clone())), terminate: terminate.then_some(true) } };
            entries.push((entry.clone(), writes.len())); writes.push(insert_entry(entry)); writes.push(Write::Value(delete_value(&pending_entry(call.result_entry_id()))));
            if let Some(usage) = &message.usage { let row = NewUsageRow { id: (lane.session().id_generator())(None), usage: *usage, entry_id: Some(call.result_entry_id().into()), adjustment: false, details: None }; usage_rows.push((row.clone(),writes.len())); writes.push(insert_usage(row)); }
            for name in message.added_tool_names.iter().flatten() { if !next_configuration.active_tool_names.contains(name) { next_configuration.active_tool_names.push(name.clone()); } }
            parent = Some(call.result_entry_id().into());
            for candidate in &mut current.batch.calls { if candidate.source_index() == call.source_index() && candidate.result_entry_id() == call.result_entry_id() { *candidate = ToolCall::Completed { source_index: call.source_index(), result_entry_id: call.result_entry_id().into(), terminate }; } }
        }
        let config_changed = next_configuration != previous_configuration;
        if config_changed { writes.push(Write::Value(set_value(&lane_config(lane.name()),serde_json::to_value(&next_configuration).map_err(|e|session_invariant_error(e.to_string()))?))); }
        let complete = current.batch.calls.iter().all(|call| matches!(call, ToolCall::Completed { .. }));
        let next = if complete {
            let all_terminate = current.batch.calls.iter().all(|call| matches!(call, ToolCall::Completed { terminate: true, .. }));
            let args = reader.scan_values(&operation_tool_args_prefix(&drive.operation_id, Some(&current.batch.turn_id)), &drive.context).await?;
            writes.extend(args.iter().map(|arg| Write::Value(delete_value(&arg.address))));
            OperationState::Checkpoint(CheckpointOperation { operation: current.operation, at: OperationMarker::Checkpoint, checkpoint: CheckpointData { continuation: if all_terminate { Continuation::MayFinish { include_final_assistant: false } } else { Continuation::NeedAssistant { overflow_recovery_used: false } }, trigger_entry_id: parent.clone().ok_or_else(|| session_invariant_error("Completed tools have no tip"))? } })
        } else { OperationState::Tools(current) };
        writes.push(Write::Value(set_value(&branch_tip(lane.name()), serde_json::json!(parent))));
        let name = lane.name().to_owned();
        let published_configuration = next_configuration.clone();
        Ok(OperationCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(move |_| complete), events: Some(Arc::new(move |commit| {
            let mut events=Vec::new();
            for (entry,index) in &entries {
                events.push(HarnessEvent::new(HarnessEventPayload::EntryAdded { entry: Box::new(Entry { id: entry.id.clone(), parent_id: entry.parent_id.clone(), kind: entry.kind.clone(), seq: commit.seqs[*index], timestamp: commit.timestamp }) }, Some(name.clone())));
                if let Some((row,seq_index))=usage_rows.iter().find(|(row,_)|row.entry_id.as_ref()==Some(&entry.id)) { events.push(HarnessEvent::new(HarnessEventPayload::Usage {lane:name.clone(),row:UsageRow {id:row.id.clone(),usage:row.usage,entry_id:row.entry_id.clone(),adjustment:false,details:None,seq:commit.seqs[*seq_index]},totals:commit.stats.usage},Some(name.clone()))); }
            }
            if config_changed { events.push(HarnessEvent::new(HarnessEventPayload::ConfigUpdate {property:"activeTools".into(),value:serde_json::json!(published_configuration.active_tool_names),previous:serde_json::json!(previous_configuration.active_tool_names)},Some(name.clone()))); }
            events
        })) }, operation_state: Box::new(next), lane: Some(LanePatch { tip_id: Some(parent), configuration:Some(next_configuration), inbox:None }) })
    }).await?;
    if matches!(committed, ContinueOperationResult::Result { value: true }) {
        lane.emit(vec![HarnessEvent::new(HarnessEventPayload::TurnEnd { run_id: drive.operation_id.clone(), turn_id: run.batch.turn_id.clone(), message: Box::new(sources.assistant.clone()), tool_results: turn_results }, Some(lane.name().into()))], &drive.context).await;
    }
    Ok(())
}
