use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agents::interaction_policy_for_agent;
use crate::state::{DeliverAs, PendingSteeringEntry, PendingSteeringWorkpoolMetadata};
use crate::store::TaskRecordStore;

use super::store::WorkpoolStore;
use super::types::{
    PoolId, WorkpoolDelivery, WorkpoolDeliveryPhase, WorkpoolError, WorkpoolErrorCode,
    WorkpoolItem, WorkpoolItemStatus, WorkpoolStatus,
};

pub type WorkerTurn = (PoolId, u32, String, u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTurnPrompt {
    pub entries: Vec<PendingSteeringEntry>,
    pub message: String,
}

pub const WORKPOOL_TURN_MESSAGE: &str = "Process the admitted workpool assignments above. Use workpool yield for each key; do not spawn or schedule tasks.";

pub fn belongs_to_turn(item: &WorkpoolItem, turn: &WorkerTurn) -> bool {
    item.binding.as_ref().is_some_and(|binding| {
        binding.task_id == turn.2
            && binding.run_epoch == turn.3
            && binding.generation == turn.1
    })
}

pub fn append_assigned_messages(
    pools: &WorkpoolStore,
    tasks: &TaskRecordStore,
    turn: &WorkerTurn,
) -> Result<(), WorkpoolError> {
    pools.mutate_result(&turn.0, |pool| {
        if pool.generation != turn.1 || pool.status == WorkpoolStatus::Cancelled {
            return Ok(pool.clone());
        }
        let assigned: Vec<_> = pool
            .items
            .iter()
            .filter(|item| {
                belongs_to_turn(item, turn)
                    && item.status == WorkpoolItemStatus::Assigned
                    && item.delivery.as_ref().is_some_and(|delivery| {
                        delivery.phase == WorkpoolDeliveryPhase::Queued
                    })
            })
            .collect();
        if assigned.is_empty() {
            return Ok(pool.clone());
        }
        let mut stale = false;
        let updated = tasks.mutate(&turn.2, |record| {
            if record.parent_session_id != pool.parent_session_id
                || record.killed == Some(true)
                || record.notification.run_epoch > i64::from(turn.3)
            {
                stale = true;
                return record.clone();
            }
            let mut queue = record.pending_steering.clone().unwrap_or_default();
            for item in assigned {
            if queue.iter().any(|entry| entry.id == item.item_id) {
                continue;
            }
            queue.push(PendingSteeringEntry {
                id: item.item_id.clone(),
                deliver_as: DeliverAs::FollowUp,
                message: json!({
                    "pool_id": pool.pool_id,
                    "generation": pool.generation,
                    "items": [{ "item_id": item.item_id, "key": item.key, "input": item.input }]
                })
                .to_string(),
                workpool: Some(PendingSteeringWorkpoolMetadata {
                    pool_id: pool.pool_id.clone(),
                    generation: pool.generation,
                    item_id: item.item_id.clone(),
                    key: item.key.clone(),
                    run_epoch: turn.3,
                }),
            });
            }
            crate::state::TaskRecord {
                pending_steering: Some(queue),
                ..record.clone()
            }
        })
            .map_err(task_store_error)?;
        if stale {
            return Err(WorkpoolError::new(WorkpoolErrorCode::StaleAssignment, "Task identity changed before queue append."));
        }
        if updated.is_none() {
            return Err(WorkpoolError::new(
                WorkpoolErrorCode::WorkerNotContinuable,
                "Assigned task record is missing.",
            ));
        }
        Ok(pool.clone())
    })?;
    Ok(())
}

pub fn begin_worker_turn(
    pools: &WorkpoolStore,
    tasks: &TaskRecordStore,
    turn: &WorkerTurn,
    prefix: Option<&str>,
) -> Result<Option<WorkerTurnPrompt>, WorkpoolError> {
    let mut captured = None;
    pools.mutate_result(&turn.0, |pool| {
        if pool.generation != turn.1 || pool.status == WorkpoolStatus::Cancelled {
            return Ok(pool.clone());
        }
        let assigned: Vec<_> = pool
            .items
            .iter()
            .filter(|item| belongs_to_turn(item, turn) && item.status == WorkpoolItemStatus::Assigned)
            .collect();
        if assigned.is_empty()
            || assigned.iter().any(|item| {
                item.delivery.as_ref().is_none_or(|delivery| {
                    delivery.phase != WorkpoolDeliveryPhase::Queued
                })
            })
        {
            return Ok(pool.clone());
        }
        let record = tasks
            .load(&turn.2)
            .map_err(task_store_error)?
            .ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::WorkerNotContinuable, "Assigned task record is missing."))?;
        let entries = record.pending_steering.unwrap_or_default();
        if assigned.iter().any(|item| !entries.iter().any(|entry| entry.id == item.item_id)) {
            return Err(WorkpoolError::new(WorkpoolErrorCode::StoreCorrupt, "Assignment has no durable steering entry."));
        }
        let canonical = interaction_policy_for_agent(record.agent_type.as_deref().unwrap_or_default())
            .is_some_and(|policy| policy.prompt_contract == "plan-review");
        let message = if prefix.is_none() {
            format!("{}\n\n{}", entries.iter().map(|entry| entry.message.as_str()).collect::<Vec<_>>().join("\n\n"), WORKPOOL_TURN_MESSAGE)
        } else if canonical {
            prefix.unwrap_or_default().to_string()
        } else {
            format!("{}\n\n{}", prefix.unwrap_or_default(), entries.iter().map(|entry| entry.message.as_str()).collect::<Vec<_>>().join("\n\n"))
        };
        let digest = message_sha256(&message);
        let mut next = pool.clone();
        for item in &mut next.items {
            if belongs_to_turn(item, turn) && item.status == WorkpoolItemStatus::Assigned {
                item.delivery = Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Dispatching, message_sha256: Some(digest.clone()), entry_ids: Some(entries.iter().map(|entry| entry.id.clone()).collect()) });
            }
        }
        captured = Some(WorkerTurnPrompt { entries, message });
        Ok(next)
    })?;
    Ok(captured)
}

pub fn acknowledge_worker_turn(
    pools: &WorkpoolStore,
    tasks: &TaskRecordStore,
    turn: &WorkerTurn,
    entries: &[PendingSteeringEntry],
) -> Result<(), WorkpoolError> {
    acknowledge_worker_turn_with_entries(pools, tasks, turn, Some(entries))
}

pub fn acknowledge_worker_turn_with_entries(
    pools: &WorkpoolStore, tasks: &TaskRecordStore, turn: &WorkerTurn,
    entries: Option<&[PendingSteeringEntry]>,
) -> Result<(), WorkpoolError> {
    let ids: std::collections::BTreeSet<String> = if let Some(entries) = entries {
        entries.iter().map(|entry| entry.id.clone()).collect()
    } else {
        pools.load(&turn.0)?.items.iter().find(|item| belongs_to_turn(item, turn))
            .and_then(|item| item.delivery.as_ref()).and_then(|delivery| delivery.entry_ids.clone())
            .unwrap_or_default().into_iter().collect()
    };
    if !ids.is_empty() {
        tasks
            .mutate(&turn.2, |record| {
                if record.notification.run_epoch != i64::from(turn.3) {
                    return record.clone();
                }
                let pending = record.pending_steering.as_deref().unwrap_or_default();
                let remaining: Vec<_> = pending
                    .iter()
                    .filter(|entry| !ids.contains(entry.id.as_str()))
                    .cloned()
                    .collect();
                crate::state::TaskRecord {
                    pending_steering: (!remaining.is_empty()).then_some(remaining),
                    ..record.clone()
                }
            })
            .map_err(task_store_error)?;
    }
    pools.mutate(&turn.0, |pool| {
        if pool.generation != turn.1 {
            return pool.clone();
        }
        let mut next = pool.clone();
        for item in &mut next.items {
            if belongs_to_turn(item, turn)
                && item.delivery.as_ref().is_some_and(|delivery| delivery.phase == WorkpoolDeliveryPhase::Dispatching)
            {
                item.delivery = Some(WorkpoolDelivery {
                    phase: WorkpoolDeliveryPhase::Acknowledged,
                    message_sha256: item.delivery.as_ref().and_then(|delivery| delivery.message_sha256.clone()),
                    entry_ids: item.delivery.as_ref().and_then(|delivery| delivery.entry_ids.clone()),
                });
            }
        }
        next
    })?;
    Ok(())
}

fn task_store_error(error: crate::store::StoreError) -> WorkpoolError {
    WorkpoolError::new(WorkpoolErrorCode::StoreCorrupt, error.to_string())
}

fn message_sha256(message: &str) -> String {
    Sha256::digest(message.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_assignment_is_idempotent_and_acknowledges_only_captured_entries() {
        use crate::state::{TaskRecordInput, create_task_record};
        use crate::store::StateDirConfig;
        use super::super::types::{WorkpoolAgent, WorkpoolBinding, WorkpoolCaller, WorkpoolCreate, WorkpoolMode, WorkpoolSpec};

        let root = tempfile::tempdir().expect("root");
        let tasks = TaskRecordStore::new(&StateDirConfig { project_dir: root.path().into(), ..Default::default() });
        let pools = WorkpoolStore::new(tasks.state_dir());
        let task = create_task_record(TaskRecordInput { parent_session_id: "parent".into(), root_session_id: "parent".into(), execution_mode: "rpc".into(), model: "test/model".into(), ..Default::default() }, None).expect("task");
        tasks.save(&task).expect("save task");
        let caller = WorkpoolCaller { session_id: "parent".into(), root_session_id: "parent".into(), depth: 0, cwd: root.path().to_string_lossy().into_owned() };
        let request = WorkpoolCreate { name: "batch".into(), agent: WorkpoolAgent::Category { category: "quick".into(), model: None, prompt: "process".into() }, mode: None, tools: None };
        let pool = pools.create(&caller, &request, WorkpoolMode::Fresh, WorkpoolSpec { start: Default::default(), plan: Default::default() }).expect("pool");
        let turn = (pool.pool_id.clone(), pool.generation, task.task_id.clone(), 0);
        pools.mutate(&pool.pool_id, |pool| {
            let mut next = pool.clone();
            next.items.push(WorkpoolItem { item_id: "assignment".into(), key: "key".into(), input: json!({"value":7}), status: WorkpoolItemStatus::Assigned,
                binding: Some(WorkpoolBinding { task_id: task.task_id.clone(), run_epoch: 0, generation: pool.generation }),
                delivery: Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Queued, message_sha256: None, entry_ids: None }), data: None, error: None, yield_sha256: None });
            next
        }).expect("assignment");
        append_assigned_messages(&pools, &tasks, &turn).expect("append");
        append_assigned_messages(&pools, &tasks, &turn).expect("idempotent append");
        let queued = tasks.load(&task.task_id).expect("load").expect("task").pending_steering.expect("queue");
        assert_eq!(queued.len(), 1);
        let metadata = queued[0].workpool.as_ref().expect("persisted metadata");
        assert_eq!(metadata.pool_id, pool.pool_id);
        assert_eq!(metadata.item_id, "assignment");
        let payload: serde_json::Value = serde_json::from_str(&queued[0].message).expect("payload");
        assert_eq!(payload["items"][0]["input"], json!({"value":7}));
        let captured = begin_worker_turn(&pools, &tasks, &turn, None).expect("begin").expect("captured");
        assert_eq!(captured.entries, queued);
        assert!(begin_worker_turn(&pools, &tasks, &turn, None).expect("second begin").is_none());
        tasks.mutate(&task.task_id, |task| {
            let mut next = task.clone();
            next.pending_steering.as_mut().expect("queue").push(PendingSteeringEntry::new("later".into(), "later input".into(), DeliverAs::FollowUp));
            next
        }).expect("later entry");
        acknowledge_worker_turn_with_entries(&pools, &tasks, &turn, Some(&[])).expect("empty explicit ack");
        assert_eq!(tasks.load(&task.task_id).expect("task").expect("task").pending_steering.as_ref().expect("queue").len(), 2);
        acknowledge_worker_turn_with_entries(&pools, &tasks, &turn, None).expect("durable fallback ack");
        let remaining = tasks.load(&task.task_id).expect("load").expect("task").pending_steering.expect("remaining");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "later");
        let delivery = pools.load(&pool.pool_id).expect("pool").items[0].delivery.clone().expect("delivery");
        assert_eq!(delivery.phase, WorkpoolDeliveryPhase::Acknowledged);
        assert_eq!(delivery.message_sha256, Some(message_sha256(&captured.message)));
        tasks.mutate(&task.task_id, |task| {
            let mut next = task.clone();
            next.notification.run_epoch = 1;
            next
        }).expect("advance epoch");
        pools.mutate(&pool.pool_id, |pool| {
            let mut next = pool.clone();
            next.items[0].delivery = Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Queued, message_sha256: None, entry_ids: None });
            next
        }).expect("queue old assignment");
        let error = append_assigned_messages(&pools, &tasks, &turn).expect_err("old epoch fenced");
        assert_eq!(error.code, WorkpoolErrorCode::StaleAssignment);
        assert_eq!(tasks.load(&task.task_id).expect("load").expect("task").pending_steering.expect("preserved queue"), remaining);
    }
}
