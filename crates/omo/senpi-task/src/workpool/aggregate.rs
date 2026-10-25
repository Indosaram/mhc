use super::types::{Json, WorkpoolItemStatus, WorkpoolRecord, WorkpoolStatus};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum WorkpoolAggregateResult {
    Data { key: String, data: Json },
    Error { key: String, error: WorkpoolAggregateError },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkpoolAggregateError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkpoolAggregateMessage {
    pub pool_id: String,
    pub generation: u32,
    pub results: Vec<WorkpoolAggregateResult>,
}

pub struct WorkpoolAggregateReceipts {
    pub ack: Arc<dyn Fn() + Send + Sync + 'static>,
    pub fail: Arc<dyn Fn(Option<String>) + Send + Sync + 'static>,
}

pub trait WorkpoolAggregatePort: Send + Sync {
    fn enqueue(&self, message: WorkpoolAggregateMessage, receipts: WorkpoolAggregateReceipts) -> Result<(), String>;
}

impl<F> WorkpoolAggregatePort for F
where
    F: Fn(WorkpoolAggregateMessage, WorkpoolAggregateReceipts) -> Result<(), String> + Send + Sync,
{
    fn enqueue(&self, message: WorkpoolAggregateMessage, receipts: WorkpoolAggregateReceipts) -> Result<(), String> {
        self(message, receipts)
    }
}

const TERMINAL: &[WorkpoolItemStatus] = &[WorkpoolItemStatus::Completed, WorkpoolItemStatus::Error, WorkpoolItemStatus::Cancelled];

pub fn pool_work_is_finished(pool: &WorkpoolRecord) -> bool {
    pool.status != WorkpoolStatus::Open
        && pool.items.iter().all(|item| TERMINAL.contains(&item.status))
        && pool.hold.is_none()
        && pool.workers.iter().all(|worker| worker.status == super::types::WorkpoolWorkerStatus::Idle)
}

pub fn aggregate_results(pool: &WorkpoolRecord) -> Option<Vec<WorkpoolAggregateResult>> {
    if !matches!(pool.status, WorkpoolStatus::Closing | WorkpoolStatus::Cancelled) { return None; }
    if pool.aggregate.as_ref().is_some_and(|aggregate| aggregate.delivered && aggregate.generation == pool.generation) { return None; }
    if pool.items.iter().any(|item| !TERMINAL.contains(&item.status)) { return None; }
    Some(pool.items.iter().map(aggregate_item_result).collect())
}

pub fn deliver_aggregate(
    pool: &WorkpoolRecord,
    port: Option<&dyn WorkpoolAggregatePort>,
    persist: Arc<dyn Fn(WorkpoolAggregateState) + Send + Sync + 'static>,
    on_failure: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
) -> bool {
    let Some(results) = aggregate_results(pool) else { return false; };
    let Some(port) = port else { return false; };

    persist(WorkpoolAggregateState { delivered: false, accepted: true });
    let message = WorkpoolAggregateMessage { pool_id: pool.pool_id.clone(), generation: pool.generation, results };
    let ack_persist = Arc::clone(&persist);
    let fail_persist = Arc::clone(&persist);
    let fail_report = on_failure.clone();
    let receipts = WorkpoolAggregateReceipts {
        ack: Arc::new(move || ack_persist(WorkpoolAggregateState { delivered: true, accepted: true })),
        fail: Arc::new(move |error| {
            fail_persist(WorkpoolAggregateState { delivered: false, accepted: false });
            if let (Some(on_failure), Some(error)) = (&fail_report, error) { on_failure(error); }
        }),
    };
    match port.enqueue(message, receipts) {
        Ok(()) => true,
        Err(error) => {
            persist(WorkpoolAggregateState { delivered: false, accepted: false });
            if let Some(on_failure) = &on_failure { on_failure(error); }
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkpoolAggregateState {
    pub delivered: bool,
    pub accepted: bool,
}

pub fn aggregate_item_result(item: &super::types::WorkpoolItem) -> WorkpoolAggregateResult {
    match item.status {
        WorkpoolItemStatus::Completed => WorkpoolAggregateResult::Data { key: item.key.clone(), data: item.data.clone().unwrap_or(Json::Null) },
        _ => { let error = item.error.clone().unwrap_or_else(|| super::types::WorkpoolYieldError { code: "cancelled".into(), message: "Pool item did not complete.".into() }); WorkpoolAggregateResult::Error { key: item.key.clone(), error: WorkpoolAggregateError { code: error.code, message: error.message } } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::manager::types::{ManagerStartSpec, ResolvedChildPlan};
    use super::super::types::{WorkpoolAgent, WorkpoolAggregate, WorkpoolItem, WorkpoolWorker, WorkpoolWorkerStatus, WorkpoolMode, WorkpoolSpec};
    use std::sync::Mutex;

    fn pool(status: WorkpoolStatus, items: Vec<WorkpoolItem>, aggregate: Option<WorkpoolAggregate>) -> WorkpoolRecord {
        WorkpoolRecord { hold: None, version: 1, pool_id: "wp_test".into(), name: "pool".into(), parent_session_id: "parent".into(), root_session_id: "root".into(), generation: 1, revision: 0, mode: WorkpoolMode::Fresh, agent: WorkpoolAgent::Category { prompt: "p".into(), model: None, category: "quick".into() }, worker_spec: WorkpoolSpec { start: ManagerStartSpec::default(), plan: ResolvedChildPlan::default() }, status, kernel_tool_names: None, items, workers: vec![], aggregate }
    }

    fn item(key: &str, status: WorkpoolItemStatus) -> WorkpoolItem {
        WorkpoolItem { key: key.into(), input: json!(null), item_id: format!("wp_item_{key}"), status, binding: None, delivery: None, data: None, error: None, yield_sha256: None }
    }

    #[test]
    fn omitted_mode_defaults_to_keep_alive_contract() { assert_eq!(crate::workpool::default_mode::WORKPOOL_DEFAULT_MODE, WorkpoolMode::KeepAlive); }

    #[test]
    fn empty_closing_pool_delivers_one_empty_result() { assert_eq!(aggregate_results(&pool(WorkpoolStatus::Closing, vec![], None)), Some(vec![])); }

    #[test]
    fn terminal_items_preserve_input_order_and_data() {
        let mut first = item("b", WorkpoolItemStatus::Completed); first.data = Some(json!(2));
        let mut second = item("a", WorkpoolItemStatus::Completed); second.data = Some(json!(1));
        assert_eq!(aggregate_results(&pool(WorkpoolStatus::Closing, vec![first, second], None)), Some(vec![WorkpoolAggregateResult::Data { key: "b".into(), data: json!(2) }, WorkpoolAggregateResult::Data { key: "a".into(), data: json!(1) }]));
    }

    #[test]
    fn aggregate_message_serializes_with_pinned_wire_shape() {
        let message = WorkpoolAggregateMessage {
            pool_id: "wp_test".into(),
            generation: 3,
            results: vec![
                WorkpoolAggregateResult::Data { key: "ok".into(), data: json!(1) },
                WorkpoolAggregateResult::Error { key: "bad".into(), error: WorkpoolAggregateError { code: "failed".into(), message: "no".into() } },
            ],
        };
        assert_eq!(serde_json::to_value(message).expect("serialize aggregate message"), json!({
            "pool_id": "wp_test",
            "generation": 3,
            "results": [
                { "key": "ok", "data": 1 },
                { "key": "bad", "error": { "code": "failed", "message": "no" } },
            ],
        }));
    }

    #[test]
    fn delivered_same_generation_is_not_redelivered() { assert_eq!(aggregate_results(&pool(WorkpoolStatus::Closing, vec![], Some(WorkpoolAggregate { generation: 1, delivered: true, accepted: None }))), None); }

    #[test]
    fn nonterminal_items_keep_aggregate_pending() { assert_eq!(aggregate_results(&pool(WorkpoolStatus::Closing, vec![item("a", WorkpoolItemStatus::Assigned)], None)), None); }

    #[test]
    fn cancelled_item_becomes_keyed_cancelled_error() { assert_eq!(aggregate_item_result(&item("a", WorkpoolItemStatus::Cancelled)), WorkpoolAggregateResult::Error { key: "a".into(), error: WorkpoolAggregateError { code: "cancelled".into(), message: "Pool item did not complete.".into() } }); }

    #[test]
    fn failed_item_preserves_error_details() { let mut value = item("a", WorkpoolItemStatus::Error); value.error = Some(super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "stale".into() }); assert_eq!(aggregate_item_result(&value), WorkpoolAggregateResult::Error { key: "a".into(), error: WorkpoolAggregateError { code: "kernel_tool_stale".into(), message: "stale".into() } }); }

    #[test]
    fn delivery_persists_accepted_before_enqueue_and_delivered_on_ack() {
        let value = pool(WorkpoolStatus::Closing, vec![], None);
        let states = Arc::new(Mutex::new(Vec::new()));
        let messages = Arc::new(Mutex::new(Vec::new()));
        let delayed = Arc::new(Mutex::new(None));
        let port = {
            let messages = Arc::clone(&messages);
            let delayed = Arc::clone(&delayed);
            move |message: WorkpoolAggregateMessage, receipts: WorkpoolAggregateReceipts| {
                messages.lock().unwrap().push(message);
                *delayed.lock().unwrap() = Some(receipts);
                Ok(())
            }
        };
        let persist = { let states = Arc::clone(&states); Arc::new(move |state| states.lock().unwrap().push(state)) };
        assert!(deliver_aggregate(&value, Some(&port), persist, None));
        assert_eq!(*states.lock().unwrap(), vec![
            WorkpoolAggregateState { delivered: false, accepted: true },
        ]);
        assert_eq!(*messages.lock().unwrap(), vec![WorkpoolAggregateMessage { pool_id: "wp_test".into(), generation: 1, results: vec![] }]);
        let receipts = delayed.lock().unwrap().take().unwrap();
        (receipts.ack)();
        assert_eq!(states.lock().unwrap().last(), Some(&WorkpoolAggregateState { delivered: true, accepted: true }));
    }

    #[test]
    fn delivery_receipt_failure_clears_accepted_and_reports_supplied_error() {
        let value = pool(WorkpoolStatus::Closing, vec![], None);
        let states = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let delayed = Arc::new(Mutex::new(None));
        let port = { let delayed = Arc::clone(&delayed); move |_: WorkpoolAggregateMessage, receipts: WorkpoolAggregateReceipts| { *delayed.lock().unwrap() = Some(receipts); Ok(()) } };
        let persist = { let states = Arc::clone(&states); Arc::new(move |state| states.lock().unwrap().push(state)) };
        let on_failure = { let failures = Arc::clone(&failures); Arc::new(move |error| failures.lock().unwrap().push(error)) };
        assert!(deliver_aggregate(&value, Some(&port), persist, Some(on_failure)));
        (delayed.lock().unwrap().take().unwrap().fail)(Some("lost".into()));
        assert_eq!(*states.lock().unwrap(), vec![
            WorkpoolAggregateState { delivered: false, accepted: true },
            WorkpoolAggregateState { delivered: false, accepted: false },
        ]);
        assert_eq!(*failures.lock().unwrap(), vec!["lost"]);
    }

    #[test]
    fn synchronous_enqueue_error_clears_accepted_reports_and_returns_false() {
        let value = pool(WorkpoolStatus::Closing, vec![], None);
        let states = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let port = |_: WorkpoolAggregateMessage, _: WorkpoolAggregateReceipts| Err("notifier down".to_owned());
        let persist = { let states = Arc::clone(&states); Arc::new(move |state| states.lock().unwrap().push(state)) };
        let on_failure = { let failures = Arc::clone(&failures); Arc::new(move |error| failures.lock().unwrap().push(error)) };
        assert!(!deliver_aggregate(&value, Some(&port), persist, Some(on_failure)));
        assert_eq!(*states.lock().unwrap(), vec![
            WorkpoolAggregateState { delivered: false, accepted: true },
            WorkpoolAggregateState { delivered: false, accepted: false },
        ]);
        assert_eq!(*failures.lock().unwrap(), vec!["notifier down"]);
    }

    #[test]
    fn missing_port_or_pending_aggregate_is_not_accepted() {
        let value = pool(WorkpoolStatus::Open, vec![], None);
        let states = Arc::new(Mutex::new(Vec::new()));
        let persist = { let states = Arc::clone(&states); Arc::new(move |state| states.lock().unwrap().push(state)) };
        assert!(!deliver_aggregate(&value, None, persist, None));
        assert!(states.lock().unwrap().is_empty());
    }

    #[test]
    fn finished_requires_closed_pool_and_idle_workers() { let mut value = pool(WorkpoolStatus::Closing, vec![item("a", WorkpoolItemStatus::Completed)], None); value.workers.push(WorkpoolWorker { task_id: "st".into(), run_epoch: 0, status: WorkpoolWorkerStatus::Idle, completed_turns: 0, idle_since: 0 }); assert!(pool_work_is_finished(&value)); }

    #[test]
    fn busy_worker_prevents_finished_state() { let mut value = pool(WorkpoolStatus::Closing, vec![], None); value.workers.push(WorkpoolWorker { task_id: "st".into(), run_epoch: 0, status: WorkpoolWorkerStatus::Busy, completed_turns: 0, idle_since: 0 }); assert!(!pool_work_is_finished(&value)); }
}
