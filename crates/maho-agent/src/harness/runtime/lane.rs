//! Port of senpi `runtime/lane.ts`: serialized commands and durable lane publication.

use std::sync::{Arc, Mutex};

pub use super::types::{CommitDecision, ContinueOperationResult, Drive, FinishDecision, LaneCommand, LaneState, OperationCommand, ProcedureResult};
use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventBus, HarnessEventPayload};
use crate::harness::session::session::{SessionError, SessionErrorKind, session_invariant_error};
use crate::harness::session::types::{
    Control, InboxItem, InboxItemKind, LaneConfiguration, LaneModelRef, NewEntry, Operation,
    OperationMeta, OperationState, PendingEntry, Session, SessionReader, Write,
};
use crate::harness::session::values::*;
use maho_ai::types::{BoxFuture, ModelThinkingLevel};


pub enum QueuedInput {
    Text(String),
    Message(Box<crate::types::AgentMessage>),
}

pub enum PromptInput {
    Text { text: String, images: Vec<maho_ai::types::ImageContent> },
    Messages(Vec<crate::types::AgentMessage>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationAdmission {
    pub operation_id: String,
    pub started_at: i64,
    pub kind: crate::harness::session::types::OperationIntentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("Lane {lane:?} already has an active operation")]
    LaneBusy { lane: String, operation_id: String },
    #[error("Acceptance must append at least one message")]
    Empty,
    #[error("Cannot accept a pending assistant message")]
    PendingAssistant,
    #[error("Lane has nothing to compact")]
    NothingToCompact,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbortRequest {
    pub operation_id: String,
    pub newly_requested: bool,
    pub steer: Vec<crate::types::AgentMessage>,
    pub follow_up: Vec<crate::types::AgentMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationMismatch {
    pub expected: String,
    pub current_operation_id: Option<String>,
    pub last_operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneExecutionInfo {
    pub lane: String,
    pub tip_id: Option<String>,
    pub configured_model: LaneModelRef,
    pub current: Option<Operation>,
    pub captured_model: Option<LaneModelRef>,
    pub last_operation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelQueuedOutcome { Cancelled, AlreadyConsumed, NotFound }

pub fn inbox_items(inbox: &[InboxItem], kind: InboxItemKind) -> Vec<InboxItem> {
    inbox
        .iter()
        .filter(|item| item.kind == kind)
        .cloned()
        .collect()
}

pub fn without_inbox_items(inbox: &[InboxItem], removed: &[InboxItem]) -> Vec<InboxItem> {
    inbox
        .iter()
        .filter(|item| !removed.iter().any(|other| other.entry_id == item.entry_id))
        .cloned()
        .collect()
}

pub fn select_accepted_inbox(
    inbox: &[InboxItem],
    steering: crate::types::QueueMode,
    follow_up: crate::types::QueueMode,
) -> (Vec<InboxItem>, Vec<InboxItem>) {
    let (mut steer_taken, mut follow_up_taken) = (false, false);
    let (mut selected, mut remainder) = (Vec::new(), Vec::new());
    for item in inbox {
        let eligible = match item.kind {
            InboxItemKind::Write | InboxItemKind::NextRun => true,
            InboxItemKind::Steer => steering == crate::types::QueueMode::All || !steer_taken,
            InboxItemKind::FollowUp => {
                follow_up == crate::types::QueueMode::All || !follow_up_taken
            }
        };
        if eligible {
            selected.push(item.clone());
            steer_taken |= item.kind == InboxItemKind::Steer;
            follow_up_taken |= item.kind == InboxItemKind::FollowUp;
        } else {
            remainder.push(item.clone());
        }
    }
    (selected, remainder)
}

pub fn pending_entry_write(id: String, pending: PendingEntry) -> NewEntry {
    match pending {
        PendingEntry::Message { payload } => NewEntry::message(id, None, payload),
        PendingEntry::Custom {
            custom_type,
            payload,
        } => match payload {
            Some(data) => NewEntry::custom_with_data(id, None, custom_type, data),
            None => NewEntry::custom(id, None, custom_type),
        },
    }
}

pub(crate) fn encoded<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, SessionError> {
    serde_json::to_value(value).map_err(|error| session_invariant_error(error.to_string()))
}

pub(crate) fn durable_lane_state(state: &LaneState) -> crate::harness::session::types::LaneState {
    crate::harness::session::types::LaneState {
        current_operation_id: state
            .operation
            .as_ref()
            .map(|op| op.meta.operation_id.clone()),
        last_operation_id: state.last_operation_id.clone(),
        inbox: state.inbox.clone(),
    }
}

pub struct Lane {
    pub name: String,
    pub session: Arc<dyn Session>,
    pub events: HarnessEventBus,
    state: Mutex<LaneState>,
    closed_error: Mutex<Option<SessionError>>,
}

impl Lane {
    pub fn new(
        name: String,
        session: Arc<dyn Session>,
        state: LaneState,
        events: HarnessEventBus,
    ) -> Self {
        Self {
            name,
            session,
            events,
            state: Mutex::new(state),
            closed_error: Mutex::new(None),
        }
    }

    pub fn state(&self) -> LaneState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn assert_open(&self) -> Result<(), SessionError> {
        match self
            .closed_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn seal(&self, error: SessionError) {
        let mut closed = self.closed_error.lock().unwrap_or_else(|e| e.into_inner());
        if closed.is_none() {
            *closed = Some(error);
        }
    }

    pub fn get_tip_id(&self) -> Result<Option<String>, SessionError> {
        self.assert_open()?;
        Ok(self.state().tip_id)
    }

    pub async fn inspect_execution(&self, context: &Context) -> Result<LaneExecutionInfo, SessionError> {
        let name = self.name.clone();
        self.command(move |state, _| Box::pin(async move {
            let captured_model = state.operation.as_ref().and_then(|operation| {
                let model = match &operation.state {
                    OperationState::AssistantReady(value) => &value.assistant.generation_context.configuration.model,
                    OperationState::AssistantEffectPending(value) => &value.assistant.generation_context.configuration.model,
                    OperationState::AssistantRetryWait(value) => &value.assistant.generation_context.configuration.model,
                    OperationState::Tools(value) => &value.batch.configuration.model,
                    OperationState::DeferredSuspended(value) => &value.deferred.configuration.model,
                    OperationState::DeferredEffectPending(value) => &value.deferred.configuration.model,
                    OperationState::SummaryReady(value) => &value.ready.scope.summary_context.configuration.model,
                    OperationState::SummaryEffectPending(value) => &value.pending.scope.summary_context.configuration.model,
                    OperationState::SummaryRetryWait(value) => &value.retry.scope.summary_context.configuration.model,
                    OperationState::Starting(_) | OperationState::Checkpoint(_) | OperationState::SummaryDeciding(_) | OperationState::NavigationReadyToCommit(_) => return None,
                };
                Some(model.clone())
            });
            Ok(LaneCommand::Return { result: LaneExecutionInfo { lane: name, tip_id: state.tip_id, configured_model: state.configuration.model, current: state.operation, captured_model, last_operation_id: state.last_operation_id } })
        }), context).await
    }

    pub async fn accept_compaction(&self, custom_instructions: Option<String>, operation_id: Option<String>, settings: crate::harness::session::types::RunSettings, context: &Context) -> Result<Result<OperationAdmission, AdmissionError>, SessionError> {
        use crate::harness::session::types::{StorageBranchScan, BranchOrder, EntryType, OperationScope, OperationMarker, OperationIntent, OperationIntentKind, SummaryDecidingOperation, SummaryTask, SummaryTaskReason, ResultBoundary};
        self.assert_open()?;
        let started_at = now_ms();
        let operation_id = operation_id.unwrap_or_else(|| (self.session.id_generator())(Some(started_at)));
        let task_id = (self.session.id_generator())(Some(started_at));
        let name = self.name.clone();
        let read_context = context.clone();
        self.command(move |mut state, reader| Box::pin(async move {
            if let Some(operation) = &state.operation { return Ok(LaneCommand::Return { result: Err(AdmissionError::LaneBusy { lane: name, operation_id: operation.meta.operation_id.clone() }) }); }
            let mut path = if let Some(tip) = &state.tip_id {
                let mut query = StorageBranchScan::new(tip);
                query.stop_at_type = Some(EntryType::Compaction);
                query.order = Some(BranchOrder::NewestFirst);
                reader.scan_branch(query, &read_context).await?
            } else { vec![] };
            path.reverse();
            let prepared = crate::harness::compaction::compaction::prepare_compaction(&path, settings.compaction).map_err(|error| session_invariant_error(error.to_string()))?;
            let Some(prepared) = prepared else { return Ok(LaneCommand::Return { result: Err(AdmissionError::NothingToCompact) }); };
            let meta = OperationMeta { operation_id: operation_id.clone(), lane: name.clone(), source_tip_id: state.tip_id.clone(), started_at, intent: OperationIntent::Compaction { custom_instructions: custom_instructions.clone() } };
            let current = OperationState::SummaryDeciding(SummaryDecidingOperation { operation: OperationScope { control: Control::Running, settings, latest_assistant_entry_id: None }, at: OperationMarker::SummaryDeciding, task: SummaryTask { task_id: task_id.clone(), reason: Some(SummaryTaskReason::Manual), custom_instructions, boundary: ResultBoundary::Finish } });
            state.operation = Some(Operation { meta: meta.clone(), state: current.clone() });
            let writes = vec![
                Write::Value(set_value(&operation_preparation(&operation_id, &task_id), encoded(&super::structural::durable_compaction_preparation(&prepared))?)),
                Write::Value(set_value(&operation_meta(&operation_id), encoded(&meta)?)),
                Write::Value(set_value(&operation_state(&operation_id), encoded(&current)?)),
                Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?)),
            ];
            let admission = OperationAdmission { operation_id: operation_id.clone(), started_at, kind: OperationIntentKind::Compaction };
            Ok(LaneCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(move |_| Ok(admission.clone())), events: Some(Arc::new(move |_| vec![HarnessEvent::new(HarnessEventPayload::CompactionStart { run_id: operation_id.clone(), reason: "manual".into(), started_at }, Some(name.clone()))])) }, next: Box::new(state) })
        }), context).await
    }

    pub async fn request_operation_abort(&self, operation_id: String, context: &Context) -> Result<Result<AbortRequest, OperationMismatch>, SessionError> {
        let name = self.name.clone();
        let read_context = context.clone();
        self.command(move |mut state, reader| Box::pin(async move {
            let Some(mut operation) = state.operation.clone().filter(|operation| operation.meta.operation_id == operation_id) else {
                return Ok(LaneCommand::Return { result: Err(OperationMismatch { expected: operation_id, current_operation_id: state.operation.map(|operation| operation.meta.operation_id), last_operation_id: state.last_operation_id }) });
            };
            if matches!(operation.state.operation_scope_of().control, Control::CancelRequested { .. }) {
                return Ok(LaneCommand::Return { result: Ok(AbortRequest { operation_id, newly_requested: false, steer: vec![], follow_up: vec![] }) });
            }
            let removed: Vec<_> = state.inbox.iter().filter(|item| matches!(item.kind, InboxItemKind::Steer | InboxItemKind::FollowUp)).cloned().collect();
            let mut steer = vec![];
            let mut follow_up = vec![];
            for item in &removed {
                let stored = reader.get_value(&pending_entry(&item.entry_id), &read_context).await?.ok_or_else(|| session_invariant_error("Pending abort entry is missing its message"))?;
                let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|error| session_invariant_error(error.to_string()))?;
                let PendingEntry::Message { payload } = pending else { return Err(session_invariant_error("Pending abort entry is not a message")); };
                if item.kind == InboxItemKind::Steer { steer.push(payload); } else { follow_up.push(payload); }
            }
            let mut value = encoded(&operation.state)?;
            value["control"] = encoded(&Control::CancelRequested { requested_at: now_ms() })?;
            operation.state = serde_json::from_value(value.clone()).map_err(|error| session_invariant_error(error.to_string()))?;
            state.operation = Some(operation);
            state.inbox = without_inbox_items(&state.inbox, &removed);
            let queues = read_lane_queues(reader, &state.inbox, &read_context).await?;
            let mut writes: Vec<_> = removed.iter().map(|item| Write::Value(delete_value(&pending_entry(&item.entry_id)))).collect();
            writes.push(Write::Value(set_value(&operation_state(&operation_id), value)));
            writes.push(Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?)));
            let result = AbortRequest { operation_id: operation_id.clone(), newly_requested: true, steer: steer.clone(), follow_up: follow_up.clone() };
            Ok(LaneCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(move |_| Ok(result.clone())), events: Some(Arc::new(move |_| {
                let mut events = vec![HarnessEvent::new(HarnessEventPayload::OperationAbort { operation_id: operation_id.clone(), steer: steer.clone(), follow_up: follow_up.clone() }, Some(name.clone()))];
                if !removed.is_empty() { events.push(HarnessEvent::new(HarnessEventPayload::QueueUpdate { queues: queues.clone() }, Some(name.clone()))); }
                events
            })) }, next: Box::new(state) })
        }), context).await
    }

    pub async fn accept_prompt(&self, input: PromptInput, operation_id: Option<String>, settings: crate::harness::session::types::RunSettings, context: &Context) -> Result<Result<OperationAdmission, AdmissionError>, SessionError> {
        use crate::harness::session::types::{OperationIntent, OperationMarker, OperationScope, OperationIntentKind, StartingOperation};
        self.assert_open()?;
        let started_at = now_ms();
        let operation_id = operation_id.unwrap_or_else(|| (self.session.id_generator())(Some(started_at)));
        let messages = match input {
            PromptInput::Messages(messages) => messages,
            PromptInput::Text { text, images } => {
                if text.is_empty() && images.is_empty() { vec![] } else {
                    let mut content = vec![];
                    if !text.is_empty() { content.push(maho_ai::types::ContentBlock::text(text)); }
                    content.extend(images.into_iter().map(maho_ai::types::ContentBlock::Image));
                    vec![crate::types::AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage { content: maho_ai::types::UserContent::Blocks(content), timestamp: started_at }))]
                }
            }
        };
        if messages.iter().any(|message| matches!(message.try_as_llm(), Some(maho_ai::types::Message::Assistant(assistant)) if assistant.stop_reason == maho_ai::types::StopReason::Pending)) {
            return Ok(Err(AdmissionError::PendingAssistant));
        }
        let prompt: Vec<_> = messages.into_iter().map(|message| NewEntry::message((self.session.id_generator())(Some(started_at)), None, message)).collect();
        let name = self.name.clone();
        let read_context = context.clone();
        self.command(move |mut state, reader| Box::pin(async move {
            if let Some(operation) = &state.operation { return Ok(LaneCommand::Return { result: Err(AdmissionError::LaneBusy { lane: name, operation_id: operation.meta.operation_id.clone() }) }); }
            let (selected, remainder) = select_accepted_inbox(&state.inbox, settings.steering_mode, settings.follow_up_mode);
            let mut entries = vec![];
            for item in &selected {
                let stored = reader.get_value(&pending_entry(&item.entry_id), &read_context).await?.ok_or_else(|| session_invariant_error(format!("Pending {:?} entry {} is missing its payload", item.kind, item.entry_id)))?;
                let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|error| session_invariant_error(error.to_string()))?;
                queued_item(item, &pending)?;
                if matches!(&pending, PendingEntry::Message { payload } if matches!(payload.try_as_llm(), Some(maho_ai::types::Message::Assistant(message)) if message.stop_reason == maho_ai::types::StopReason::Pending)) { return Err(session_invariant_error("Pending entry contains a pending assistant")); }
                entries.push(pending_entry_write(item.entry_id.clone(), pending));
            }
            if prompt.is_empty() && !selected.iter().any(|item| item.kind != InboxItemKind::Write) { return Ok(LaneCommand::Return { result: Err(AdmissionError::Empty) }); }
            let meta = OperationMeta { operation_id: operation_id.clone(), lane: name.clone(), source_tip_id: state.tip_id.clone(), started_at, intent: OperationIntent::Run { prompt_entry_ids: prompt.iter().map(|entry| entry.id.clone()).collect() } };
            entries.extend(prompt);
            let mut tip = state.tip_id.clone();
            for entry in &mut entries { entry.parent_id = tip; tip = Some(entry.id.clone()); }
            let starting = OperationState::Starting(StartingOperation { at: OperationMarker::Starting, operation: OperationScope { control: Control::Running, settings, latest_assistant_entry_id: None } });
            state.tip_id = tip;
            state.inbox = remainder;
            state.operation = Some(Operation { meta: meta.clone(), state: starting.clone() });
            let queues = read_lane_queues(reader, &state.inbox, &read_context).await?;
            let mut writes: Vec<_> = entries.iter().cloned().map(crate::harness::session::commit::insert_entry).collect();
            writes.extend(selected.iter().map(|item| Write::Value(delete_value(&pending_entry(&item.entry_id)))));
            writes.extend([
                Write::Value(set_value(&branch_tip(&name), encoded(&state.tip_id)?)),
                Write::Value(set_value(&operation_meta(&operation_id), encoded(&meta)?)),
                Write::Value(set_value(&operation_state(&operation_id), encoded(&starting)?)),
                Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?)),
            ]);
            let admission = OperationAdmission { operation_id: operation_id.clone(), started_at, kind: OperationIntentKind::Run };
            Ok(LaneCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(move |_| Ok(admission.clone())), events: Some(Arc::new(move |commit| {
                let mut events = vec![HarnessEvent::run_start(&operation_id, started_at, &name)];
                for (entry, seq) in entries.iter().zip(&commit.seqs) {
                    let entry = crate::harness::session::types::Entry { id: entry.id.clone(), parent_id: entry.parent_id.clone(), seq: *seq, timestamp: commit.timestamp, kind: entry.kind.clone() };
                    if let crate::harness::session::types::EntryKind::Message { message, .. } = &entry.kind {
                        events.push(HarnessEvent::new(HarnessEventPayload::MessageStart { run_id: Some(operation_id.clone()), message: message.clone() }, Some(name.clone())));
                        events.push(HarnessEvent::new(HarnessEventPayload::MessageEnd { run_id: Some(operation_id.clone()), message: message.clone(), entry_id: Some(entry.id.clone()) }, Some(name.clone())));
                    }
                    events.push(HarnessEvent::new(HarnessEventPayload::EntryAdded { entry: Box::new(entry) }, Some(name.clone())));
                }
                if !selected.is_empty() { events.push(HarnessEvent::new(HarnessEventPayload::QueueUpdate { queues: queues.clone() }, Some(name.clone()))); }
                events
            })) }, next: Box::new(state) })
        }), context).await
    }

    /// The Session mutation barrier covers planning, commit and memory publication, but not event delivery.
    pub async fn command<T, F>(&self, plan: F, context: &Context) -> Result<T, SessionError>
    where
        F: for<'a> FnOnce(
            LaneState,
            &'a dyn SessionReader,
        ) -> BoxFuture<'a, Result<LaneCommand<T>, SessionError>>,
    {
        self.assert_open()?;
        let mutation = self.session.begin_mutation(context).await?;
        let outcome: Result<(_, Option<BoxFuture<'static, ()>>), SessionError> = async {
            self.assert_open()?;
            match plan(self.state(), mutation.as_ref()).await? {
                LaneCommand::Return { result } => Ok((Ok(result), None)),
                LaneCommand::Reject { error } => Ok((
                    Err(SessionError::new(SessionErrorKind::Invariant, error)),
                    None,
                )),
                LaneCommand::Commit { decision, next } => {
                    let commit = mutation.commit(decision.writes, context).await?;
                    *self.state.lock().unwrap_or_else(|e| e.into_inner()) = *next;
                    let result = (decision.materialize)(&commit);
                    let events = decision
                        .events
                        .map(|emit| emit(&commit))
                        .unwrap_or_default();
                    let delivery = self.events.begin_emit_batch(events, context.clone());
                    Ok((Ok(result), Some(delivery)))
                }
            }
        }
        .await;
        if let Err(error) = &outcome {
            self.seal(error.clone());
        }
        mutation.end(context).await;
        let (result, delivery) = outcome?;
        if let Some(delivery) = delivery {
            delivery.await;
        }
        result
    }

    pub async fn settle_operation<T, F>(
        &self,
        plan: F,
        context: &Context,
    ) -> Result<T, SessionError>
    where
        F: for<'a> FnOnce(
                LaneState,
                OperationState,
                OperationMeta,
                &'a dyn SessionReader,
            ) -> BoxFuture<'a, Result<OperationCommand<T>, SessionError>>
            + Send
            + 'static,
    {
        let name = self.name.clone();
        self.command(
            move |state, reader| {
                Box::pin(async move {
                    let operation = state
                        .operation
                        .clone()
                        .ok_or_else(|| session_invariant_error("Lane has no active operation"))?;
                    let decision = plan(
                        state.clone(),
                        operation.state,
                        operation.meta.clone(),
                        reader,
                    )
                    .await?;
                    let (next, mut writes, materialize, events) = match decision {
                        OperationCommand::Return { result } => {
                            return Ok(LaneCommand::Return { result });
                        }
                        OperationCommand::Commit {
                            decision,
                            operation_state: next_state,
                            lane,
                        } => {
                            let mut next = state;
                            if let Some(patch) = lane {
                                if let Some(tip) = patch.tip_id {
                                    next.tip_id = tip;
                                }
                                if let Some(config) = patch.configuration {
                                    next.configuration = config;
                                }
                                if let Some(inbox) = patch.inbox {
                                    next.inbox = inbox;
                                }
                            }
                            next.operation = Some(Operation {
                                meta: operation.meta.clone(),
                                state: *next_state,
                            });
                            let mut writes = decision.writes;
                            writes.push(Write::Value(set_value(
                                &operation_state(&operation.meta.operation_id),
                                encoded(&next.operation.as_ref().map(|op| &op.state))?,
                            )));
                            (next, writes, decision.materialize, decision.events)
                        }
                        OperationCommand::Finish { decision } => {
                            let mut next = state;
                            if let Some(patch) = decision.lane {
                                if let Some(tip) = patch.tip_id {
                                    next.tip_id = tip;
                                }
                                if let Some(config) = patch.configuration {
                                    next.configuration = config;
                                }
                                if let Some(inbox) = patch.inbox {
                                    next.inbox = inbox;
                                }
                            }
                            next.operation = None;
                            next.last_operation_id = Some(operation.meta.operation_id.clone());
                            let mut writes = decision.writes;
                            writes.push(Write::Value(set_value(
                                &operation_result(&operation.meta.operation_id),
                                encoded(&decision.record)?,
                            )));
                            (next, writes, decision.materialize, decision.events)
                        }
                    };
                    writes.push(Write::Value(set_value(
                        &lane_state(&name),
                        encoded(&durable_lane_state(&next))?,
                    )));
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes,
                            materialize,
                            events,
                        },
                        next: Box::new(next),
                    })
                })
            },
            context,
        )
        .await
    }

    pub async fn continue_operation<T, F>(
        &self,
        plan: F,
        context: &Context,
    ) -> Result<ContinueOperationResult<T>, SessionError>
    where
        T: Send + Sync + 'static,
        F: for<'a> FnOnce(
                LaneState,
                OperationState,
                OperationMeta,
                &'a dyn SessionReader,
            ) -> BoxFuture<'a, Result<OperationCommand<T>, SessionError>>
            + Send
            + 'static,
    {
        self.settle_operation(
            move |state, current, meta, reader| {
                Box::pin(async move {
                    if matches!(
                        current.operation_scope_of().control,
                        Control::CancelRequested { .. }
                    ) {
                        return Ok(OperationCommand::Return {
                            result: ContinueOperationResult::CancelRequested,
                        });
                    }
                    Ok(match plan(state, current, meta, reader).await? {
                        OperationCommand::Return { result } => OperationCommand::Return {
                            result: ContinueOperationResult::Result { value: result },
                        },
                        OperationCommand::Commit {
                            decision,
                            operation_state,
                            lane,
                        } => OperationCommand::Commit {
                            decision: CommitDecision {
                                writes: decision.writes,
                                events: decision.events,
                                materialize: Arc::new(move |commit| {
                                    ContinueOperationResult::Result {
                                        value: (decision.materialize)(commit),
                                    }
                                }),
                            },
                            operation_state,
                            lane,
                        },
                        OperationCommand::Finish { decision } => {
                            let FinishDecision {
                                writes,
                                record,
                                lane,
                                events,
                                materialize,
                            } = *decision;
                            OperationCommand::Finish {
                                decision: Box::new(FinishDecision {
                                    writes,
                                    record,
                                    lane,
                                    events,
                                    materialize: Arc::new(move |commit| {
                                        ContinueOperationResult::Result {
                                            value: materialize(commit),
                                        }
                                    }),
                                }),
                            }
                        }
                    })
                })
            },
            context,
        )
        .await
    }

    async fn set_configuration<F>(
        &self,
        update: F,
        property: &'static str,
        context: &Context,
    ) -> Result<(), SessionError>
    where
        F: FnOnce(&mut LaneConfiguration) + Send + 'static,
    {
        let name = self.name.clone();
        self.command(
            move |mut state, _| {
                Box::pin(async move {
                    let previous = encoded(&state.configuration)?;
                    update(&mut state.configuration);
                    let value = encoded(&state.configuration)?;
                    let key = if property == "activeTools" {
                        "activeToolNames"
                    } else {
                        property
                    };
                    let event = HarnessEvent::new(
                        HarnessEventPayload::ConfigUpdate {
                            property: property.into(),
                            previous: previous[key].clone(),
                            value: value[key].clone(),
                        },
                        Some(name.clone()),
                    );
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes: vec![Write::Value(set_value(&lane_config(&name), value))],
                            materialize: Arc::new(|_| ()),
                            events: Some(Arc::new(move |_| vec![event.clone()])),
                        },
                        next: Box::new(state),
                    })
                })
            },
            context,
        )
        .await
    }

    pub async fn set_model(
        &self,
        model: LaneModelRef,
        context: &Context,
    ) -> Result<(), SessionError> {
        self.set_configuration(move |config| config.model = model, "model", context)
            .await
    }
    pub async fn set_thinking_level(
        &self,
        level: ModelThinkingLevel,
        context: &Context,
    ) -> Result<(), SessionError> {
        self.set_configuration(
            move |config| config.thinking_level = level,
            "thinkingLevel",
            context,
        )
        .await
    }
    pub async fn set_active_tools(
        &self,
        names: Vec<String>,
        context: &Context,
    ) -> Result<(), SessionError> {
        self.set_configuration(
            move |config| config.active_tool_names = names,
            "activeTools",
            context,
        )
        .await
    }
    pub fn get_thinking_level(&self) -> Result<ModelThinkingLevel, SessionError> {
        self.assert_open()?;
        Ok(self.state().configuration.thinking_level)
    }
    pub fn get_active_tools(&self) -> Result<Vec<String>, SessionError> {
        self.assert_open()?;
        Ok(self.state().configuration.active_tool_names)
    }

    pub async fn get_result(&self, id: &str, context: &Context) -> Result<Option<crate::harness::session::types::OperationResultRecord>, SessionError> {
        self.assert_open()?;
        self.session.get_value(&operation_result(id), context).await?
            .map(|stored| serde_json::from_value(stored.value).map_err(|error| session_invariant_error(error.to_string()))).transpose()
    }

    pub async fn steer(&self, input: QueuedInput, images: Vec<maho_ai::types::ImageContent>, context: &Context) -> Result<String, SessionError> { self.enqueue(InboxItemKind::Steer, input, images, context).await }
    pub async fn follow_up(&self, input: QueuedInput, images: Vec<maho_ai::types::ImageContent>, context: &Context) -> Result<String, SessionError> { self.enqueue(InboxItemKind::FollowUp, input, images, context).await }
    pub async fn next_run(&self, input: QueuedInput, images: Vec<maho_ai::types::ImageContent>, context: &Context) -> Result<String, SessionError> { self.enqueue(InboxItemKind::NextRun, input, images, context).await }

    async fn enqueue(&self, kind: InboxItemKind, input: QueuedInput, images: Vec<maho_ai::types::ImageContent>, context: &Context) -> Result<String, SessionError> {
        self.assert_open()?;
        let at = now_ms();
        let message = match input {
            QueuedInput::Text(text) => {
                if text.is_empty() && images.is_empty() { return Err(session_invariant_error("Queued input must contain text or an image")); }
                let mut content = Vec::new();
                if !text.is_empty() { content.push(maho_ai::types::ContentBlock::text(text)); }
                content.extend(images.into_iter().map(maho_ai::types::ContentBlock::Image));
                crate::types::AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage { content: maho_ai::types::UserContent::Blocks(content), timestamp: at }))
            },
            QueuedInput::Message(message) => {
                let mut message = *message;
                if matches!(message.try_as_llm(), Some(maho_ai::types::Message::Assistant(assistant)) if assistant.stop_reason == maho_ai::types::StopReason::Pending) { return Err(session_invariant_error("Cannot queue a pending assistant message")); }
                if !images.is_empty() {
                    let crate::types::AgentMessage::Llm(maho_ai::types::Message::User(user)) = &mut message else { return Err(session_invariant_error("Images can be added only to queued user messages")); };
                    let mut content = match &user.content {
                        maho_ai::types::UserContent::Text(text) => if text.is_empty() { vec![] } else { vec![maho_ai::types::ContentBlock::text(text.clone())] },
                        maho_ai::types::UserContent::Blocks(blocks) => blocks.clone(),
                    };
                    content.extend(images.into_iter().map(maho_ai::types::ContentBlock::Image));
                    user.content = maho_ai::types::UserContent::Blocks(content);
                }
                message
            },
        };
        let id = (self.session.id_generator())(Some(at));
        let name = self.name.clone();
        let read_context = context.clone();
        self.command(move |mut state, reader| Box::pin(async move {
            let mut queues = read_lane_queues(reader, &state.inbox, &read_context).await?;
            let item = InboxItem { entry_id: id.clone(), kind };
            let pending = PendingEntry::Message { payload: message };
            queues.push(queued_item(&item, &pending)?);
            state.inbox.push(item);
            let writes = vec![Write::Value(set_value(&pending_entry(&id), encoded(&pending)?)), Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?))];
            Ok(LaneCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(move |_| id.clone()), events: Some(Arc::new(move |_| vec![HarnessEvent::new(HarnessEventPayload::QueueUpdate { queues: queues.clone() }, Some(name.clone()))])) }, next: Box::new(state) })
        }), context).await
    }

    pub async fn cancel_queued(&self, id: String, context: &Context) -> Result<CancelQueuedOutcome, SessionError> {
        self.assert_open()?;
        let name = self.name.clone();
        let read_context = context.clone();
        self.command(move |mut state, reader| Box::pin(async move {
            if !state.inbox.iter().any(|item| item.entry_id == id) {
                let consumed = reader.get_entries(vec![id.clone()], &read_context).await?.contains_key(&id);
                return Ok(LaneCommand::Return { result: if consumed { CancelQueuedOutcome::AlreadyConsumed } else { CancelQueuedOutcome::NotFound } });
            }
            if reader.get_value(&pending_entry(&id), &read_context).await?.is_none() { return Err(session_invariant_error(format!("Queued entry {id} is missing its payload"))); }
            state.inbox.retain(|item| item.entry_id != id);
            let queues = read_lane_queues(reader, &state.inbox, &read_context).await?;
            let writes = vec![Write::Value(delete_value(&pending_entry(&id))), Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?))];
            Ok(LaneCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(|_| CancelQueuedOutcome::Cancelled), events: Some(Arc::new(move |_| vec![HarnessEvent::new(HarnessEventPayload::QueueUpdate { queues: queues.clone() }, Some(name.clone()))])) }, next: Box::new(state) })
        }), context).await
    }

    pub async fn record_usage(&self, usage: maho_ai::types::Usage, entry_id: Option<String>, details: Option<serde_json::Value>, context: &Context) -> Result<String, SessionError> {
        self.assert_open()?;
        let id = (self.session.id_generator())(None);
        let name = self.name.clone();
        self.command(move |state, _| Box::pin(async move {
            let row = crate::harness::session::types::NewUsageRow { id: id.clone(), usage, entry_id, adjustment: true, details };
            Ok(LaneCommand::Commit { decision: CommitDecision {
                writes: vec![crate::harness::session::commit::insert_usage(row.clone())], materialize: Arc::new(move |_| id.clone()),
                events: Some(Arc::new(move |commit| vec![HarnessEvent::new(HarnessEventPayload::Usage { lane: name.clone(), row: crate::harness::session::types::UsageRow { id: row.id.clone(), seq: commit.seqs[0], usage: row.usage, entry_id: row.entry_id.clone(), adjustment: true, details: row.details.clone() }, totals: commit.stats.usage }, Some(name.clone()))])),
            }, next: Box::new(state) })
        }), context).await
    }

    pub async fn find_entries(&self, query: Option<crate::harness::session::types::BranchScan>, context: &Context) -> Result<Vec<crate::harness::session::types::Entry>, SessionError> {
        self.assert_open()?;
        let query = query.unwrap_or_default();
        let Some(start) = query.start.or(self.state().tip_id) else { return Ok(vec![]); };
        self.session.scan_branch(crate::harness::session::types::StorageBranchScan {
            start, stop_at_type: query.stop_at_type, stop_at_id: query.stop_at_id, entry_type: query.entry_type, custom_type: query.custom_type, order: Some(query.order.unwrap_or(crate::harness::session::types::BranchOrder::NewestFirst)), limit: query.limit, cursor: query.cursor,
        }, context).await
    }

    pub async fn find_entry(&self, query: Option<crate::harness::session::types::BranchScan>, context: &Context) -> Result<Option<crate::harness::session::types::Entry>, SessionError> {
        let mut query = query.unwrap_or_default();
        query.limit = Some(query.limit.unwrap_or(1).min(1));
        Ok(self.find_entries(Some(query), context).await?.into_iter().next())
    }

    pub async fn append_message(&self, message: crate::types::AgentMessage, context: &Context) -> Result<String, SessionError> {
        self.append(PendingEntry::Message { payload: message }, context).await
    }

    pub async fn append_custom_entry(&self, custom_type: String, data: Option<serde_json::Value>, context: &Context) -> Result<String, SessionError> {
        self.append(PendingEntry::Custom { custom_type, payload: data }, context).await
    }

    async fn append(&self, pending: PendingEntry, context: &Context) -> Result<String, SessionError> {
        self.assert_open()?;
        if let PendingEntry::Message { payload } = &pending
            && matches!(payload.try_as_llm(), Some(maho_ai::types::Message::Assistant(message)) if message.stop_reason == maho_ai::types::StopReason::Pending) {
                return Err(crate::harness::session::session::session_pending_assistant_message_error());
        }
        let id = (self.session.id_generator())(None);
        let name = self.name.clone();
        let context_read = context.clone();
        self.command(move |mut state, reader| Box::pin(async move {
            if state.operation.is_none() {
                let queued = inbox_items(&state.inbox, InboxItemKind::Write);
                let mut entries = Vec::with_capacity(queued.len() + 1);
                for item in &queued {
                    let stored = reader.get_value(&pending_entry(&item.entry_id), &context_read).await?.ok_or_else(|| session_invariant_error(format!("Pending write {} is missing its payload", item.entry_id)))?;
                    let pending = serde_json::from_value(stored.value).map_err(|error| session_invariant_error(error.to_string()))?;
                    entries.push(pending_entry_write(item.entry_id.clone(), pending));
                }
                state.inbox = without_inbox_items(&state.inbox, &queued);
                let queues = if queued.is_empty() { None } else { Some(read_lane_queues(reader, &state.inbox, &context_read).await?) };
                entries.push(pending_entry_write(id.clone(), pending));
                let mut parent = state.tip_id.clone();
                for entry in &mut entries { entry.parent_id = parent; parent = Some(entry.id.clone()); }
                state.tip_id = Some(id.clone());
                let mut writes: Vec<_> = entries.iter().cloned().map(crate::harness::session::commit::insert_entry).collect();
                writes.extend(queued.iter().map(|item| Write::Value(delete_value(&pending_entry(&item.entry_id)))));
                writes.push(Write::Value(set_value(&branch_tip(&name), encoded(&state.tip_id)?)));
                writes.push(Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?)));
                Ok(LaneCommand::Commit { decision: CommitDecision {
                    writes, materialize: Arc::new(move |_| id.clone()), events: Some(Arc::new(move |commit| {
                        let mut events = Vec::new();
                        for (entry, seq) in entries.iter().zip(&commit.seqs) {
                            let entry = crate::harness::session::types::Entry { id: entry.id.clone(), parent_id: entry.parent_id.clone(), seq: *seq, timestamp: commit.timestamp, kind: entry.kind.clone() };
                            if let crate::harness::session::types::EntryKind::Message { message, .. } = &entry.kind {
                                events.push(HarnessEvent::new(HarnessEventPayload::MessageStart { run_id: None, message: message.clone() }, Some(name.clone())));
                                events.push(HarnessEvent::new(HarnessEventPayload::MessageEnd { run_id: None, message: message.clone(), entry_id: Some(entry.id.clone()) }, Some(name.clone())));
                            }
                            events.push(HarnessEvent::new(HarnessEventPayload::EntryAdded { entry: Box::new(entry) }, Some(name.clone())));
                        }
                        if let Some(queues) = &queues { events.push(HarnessEvent::new(HarnessEventPayload::QueueUpdate { queues: queues.clone() }, Some(name.clone()))); }
                        events
                    })),
                }, next: Box::new(state) })
            } else {
                let mut queues = read_lane_queues(reader, &state.inbox, &context_read).await?;
                queues.push(queued_item(&InboxItem { entry_id: id.clone(), kind: InboxItemKind::Write }, &pending)?);
                state.inbox.push(InboxItem { entry_id: id.clone(), kind: InboxItemKind::Write });
                let writes = vec![Write::Value(set_value(&pending_entry(&id), encoded(&pending)?)), Write::Value(set_value(&lane_state(&name), encoded(&durable_lane_state(&state))?))];
                Ok(LaneCommand::Commit { decision: CommitDecision { writes, materialize: Arc::new(move |_| id.clone()), events: Some(Arc::new(move |_| vec![HarnessEvent::new(HarnessEventPayload::QueueUpdate { queues: queues.clone() }, Some(name.clone()))])) }, next: Box::new(state) })
            }
        }), context).await
    }
}

fn queued_item(item: &InboxItem, pending: &PendingEntry) -> Result<crate::harness::events::LaneQueuedItem, SessionError> {
    let kind = match item.kind { InboxItemKind::Write => "write", InboxItemKind::Steer => "steer", InboxItemKind::FollowUp => "followUp", InboxItemKind::NextRun => "nextRun" }.to_owned();
    let (item_type, message, custom_type, data) = match pending {
        PendingEntry::Message { payload } => ("message", Some(payload.clone()), None, None),
        PendingEntry::Custom { custom_type, payload } => {
            if item.kind != InboxItemKind::Write { return Err(session_invariant_error(format!("Pending {kind} entry {} is not a message", item.entry_id))); }
            ("custom", None, Some(custom_type.clone()), payload.clone())
        },
    };
    Ok(crate::harness::events::LaneQueuedItem { entry_id: item.entry_id.clone(), kind, item_type: item_type.into(), message, custom_type, data })
}

async fn read_lane_queues(reader: &dyn SessionReader, inbox: &[InboxItem], context: &Context) -> Result<Vec<crate::harness::events::LaneQueuedItem>, SessionError> {
    let mut queues = Vec::with_capacity(inbox.len());
    for item in inbox {
        let stored = reader.get_value(&pending_entry(&item.entry_id), context).await?.ok_or_else(|| session_invariant_error(format!("Pending {:?} entry {} is missing its payload", item.kind, item.entry_id)))?;
        let pending: PendingEntry = serde_json::from_value(stored.value).map_err(|error| session_invariant_error(error.to_string()))?;
        queues.push(queued_item(item, &pending)?);
    }
    Ok(queues)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
