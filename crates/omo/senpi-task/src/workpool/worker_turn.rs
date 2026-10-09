use super::store::WorkpoolStore;
use super::types::{PoolId, WorkpoolError, WorkpoolErrorCode, WorkpoolItem, WorkpoolItemStatus, WorkpoolDelivery, WorkpoolDeliveryPhase};

pub type WorkerTurn = (PoolId, u32, String, u32);
pub const WORKPOOL_TURN_MESSAGE: &str = "Process the admitted workpool assignments above. Use workpool yield for each key; do not spawn or schedule tasks.";

pub fn belongs_to_turn(item: &WorkpoolItem, turn: &WorkerTurn) -> bool { item.binding.as_ref().is_some_and(|binding| binding.task_id == turn.2 && binding.run_epoch == turn.3 && binding.generation == turn.1) }

pub fn append_assigned_messages(store: &WorkpoolStore, turn: &WorkerTurn) -> Result<(), WorkpoolError> {
    store.mutate(&turn.0, |pool| {
        if pool.generation != turn.1 || pool.status == super::types::WorkpoolStatus::Cancelled { return pool.clone(); }
        let mut next = pool.clone();
        for item in &mut next.items { if belongs_to_turn(item, turn) && item.status == WorkpoolItemStatus::Assigned && item.delivery.as_ref().is_some_and(|delivery| delivery.phase == WorkpoolDeliveryPhase::Queued) { item.delivery = Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Dispatching, message_sha256: None }); } }
        next
    })?;
    Ok(())
}

pub fn begin_worker_turn(store: &WorkpoolStore, turn: &WorkerTurn, prefix: Option<&str>) -> Result<Option<String>, WorkpoolError> {
    let pool = store.load(&turn.0)?;
    if pool.generation != turn.1 || pool.status == super::types::WorkpoolStatus::Cancelled { return Ok(None); }
    if !pool.items.iter().any(|item| belongs_to_turn(item, turn) && item.status == WorkpoolItemStatus::Assigned) { return Ok(None); }
    let message = prefix.map_or_else(|| WORKPOOL_TURN_MESSAGE.to_string(), |prefix| format!("{prefix}\n\n{WORKPOOL_TURN_MESSAGE}"));
    append_assigned_messages(store, turn)?;
    Ok(Some(message))
}

pub fn acknowledge_worker_turn(store: &WorkpoolStore, turn: &WorkerTurn) -> Result<(), WorkpoolError> { store.mutate(&turn.0, |pool| { let mut next = pool.clone(); for item in &mut next.items { if belongs_to_turn(item, turn) && item.delivery.as_ref().is_some_and(|delivery| delivery.phase == WorkpoolDeliveryPhase::Dispatching) { item.delivery = Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Acknowledged, message_sha256: None }); } } next })?; Ok(()) }

#[cfg(test)] mod tests { use super::*; #[test] fn turn_message_is_stable() { assert!(WORKPOOL_TURN_MESSAGE.contains("workpool yield")); } }
