//! Port of senpi `runtime/lane.ts`: serialized commands and durable lane publication.

use std::sync::{Arc, Mutex};

use super::types::{ContinueOperationResult, LaneState};
use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventBus, HarnessEventPayload};
use crate::harness::session::session::{SessionError, SessionErrorKind, session_invariant_error};
use crate::harness::session::types::{
    Control, InboxItem, InboxItemKind, LaneConfiguration, LaneModelRef, NewEntry, Operation,
    OperationMeta, OperationState, PendingEntry, Session, SessionReader, Write,
};
use crate::harness::session::values::*;
use maho_ai::types::{BoxFuture, ModelThinkingLevel};

pub type CommitEvents =
    Arc<dyn Fn(&crate::harness::session::types::CommitResult) -> Vec<HarnessEvent> + Send + Sync>;

pub struct CommitDecision<T> {
    pub writes: Vec<Write>,
    pub materialize: Arc<dyn Fn(&crate::harness::session::types::CommitResult) -> T + Send + Sync>,
    pub events: Option<CommitEvents>,
}

pub enum LaneCommand<T> {
    Commit {
        decision: CommitDecision<T>,
        next: Box<LaneState>,
    },
    Return {
        result: T,
    },
    Reject {
        error: String,
    },
}

pub struct FinishDecision<T> {
    pub writes: Vec<Write>,
    pub record: crate::harness::session::types::OperationResultRecord,
    pub lane: Option<super::types::LanePatch>,
    pub materialize: Arc<dyn Fn(&crate::harness::session::types::CommitResult) -> T + Send + Sync>,
    pub events: Option<CommitEvents>,
}

pub enum OperationCommand<T> {
    Commit {
        decision: CommitDecision<T>,
        operation_state: Box<OperationState>,
        lane: Option<super::types::LanePatch>,
    },
    Finish {
        decision: Box<FinishDecision<T>>,
    },
    Return {
        result: T,
    },
}

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
}
