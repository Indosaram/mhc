//! Port of senpi `packages/agent/src/harness/runtime/progress.ts`.

use std::sync::{Arc, Mutex};
use maho_ai::utils::assistant_message_frame::AssistantMessageFrame;
use crate::harness::context::Context;
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{OperationState, SessionReader, ToolCall, Write};
use crate::harness::session::values::{append_list, pending_assistant_frames, pending_tool_output, set_value, ListCursor, ListOrder, ListReadOptions};
use super::types::{Drive, LaneState, RuntimeLane};

pub async fn read_assistant_frames(reader: &dyn SessionReader, operation_id: &str, response_entry_id: &str, context: &Context) -> Result<Vec<AssistantMessageFrame>, SessionError> {
    let address = pending_assistant_frames(operation_id, response_entry_id);
    let mut frames = Vec::new();
    let mut cursor = None;
    loop {
        let page = reader.read_list(&address, Some(ListReadOptions { order: Some(ListOrder::Asc), limit: Some(1000), cursor }), context).await?;
        let length = page.len();
        cursor = page.last().map(|item| ListCursor { seq: item.seq });
        for item in page { frames.push(serde_json::from_value(item.value).map_err(|e| session_invariant_error(e.to_string()))?); }
        if length < 1000 { return Ok(frames); }
    }
}

type CommitWrite<T> = Arc<dyn Fn(T) -> Result<Write, SessionError> + Send + Sync>;
type StillOwns = Arc<dyn Fn(&LaneState) -> bool + Send + Sync>;

enum ProgressWrite<T> { Item(T), Drain(tokio::sync::oneshot::Sender<()>) }

pub struct ProgressChannel<T> {
    sender: Mutex<Option<tokio::sync::mpsc::UnboundedSender<ProgressWrite<T>>>>,
    latest: Arc<Mutex<Result<(), SessionError>>>,
    drained: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl<T> ProgressChannel<T> {
    pub fn write(&self, item: T) {
        if let Some(sender) = self.sender.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            && sender.send(ProgressWrite::Item(item)).is_err() { *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = Err(session_invariant_error("Progress worker closed")); }
    }
    pub fn seal(&self) { self.sender.lock().unwrap_or_else(|e| e.into_inner()).take(); }
    pub async fn drain(&self) -> Result<(), SessionError> {
        let (send, receive) = tokio::sync::oneshot::channel();
        let sent = self.sender.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|sender| sender.send(ProgressWrite::Drain(send)));
        if let Some(result) = sent {
            result.map_err(|_| session_invariant_error("Progress worker closed"))?;
            receive.await.map_err(|e| session_invariant_error(e.to_string()))?;
        } else if let Some(task) = self.drained.lock().await.take() { task.await.map_err(|e| session_invariant_error(e.to_string()))?; }
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn open_progress<T: Send + 'static>(lane: Arc<dyn RuntimeLane>, drive: &Drive, commit_write: CommitWrite<T>, still_owns: StillOwns) -> ProgressChannel<T> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let context = drive.context.clone();
    let latest = Arc::new(Mutex::new(Ok(())));
    let settled = latest.clone();
    let worker = tokio::spawn(async move {
        while let Some(write) = receiver.recv().await {
            let item = match write { ProgressWrite::Item(item) => item, ProgressWrite::Drain(sender) => { if sender.send(()).is_err() { return; } continue; } };
            let result = async {
                let mutation = lane.session().begin_mutation(&context).await?;
                let result = async {
                    if still_owns(&lane.state()) { mutation.commit(vec![commit_write(item)?], &context).await?; }
                    Ok(())
                }.await;
                mutation.end(&context).await;
                result
            }.await;
            *settled.lock().unwrap_or_else(|e| e.into_inner()) = result;
        }
    });
    ProgressChannel { sender: Mutex::new(Some(sender)), latest, drained: tokio::sync::Mutex::new(Some(worker)) }
}

pub fn open_frame_progress(lane: Arc<dyn RuntimeLane>, drive: &Drive, response_entry_id: &str) -> ProgressChannel<AssistantMessageFrame> {
    let address = pending_assistant_frames(&drive.operation_id, response_entry_id);
    let id = response_entry_id.to_owned();
    open_progress(lane, drive, Arc::new(move |frame| Ok(Write::List(append_list(&address, serde_json::to_value(frame).map_err(|e| session_invariant_error(e.to_string()))?)))), Arc::new(move |state| {
        state.operation.as_ref().is_some_and(|op| match &op.state { OperationState::AssistantEffectPending(s) => s.response_entry_id == id, OperationState::DeferredEffectPending(s) => s.response_entry_id == id, _ => false })
    }))
}

pub fn open_tool_progress(lane: Arc<dyn RuntimeLane>, drive: &Drive, turn_id: &str, source_index: usize, invocation_id: &str) -> ProgressChannel<crate::types::AgentToolResult> {
    let address = pending_tool_output(&drive.operation_id, invocation_id);
    let (turn, invocation) = (turn_id.to_owned(), invocation_id.to_owned());
    open_progress(lane, drive, Arc::new(move |snapshot| Ok(Write::Value(set_value(&address, serde_json::to_value(snapshot).map_err(|e| session_invariant_error(e.to_string()))?)))), Arc::new(move |state| {
        state.operation.as_ref().is_some_and(|op| match &op.state { OperationState::Tools(s) => s.batch.turn_id == turn && s.batch.calls.iter().any(|call| matches!(call, ToolCall::EffectPending { source_index: index, result_entry_id, .. } if *index == source_index && result_entry_id == &invocation)), _ => false })
    }))
}
