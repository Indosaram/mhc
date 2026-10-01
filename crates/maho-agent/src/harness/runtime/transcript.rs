//! Port of senpi `packages/agent/src/harness/runtime/transcript.ts`.

use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventPayload, LaneQueuedItem};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{CommitResult, Entry, EntryKind, InboxItem, InboxItemKind, NewEntry, PendingEntry, SessionReader};
use crate::harness::session::values::pending_entry;
use crate::types::AgentMessage;
use super::types::{ContinueOperationResult, Drive, RuntimeLane};
use crate::harness::session::types::{Control, EntryType, OperationState, StorageBranchScan};

pub async fn read_bounded_entries(lane: &dyn RuntimeLane, drive: &Drive, capability: &OperationState) -> Result<ContinueOperationResult<Vec<Entry>>, SessionError> {
    let mutation = lane.session().begin_mutation(&drive.context).await?;
    let result = async {
        let state = lane.state();
        let operation = state.operation.as_ref().ok_or_else(|| session_invariant_error("Lane has no active operation"))?;
        if operation.state != *capability { return Err(session_invariant_error("Operation capability is stale")); }
        if matches!(operation.state.operation_scope_of().control, Control::CancelRequested { .. }) { return Ok(ContinueOperationResult::CancelRequested); }
        let tip = state.tip_id.ok_or_else(|| session_invariant_error("Run operation has no Branch tip"))?;
        let mut query = StorageBranchScan::new(tip);
        query.stop_at_type = Some(EntryType::Compaction);
        let mut entries = mutation.scan_branch(query, &drive.context).await?;
        entries.reverse();
        Ok(ContinueOperationResult::Result { value: entries })
    }.await;
    mutation.end(&drive.context).await;
    result
}

pub async fn read_bounded_context(lane: &dyn RuntimeLane, drive: &Drive, capability: &OperationState, options: &crate::harness::session::context::SessionContextBuildOptions) -> Result<ContinueOperationResult<Vec<AgentMessage>>, SessionError> {
    match read_bounded_entries(lane, drive, capability).await? {
        ContinueOperationResult::CancelRequested => Ok(ContinueOperationResult::CancelRequested),
        ContinueOperationResult::Result { value } => Ok(ContinueOperationResult::Result { value: crate::harness::session::context::build_session_context(&value, Some(options), &drive.context).await }),
    }
}

pub fn chain_entries(mut parent_id: Option<String>, items: &[NewEntry]) -> Vec<NewEntry> {
    items.iter().map(|item| {
        let mut entry = item.clone();
        entry.parent_id = parent_id.take();
        parent_id = Some(entry.id.clone());
        entry
    }).collect()
}

pub fn entry_lifecycle_events(entry: &Entry, lane: &str, run_id: Option<&str>) -> Vec<HarnessEvent> {
    let mut events = Vec::new();
    if let EntryKind::Message { message, .. } = &entry.kind {
        events.push(HarnessEvent::new(HarnessEventPayload::MessageStart { run_id: run_id.map(str::to_owned), message: message.clone() }, Some(lane.to_owned())));
        events.push(HarnessEvent::new(HarnessEventPayload::MessageEnd { run_id: run_id.map(str::to_owned), message: message.clone(), entry_id: Some(entry.id.clone()) }, Some(lane.to_owned())));
    }
    events.push(HarnessEvent::new(HarnessEventPayload::EntryAdded { entry: Box::new(entry.clone()) }, Some(lane.to_owned())));
    events
}

pub fn committed_entry_events(entries: &[NewEntry], commit: &CommitResult, lane: &str, run_id: Option<&str>, first_write_index: usize) -> Result<Vec<HarnessEvent>, SessionError> {
    let mut events = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let seq = *commit.seqs.get(first_write_index + index).ok_or_else(|| session_invariant_error("Committed entry is missing its sequence"))?;
        let entry = Entry { id: entry.id.clone(), parent_id: entry.parent_id.clone(), seq, timestamp: commit.timestamp, kind: entry.kind.clone() };
        events.extend(entry_lifecycle_events(&entry, lane, run_id));
    }
    Ok(events)
}

pub async fn read_lane_queues(reader: &dyn SessionReader, inbox: &[InboxItem], context: &Context) -> Result<Vec<LaneQueuedItem>, SessionError> {
    futures::future::try_join_all(inbox.iter().map(|item| async move {
        let kind = match item.kind { InboxItemKind::Steer => "steer", InboxItemKind::FollowUp => "followUp", InboxItemKind::NextRun => "nextRun", InboxItemKind::Write => "write" };
        let stored = reader.get_value(&pending_entry(&item.entry_id), context).await?.ok_or_else(|| session_invariant_error(format!("Pending {kind} entry {} is missing its payload", item.entry_id)))?;
        let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|e| session_invariant_error(e.to_string()))?;
        match pending {
            PendingEntry::Message { payload } => Ok(LaneQueuedItem { entry_id: item.entry_id.clone(), kind: kind.to_owned(), item_type: "message".to_owned(), message: Some(payload), custom_type: None, data: None }),
            PendingEntry::Custom { custom_type, payload } => {
                if item.kind != InboxItemKind::Write { return Err(session_invariant_error(format!("Pending {kind} entry {} is not a message", item.entry_id))); }
                Ok(LaneQueuedItem { entry_id: item.entry_id.clone(), kind: "write".to_owned(), item_type: "custom".to_owned(), message: None, custom_type: Some(custom_type), data: payload })
            }
        }
    })).await
}

pub async fn read_pending_messages(reader: &dyn SessionReader, ids: &[String], description: &str, context: &Context) -> Result<Vec<(String, AgentMessage)>, SessionError> {
    futures::future::try_join_all(ids.iter().map(|id| async move {
        let missing = || session_invariant_error(format!("{description} {id} is missing its message payload"));
        let stored = reader.get_value(&pending_entry(id), context).await?.ok_or_else(missing)?;
        let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|e| session_invariant_error(e.to_string()))?;
        match pending { PendingEntry::Message { payload } => Ok((id.clone(), payload)), PendingEntry::Custom { .. } => Err(missing()) }
    })).await
}
