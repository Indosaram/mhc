use serde_json::Value;

use crate::store::{PersistedTaskEvent, TaskRecordStore};

use super::store::WorkpoolStore;
use super::types::{PoolId, WorkpoolError, WorkpoolErrorCode, WorkpoolEvent, WorkpoolEventKind, WorkpoolItemStatus, WorkpoolYieldValue};
use super::yield_::{apply_yield, refuse_yield};
use super::yield_event::{read_yield_events, yield_digest, WorkpoolYieldEvent};

const INVALID_RESULT: &str = "Expected one key and either JSON data or a typed error.";

pub fn create_workpool_yield_capability(
    store: WorkpoolStore,
    tasks: TaskRecordStore,
    task_id: impl Into<String>,
    run_epoch: u32,
    emit: Option<WorkpoolEventSink>,
) -> impl Fn(Value) -> Result<(PoolId, u32, Vec<WorkpoolYieldReceipt>), WorkpoolError> {
    let task_id = task_id.into();
    let bound_pool: std::sync::Mutex<Option<PoolId>> = std::sync::Mutex::new(None);
    move |value| {
        let envelope = value.as_object().ok_or_else(invalid_envelope)?;
        if envelope.len() != 2 || envelope.get("op").and_then(Value::as_str) != Some("yield") {
            return Err(invalid_envelope());
        }
        let results = envelope.get("results").and_then(Value::as_array).ok_or_else(invalid_envelope)?;
        let is_bound = |candidate: &super::types::WorkpoolRecord| candidate.items.iter().any(|item|
            item.binding.as_ref().is_some_and(|binding| binding.task_id == task_id));
        // A worker task is only ever bound inside the pool that admitted it, so the pool found by
        // the first full scan is reused while it still holds a binding for this task.
        let cached = bound_pool.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let cached = cached.and_then(|pool_id| store.load(&pool_id).ok()).filter(&is_bound);
        let pool = match cached {
            Some(pool) => pool,
            None => store.list_result()?.into_iter().find(&is_bound)
                .ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::WorkerUnassigned, "Worker has no pool assignment."))?,
        };
        *bound_pool.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pool.pool_id.clone());
        let task = tasks.load(&task_id).map_err(store_error)?;
        if pool.status == super::types::WorkpoolStatus::Cancelled
            || task.as_ref().is_none_or(|task| task.notification.run_epoch != i64::from(run_epoch))
            || !pool.items.iter().any(|item| item.binding.as_ref().is_some_and(|binding|
                binding.task_id == task_id && binding.run_epoch == run_epoch && binding.generation == pool.generation
            ))
        {
            return Err(stale_assignment());
        }
        let turn = (pool.pool_id.clone(), pool.generation, task_id.clone(), run_epoch);
        let mut receipts = Vec::with_capacity(results.len());
        let mut events: Option<YieldEventCache> = None;
        for raw in results {
            let result: WorkpoolYieldValue = match parse_yield_result(raw) {
                Ok(result) => result,
                Err(()) => {
                    receipts.push(WorkpoolYieldReceipt::Refused(refuse_yield("invalid_input", INVALID_RESULT, None)));
                    continue;
                }
            };
            let receipt = accept_yield(&store, &tasks, &turn, result, &mut events)?;
            if let WorkpoolYieldReceipt::Accepted { item_id, .. } = &receipt && let Some(emit) = &emit {
                    emit(WorkpoolEvent {
                        pool_id: turn.0.clone(), kind: WorkpoolEventKind::ItemResult,
                        item_id: Some(item_id.clone()), task_id: Some(task_id.clone()),
                        run_epoch: Some(run_epoch), error: None,
                    });
            }
            receipts.push(receipt);
        }
        Ok((pool.pool_id, pool.generation, receipts))
    }
}

/// Durable yield events read once per yield call, keyed by the log length they reflect. Yield
/// events are only appended under the pool store gate, so an unchanged length means no new ones.
type YieldEventCache = (Option<u64>, Vec<WorkpoolYieldEvent>);

fn yield_log_len(tasks: &TaskRecordStore, task_id: &str) -> Option<u64> {
    std::fs::metadata(tasks.state_dir().join("logs").join(format!("{task_id}.jsonl"))).ok().map(|metadata| metadata.len())
}

fn accept_yield(
    pools: &WorkpoolStore,
    tasks: &TaskRecordStore,
    turn: &(PoolId, u32, String, u32),
    result: WorkpoolYieldValue,
    cache: &mut Option<YieldEventCache>,
) -> Result<WorkpoolYieldReceipt, WorkpoolError> {
    let mut receipt = WorkpoolYieldReceipt::Refused(refuse_yield(
        "stale_assignment", "Key is not assigned to this worker turn.", Some(result.key().to_string()),
    ));
    pools.mutate_result(&turn.0, |pool| {
        if pool.generation != turn.1 || pool.status == super::types::WorkpoolStatus::Cancelled {
            return Ok(pool.clone());
        }
        let task = tasks.load(&turn.2).map_err(store_error)?;
        if !task_epoch_matches(task.as_ref(), turn.3) {
            return Ok(pool.clone());
        }
        let Some(index) = pool.items.iter().position(|item|
            item.key == result.key() && item.binding.as_ref().is_some_and(|binding|
                binding.task_id == turn.2 && binding.run_epoch == turn.3 && binding.generation == turn.1
            )
        ) else { return Ok(pool.clone()) };
        let item = &pool.items[index];
        let sha256 = yield_digest(&result);
        if let Some(existing) = &item.yield_sha256 {
            receipt = if existing == &sha256 {
                WorkpoolYieldReceipt::Duplicate { key: item.key.clone(), item_id: item.item_id.clone() }
            } else {
                WorkpoolYieldReceipt::Refused(refuse_yield("yield_conflict", "This item already has a different terminal yield.", Some(item.key.clone())))
            };
            return Ok(pool.clone());
        }
        let uncertain = item.status == WorkpoolItemStatus::Error && item.error.as_ref().is_some_and(|error| error.code == "delivery_uncertain");
        if (!uncertain && item.status != WorkpoolItemStatus::Assigned)
            || item.delivery.as_ref().is_some_and(|delivery| delivery.phase == super::types::WorkpoolDeliveryPhase::Queued)
        { return Ok(pool.clone()) }

        let log_len = yield_log_len(tasks, &turn.2);
        if cache.as_ref().is_none_or(|(cached_len, _)| *cached_len != log_len) {
            let read = read_yield_events(&*tasks, &turn.0, &turn.2, turn.3, turn.1).map_err(|error|
                WorkpoolError::new(WorkpoolErrorCode::StoreCorrupt, format!("Could not read durable yield events: {error}"))
            )?;
            *cache = Some((log_len, read));
        }
        let events = cache.as_ref().map_or(&[][..], |(_, events)| events.as_slice());
        if let Some(prior) = events.iter().find(|event| event.item_id == item.item_id && event.result.key() == item.key) {
            receipt = if prior.sha256 == sha256 {
                WorkpoolYieldReceipt::Duplicate { key: item.key.clone(), item_id: item.item_id.clone() }
            } else {
                WorkpoolYieldReceipt::Refused(refuse_yield("yield_conflict", "This item already has a different durable yield event.", Some(item.key.clone())))
            };
            return Ok(pool.clone());
        }
        let durable = WorkpoolYieldEvent {
            pool_id: turn.0.clone(), generation: turn.1, task_id: turn.2.clone(),
            run_epoch: turn.3, item_id: item.item_id.clone(), result: result.clone(), sha256,
        };
        let payload = serde_json::to_value(&durable).map_err(|error| WorkpoolError::new(WorkpoolErrorCode::StoreCorrupt, error.to_string()))?;
        tasks.append_event(&turn.2, &PersistedTaskEvent { event_type: "workpool_yield".into(), payload }).map_err(store_error)?;
        if let Some((cached_len, events)) = cache.as_mut() {
            *cached_len = yield_log_len(tasks, &turn.2);
            events.push(durable);
        }
        receipt = WorkpoolYieldReceipt::Accepted { key: item.key.clone(), item_id: item.item_id.clone() };
        if uncertain { return Ok(pool.clone()) }
        let mut next = pool.clone();
        next.items[index] = apply_yield(next.items[index].clone(), &result);
        Ok(next)
    })?;
    Ok(receipt)
}

fn invalid_envelope() -> WorkpoolError { WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments.") }
fn stale_assignment() -> WorkpoolError { WorkpoolError::new(WorkpoolErrorCode::StaleAssignment, "Yield does not identify this worker's current assignment.") }
fn store_error(error: impl std::fmt::Display) -> WorkpoolError { WorkpoolError::new(WorkpoolErrorCode::StoreCorrupt, error.to_string()) }
fn task_epoch_matches(task: Option<&crate::state::TaskRecord>, run_epoch: u32) -> bool {
    task.is_some_and(|task| task.notification.run_epoch == i64::from(run_epoch))
}

fn parse_yield_result(value: &Value) -> Result<WorkpoolYieldValue, ()> {
    let object = value.as_object().ok_or(())?;
    let key = object.get("key").and_then(Value::as_str).ok_or(())?;
    if key.trim().is_empty() { return Err(()); }
    let result = match (object.get("data"), object.get("error")) {
        (Some(data), None) => {
            if object.len() != 2 { return Err(()); }
            WorkpoolYieldValue::Data { key: key.trim().to_string(), data: data.clone() }
        }
        (None, Some(error)) => {
            if object.len() != 2 { return Err(()); }
            let fields = error.as_object().ok_or(())?;
            if fields.len() != 2 { return Err(()); }
            let code = fields.get("code").and_then(Value::as_str).ok_or(())?;
            let message = fields.get("message").and_then(Value::as_str).ok_or(())?;
            if code.trim().is_empty() || message.trim().is_empty() { return Err(()); }
            WorkpoolYieldValue::Error {
                key: key.trim().to_string(),
                error: super::types::WorkpoolYieldError { code: code.trim().to_string(), message: message.trim().to_string() },
            }
        }
        _ => return Err(()),
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TaskRecordInput, TaskStatus};

    #[test]
    fn strict_result_schema_rejects_extra_fields_and_data_error_union() {
        for value in [
            serde_json::json!({"key":"a","data":null,"extra":true}),
            serde_json::json!({"key":"a","data":null,"error":{"code":"e","message":"m"}}),
            serde_json::json!({"key":"a","error":{"code":"e","message":"m","extra":true}}),
            serde_json::json!({"key":" ","data":null}),
            serde_json::json!({"key":"a","error":{"code":" ","message":"m"}}),
            serde_json::json!({"key":"a","error":{"code":"e","message":" "}}),
        ] {
            assert!(parse_yield_result(&value).is_err());
        }
    }

    #[test]
    fn strict_result_schema_trims_nonempty_strings() {
        let result = parse_yield_result(&serde_json::json!({
            "key":" a ","error":{"code":" stale ","message":" old "}
        })).expect("valid typed error");
        assert_eq!(result, WorkpoolYieldValue::Error {
            key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "stale".into(), message: "old".into() },
        });
    }

    #[test]
    fn completed_task_with_matching_epoch_passes_epoch_gate() {
        let mut record = crate::state::create_task_record(TaskRecordInput {
            parent_session_id: "parent".into(), root_session_id: "root".into(),
            execution_mode: "process".into(), model: "fixture/model".into(), ..Default::default()
        }, Some(0)).expect("record");
        record.status = TaskStatus::Completed;
        record.notification.run_epoch = 4;
        assert!(task_epoch_matches(Some(&record), 4));
        assert!(!task_epoch_matches(Some(&record), 5));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkpoolYieldReceipt {
    Accepted { key: String, item_id: String },
    Duplicate { key: String, item_id: String },
    Refused(super::yield_::YieldRefusal),
}

impl WorkpoolYieldValue { pub(crate) fn key(&self) -> &str { match self { Self::Data { key, .. } | Self::Error { key, .. } => key } } }

pub fn assignment_is_current(store: &WorkpoolStore, task_id: &str, run_epoch: u32) -> Result<(), WorkpoolError> {
    let assigned = store.list().into_iter().any(|pool| pool.items.iter().any(|item|
        item.status == WorkpoolItemStatus::Assigned && item.binding.as_ref().is_some_and(|binding|
            binding.task_id == task_id && binding.run_epoch == run_epoch && binding.generation == pool.generation
        )
    ));
    if assigned { Ok(()) } else { Err(stale_assignment()) }
}

pub type WorkpoolEventSink = Box<dyn Fn(WorkpoolEvent) + Send + Sync>;
