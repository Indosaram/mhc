//! Port of senpi `packages/agent/src/harness/runtime/restore.ts`.

use std::collections::BTreeMap;
use serde::de::DeserializeOwned;
use crate::harness::context::Context;
use crate::harness::session::session::{SessionError, session_invariant_error};
use crate::harness::session::types::{LaneConfiguration, Operation, OperationIntent, OperationMeta, OperationState, ResultBoundary, Session, SessionReader, SummaryTask};
use crate::harness::session::values::{self, StoredValue};
use super::types::LaneState;

fn summary_task(state: &OperationState) -> Option<&SummaryTask> {
    match state {
        OperationState::SummaryDeciding(s) => Some(&s.task),
        OperationState::SummaryReady(s) => Some(&s.ready.scope.task),
        OperationState::SummaryEffectPending(s) => Some(&s.pending.scope.task),
        OperationState::SummaryRetryWait(s) => Some(&s.retry.scope.task),
        OperationState::Starting(_) | OperationState::Checkpoint(_) | OperationState::AssistantReady(_) | OperationState::AssistantEffectPending(_) | OperationState::AssistantRetryWait(_) | OperationState::Tools(_) | OperationState::DeferredSuspended(_) | OperationState::DeferredEffectPending(_) | OperationState::NavigationReadyToCommit(_) => None,
    }
}

pub fn state_matches_intent(intent: &OperationIntent, state: &OperationState) -> bool {
    let task = summary_task(state);
    match intent {
        OperationIntent::Compaction { .. } => task.is_some_and(|t| matches!(t.boundary, ResultBoundary::Finish)),
        OperationIntent::Navigation { target_id, summarize, label, custom_instructions } => {
            if let OperationState::NavigationReadyToCommit(s) = state {
                return !summarize && s.target_id == *target_id && s.label == *label;
            }
            *summarize && task.is_some_and(|t| match &t.boundary {
                ResultBoundary::CommitNavigation { target_id: target, label: task_label } => target_id.as_ref() == Some(target) && label == task_label && custom_instructions == &t.custom_instructions,
                ResultBoundary::ResumeCheckpoint { .. } | ResultBoundary::Finish => false,
            })
        }
        OperationIntent::Run { .. } => !matches!(state, OperationState::NavigationReadyToCommit(_)) && task.is_none_or(|t| matches!(t.boundary, ResultBoundary::ResumeCheckpoint { .. })),
    }
}

#[derive(Debug, Clone)]
pub enum ClassifiedLaneStorage {
    Absent,
    Branch { tip: StoredValue },
    Lane { tip: StoredValue, configuration: Box<StoredValue>, lane_state: Box<StoredValue> },
}

fn classify_lane_storage(lane: &str, tip: Option<StoredValue>, configuration: Option<StoredValue>, lane_state: Option<StoredValue>) -> Result<ClassifiedLaneStorage, SessionError> {
    if tip.is_none() && configuration.is_none() && lane_state.is_none() { return Ok(ClassifiedLaneStorage::Absent); }
    let quoted = serde_json::Value::String(lane.to_owned());
    let tip = tip.ok_or_else(|| session_invariant_error(format!("Lane {quoted} is missing branch.tip")))?;
    if configuration.is_none() && lane_state.is_none() { return Ok(ClassifiedLaneStorage::Branch { tip }); }
    let configuration = configuration.ok_or_else(|| session_invariant_error(format!("Lane {quoted} is missing lane.config")))?;
    let lane_state = lane_state.ok_or_else(|| session_invariant_error(format!("Lane {quoted} is missing lane.state")))?;
    Ok(ClassifiedLaneStorage::Lane { tip, configuration: Box::new(configuration), lane_state: Box::new(lane_state) })
}

pub async fn read_lane_storage(reader: &dyn SessionReader, lane: &str, context: &Context) -> Result<ClassifiedLaneStorage, SessionError> {
    let (tip_address, config_address, state_address) = (values::branch_tip(lane), values::lane_config(lane), values::lane_state(lane));
    let (tip, configuration, state) = tokio::try_join!(reader.get_value(&tip_address, context), reader.get_value(&config_address, context), reader.get_value(&state_address, context))?;
    classify_lane_storage(lane, tip, configuration, state)
}

pub async fn restore_session(session: &dyn Session, context: &Context) -> Result<BTreeMap<String, LaneState>, SessionError> {
    let mutation = session.begin_mutation(context).await?;
    let result = async {
        let (tip_prefix, config_prefix, state_prefix) = (values::branch_tip_inventory_prefix(), values::lane_config(""), values::lane_state(""));
        let (tips, configurations, states) = tokio::try_join!(mutation.scan_values(&tip_prefix, context), mutation.scan_values(&config_prefix, context), mutation.scan_values(&state_prefix, context))?;
        type LaneValues = (Option<StoredValue>, Option<StoredValue>, Option<StoredValue>);
        let mut inventory: BTreeMap<String, LaneValues> = BTreeMap::new();
        for value in tips { let key = value.address.key.clone(); inventory.entry(key).or_default().0 = Some(value); }
        for value in configurations { let key = value.address.key.clone(); inventory.entry(key).or_default().1 = Some(value); }
        for value in states { let key = value.address.key.clone(); inventory.entry(key).or_default().2 = Some(value); }
        let mut restored = BTreeMap::new();
        for (lane, (tip, configuration, state)) in inventory {
            let stored = classify_lane_storage(&lane, tip, configuration, state)?;
            if matches!(stored, ClassifiedLaneStorage::Lane { .. }) {
                restored.insert(lane.clone(), restore_lane_state(mutation.as_ref(), &lane, stored, context).await?);
            }
        }
        Ok(restored)
    }.await;
    mutation.end(context).await;
    result
}

pub async fn restore_lane(session: &dyn Session, lane: &str, context: &Context) -> Result<LaneState, SessionError> {
    let mutation = session.begin_mutation(context).await?;
    let result = async {
        let stored = read_lane_storage(mutation.as_ref(), lane, context).await?;
        restore_lane_state(mutation.as_ref(), lane, stored, context).await
    }.await;
    mutation.end(context).await;
    result
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, SessionError> {
    serde_json::from_value(value).map_err(|e| session_invariant_error(e.to_string()))
}

pub fn decode_operation_state(value: serde_json::Value) -> Result<OperationState, SessionError> {
    use crate::harness::session::types::OperationMarker;
    let marker: OperationMarker = decode(value.get("at").cloned().ok_or_else(|| session_invariant_error("Operation state is missing at"))?)?;
    Ok(match marker {
        OperationMarker::Starting => OperationState::Starting(decode(value)?),
        OperationMarker::Checkpoint => OperationState::Checkpoint(decode(value)?),
        OperationMarker::AssistantReady => OperationState::AssistantReady(decode(value)?),
        OperationMarker::AssistantEffectPending => OperationState::AssistantEffectPending(decode(value)?),
        OperationMarker::AssistantRetryWait => OperationState::AssistantRetryWait(decode(value)?),
        OperationMarker::Tools => OperationState::Tools(decode(value)?),
        OperationMarker::DeferredSuspended => OperationState::DeferredSuspended(decode(value)?),
        OperationMarker::DeferredEffectPending => OperationState::DeferredEffectPending(decode(value)?),
        OperationMarker::SummaryDeciding => OperationState::SummaryDeciding(decode(value)?),
        OperationMarker::SummaryReady => OperationState::SummaryReady(decode(value)?),
        OperationMarker::SummaryEffectPending => OperationState::SummaryEffectPending(decode(value)?),
        OperationMarker::SummaryRetryWait => OperationState::SummaryRetryWait(decode(value)?),
        OperationMarker::NavigationReadyToCommit => OperationState::NavigationReadyToCommit(decode(value)?),
    })
}

pub async fn restore_lane_state(reader: &dyn SessionReader, lane: &str, stored: ClassifiedLaneStorage, context: &Context) -> Result<LaneState, SessionError> {
    let quoted = serde_json::Value::String(lane.to_owned());
    let (tip, configuration, stored_state) = match stored {
        ClassifiedLaneStorage::Absent => return Err(session_invariant_error(format!("Lane {quoted} is missing branch.tip"))),
        ClassifiedLaneStorage::Branch { .. } => return Err(session_invariant_error(format!("Lane {quoted} is missing lane.config"))),
        ClassifiedLaneStorage::Lane { tip, configuration, lane_state } => (tip, configuration, lane_state),
    };
    let durable: crate::harness::session::types::LaneState = decode(stored_state.value)?;
    let operation = match &durable.current_operation_id {
        None => None,
        Some(id) => {
            let (meta_address, state_address) = (values::operation_meta(id), values::operation_state(id));
            let (meta, state) = tokio::try_join!(reader.get_value(&meta_address, context), reader.get_value(&state_address, context))?;
            let meta: OperationMeta = decode(meta.ok_or_else(|| session_invariant_error(format!("Operation {id} is missing op.meta")))?.value)?;
            let state = decode_operation_state(state.ok_or_else(|| session_invariant_error(format!("Operation {id} is missing op.state")))?.value)?;
            if meta.operation_id != *id { return Err(session_invariant_error(format!("Operation {id} metadata names operation {}", serde_json::Value::String(meta.operation_id)))); }
            if meta.lane != lane { return Err(session_invariant_error(format!("Operation {id} belongs to lane {}, not {quoted}", serde_json::Value::String(meta.lane)))); }
            if !state_matches_intent(&meta.intent, &state) {
                let kind = serde_json::to_value(meta.intent.kind()).map_err(|e| session_invariant_error(e.to_string()))?;
                let at = serde_json::to_value(state.at()).map_err(|e| session_invariant_error(e.to_string()))?;
                return Err(session_invariant_error(format!("Operation {id} intent {} does not match state {}", kind.as_str().unwrap_or_default(), at.as_str().unwrap_or_default())));
            }
            Some(Operation { meta, state })
        }
    };
    Ok(LaneState { tip_id: decode(tip.value)?, configuration: decode::<LaneConfiguration>(configuration.value)?, inbox: durable.inbox, last_operation_id: durable.last_operation_id, operation })
}
