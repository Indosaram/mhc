//! Port of senpi `packages/agent/src/harness/runtime/transcript.ts`.

use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventPayload, LaneQueuedItem};
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{CommitResult, Entry, EntryKind, InboxItem, InboxItemKind, NewEntry, PendingEntry, SessionReader};
use crate::harness::session::values::pending_entry;
use crate::types::AgentMessage;
use super::types::{ContinueOperationResult, Drive, RuntimeLane};
use crate::harness::session::types::{Control, EntryType, OperationState, StorageBranchScan};

pub async fn watch_lane(lane: std::sync::Arc<dyn RuntimeLane>, events: crate::harness::events::HarnessEventBus, context: &Context, faulted: bool) -> Result<std::sync::Arc<crate::harness::events::BufferedEventWatcher<Result<serde_json::Value, SessionError>>>, SessionError> {
    let mutation=lane.session().begin_mutation(context).await?;
    let name=lane.name().to_owned();
    let capture_lane=lane.clone();
    let capture=std::sync::Arc::new(move |context:Context| {
        let lane=capture_lane.clone();
        Box::pin(async move {
            let mutation=lane.session().begin_mutation(&context).await?;
            let result=capture_lane_snapshot(lane.as_ref(),mutation.as_ref(),&context,faulted).await;
            mutation.end(&context).await;
            result
        }) as maho_ai::types::BoxFuture<'static,Result<serde_json::Value,SessionError>>
    });
    let resnapshot=std::sync::Arc::new(move |context:Context,mark_boundary:std::sync::Arc<dyn Fn()+Send+Sync>| {
        let capture=capture.clone();
        Box::pin(async move {let snapshot=capture(context).await;mark_boundary();snapshot}) as maho_ai::types::BoxFuture<'static,Result<serde_json::Value,SessionError>>
    });
    let watcher=events.watch(Ok(serde_json::Value::Null),std::sync::Arc::new(move |event|event.event_type()=="usage"||event.lane.is_none()||event.lane.as_deref()==Some(&name)),Some(resnapshot));
    let result=capture_lane_snapshot(lane.as_ref(),mutation.as_ref(),context,faulted).await;
    mutation.end(context).await;
    match result {Ok(snapshot)=>{watcher.set_snapshot(Ok(snapshot));Ok(watcher)},Err(error)=>{watcher.unsubscribe();Err(error)}}
}

pub async fn capture_lane_snapshot(lane: &dyn RuntimeLane, reader: &dyn SessionReader, context: &Context, faulted: bool) -> Result<serde_json::Value, SessionError> {
    use serde_json::{json, Value};
    use maho_ai::types::{ContentBlock, Message};
    use crate::harness::session::types::ToolCall;
    use crate::harness::session::values::{operation_result, operation_tool_args, pending_tool_output};
    let captured = lane.state();
    let transcript = match &captured.tip_id {
        None => Vec::new(),
        Some(tip) => { let mut query=StorageBranchScan::new(tip.clone());query.stop_at_type=Some(EntryType::Compaction);let mut entries=reader.scan_branch(query,context).await?;entries.reverse();entries },
    };
    let queues=read_lane_queues(reader,&captured.inbox,context).await?;
    let mut snapshot=json!({"lane":lane.name(),"transcript":transcript,"tipId":captured.tip_id,"configuration":captured.configuration,"stats":reader.get_stats(context).await?,"operation":null,"queues":queues.iter().map(|q|{let mut value=json!({"entryId":q.entry_id,"kind":q.kind,"type":q.item_type});if let Some(m)=&q.message{value["message"]=json!(m);}
    if let Some(t)=&q.custom_type{value["customType"]=json!(t);}
    if let Some(d)=&q.data{value["data"]=d.clone();}value}).collect::<Vec<_>>(),"faulted":faulted});
    if let Some(id)=&captured.last_operation_id {snapshot["lastResult"]=reader.get_value(&operation_result(id),context).await?.ok_or_else(||session_invariant_error(format!("Lane {:?} is missing result {id}",lane.name())))?.value;}
    if let Some(operation)=captured.operation {
        let mut projected=json!({"id":operation.meta.operation_id,"kind":operation.meta.intent.kind(),"startedAt":operation.meta.started_at,"fromTipId":operation.meta.source_tip_id,"status":if matches!(operation.state.operation_scope_of().control,Control::CancelRequested {..}){"aborting"}else{"open"},"runningTools":[]});
        let response_id=match &operation.state {
            OperationState::AssistantEffectPending(s)=>Some(&s.response_entry_id),
            OperationState::DeferredEffectPending(s)=>Some(&s.response_entry_id),
            _=>None,
        };
        if let Some(id)=response_id {let frames=super::progress::read_assistant_frames(reader,&operation.meta.operation_id,id,context).await?;if let Some(message)=maho_ai::utils::assistant_message_frame::reduce_assistant_message_frames(frames.iter()).map_err(|e|session_invariant_error(e.to_string()))?{projected["streamingMessage"]=json!(message);}}
        let deferred=match &operation.state {OperationState::DeferredSuspended(s)=>Some(&s.deferred),OperationState::DeferredEffectPending(s)=>Some(&s.deferred),_=>None};
        if let Some(scope)=deferred {let entries=reader.get_entries(vec![scope.source_entry_id.clone()],context).await?;let handle=entries.get(&scope.source_entry_id).and_then(|e|e.kind.message()).and_then(|m|m.try_as_llm()).and_then(|m|match m{Message::Assistant(a)=>a.deferred.as_ref(),_=>None}).ok_or_else(||session_invariant_error("Deferred source is missing its assistant handle"))?;projected["deferred"]=json!({"handle":handle,"poll":scope.poll});}
        match &operation.state {
            OperationState::AssistantRetryWait(s)=>projected["retry"]=json!({"attempt":s.retry_wait.next_attempt,"maxAttempts":s.assistant.generation_context.retry_policy.max_attempts,"nextAttemptAt":s.retry_wait.not_before}),
            OperationState::SummaryRetryWait(s)=>projected["retry"]=json!({"attempt":s.retry.retry_wait.next_attempt,"maxAttempts":s.retry.scope.summary_context.retry_policy.max_attempts,"nextAttemptAt":s.retry.retry_wait.not_before}),
            OperationState::Tools(s)=>{
                let entries=reader.get_entries(vec![s.batch.assistant_entry_id.clone()],context).await?;
                let Some(Message::Assistant(assistant))=entries.get(&s.batch.assistant_entry_id).and_then(|e|e.kind.message()).and_then(|m|m.try_as_llm())else{return Err(session_invariant_error("Tool batch assistant entry is invalid"));};
                let mut tools=Vec::new();
                for call in &s.batch.calls {
                    if matches!(call,ToolCall::Planned {..}|ToolCall::Completed {..}){continue;}
                    let Some(ContentBlock::ToolCall(block))=assistant.content.get(call.source_index())else{return Err(session_invariant_error(format!("Tool call source index {} does not name a tool-call block",call.source_index())));};
                    let args=reader.get_value(&operation_tool_args(&operation.meta.operation_id,&s.batch.turn_id,call.source_index()),context).await?;
                    let mut tool=json!({"toolCallId":block.id,"toolName":block.name});
                    if matches!(call,ToolCall::EffectPending {..}) {
                        tool["status"]=json!("running");tool["args"]=args.ok_or_else(||session_invariant_error(format!("Tool call {} is missing persisted arguments",block.id)))?.value;
                        if let Some(checkpoint)=reader.get_value(&pending_tool_output(&operation.meta.operation_id,call.result_entry_id()),context).await?{tool["result"]=checkpoint.value;}
                    }else{
                        let staged=reader.get_value(&pending_entry(call.result_entry_id()),context).await?.ok_or_else(||session_invariant_error(format!("Tool call {} is missing its staged result",call.result_entry_id())))?;
                        let pending:PendingEntry=serde_json::from_value(staged.value).map_err(|e|session_invariant_error(e.to_string()))?;
                        let PendingEntry::Message {payload}=pending else{return Err(session_invariant_error("Tool call is missing its staged result"));};
                        let Some(Message::ToolResult(message))=payload.try_as_llm()else{return Err(session_invariant_error("Tool call is missing its staged result"));};
                        if message.tool_call_id!=block.id||message.tool_name!=block.name{return Err(session_invariant_error(format!("Tool call {} has a mismatched staged result",call.result_entry_id())));}
                        tool["status"]=json!("settled");tool["args"]=args.map_or_else(||json!(block.arguments),|a|a.value);tool["isError"]=json!(message.is_error);
                        let mut result=json!({"content":message.content});if let Some(details)=&message.details{result["details"]=details.clone();}
    if let Some(usage)=message.usage{result["usage"]=json!(usage);}
    if let Some(names)=&message.added_tool_names{result["addedToolNames"]=json!(names);}
    if matches!(call,ToolCall::OutcomeReady {terminate:true,..}){result["terminate"]=Value::Bool(true);}tool["result"]=result;
                    }
                    tools.push(tool);
                }
                projected["runningTools"]=json!(tools);
            },
            _=>{},
        }
        snapshot["operation"]=projected;
    }
    Ok(snapshot)
}

pub async fn read_bounded_entries(lane: &dyn RuntimeLane, drive: &Drive, capability: &OperationState) -> Result<ContinueOperationResult<Vec<Entry>>, SessionError> {
    let mutation = lane.session().begin_mutation(&drive.context).await?;
    let result = async {
        let state = lane.state();
        let operation = state.operation.as_ref().ok_or_else(|| session_invariant_error("Lane has no active operation"))?;
        let _ = capability;
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
