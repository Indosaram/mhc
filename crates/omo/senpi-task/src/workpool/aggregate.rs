use super::types::{Json, WorkpoolItemStatus, WorkpoolRecord, WorkpoolStatus};

#[derive(Debug, Clone, PartialEq)]
pub enum WorkpoolAggregateResult {
    Data { key: String, data: Json },
    Error { key: String, code: String, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkpoolAggregateMessage {
    pub pool_id: String,
    pub generation: u32,
    pub results: Vec<WorkpoolAggregateResult>,
}

pub struct WorkpoolAggregateReceipts<'a> {
    pub ack: Box<dyn FnOnce() + 'a>,
    pub fail: Box<dyn FnOnce(Option<String>) + 'a>,
}

pub trait WorkpoolAggregatePort {
    fn enqueue(&self, message: WorkpoolAggregateMessage, receipts: WorkpoolAggregateReceipts<'_>);
}

const TERMINAL: &[WorkpoolItemStatus] = &[WorkpoolItemStatus::Completed, WorkpoolItemStatus::Error, WorkpoolItemStatus::Cancelled];

pub fn pool_work_is_finished(pool: &WorkpoolRecord) -> bool {
    pool.status != WorkpoolStatus::Open
        && pool.items.iter().all(|item| TERMINAL.contains(&item.status))
        && pool.workers.iter().all(|worker| worker.status != super::types::WorkpoolWorkerStatus::Busy)
}

pub fn aggregate_results(pool: &WorkpoolRecord) -> Option<Vec<WorkpoolAggregateResult>> {
    if !matches!(pool.status, WorkpoolStatus::Closing | WorkpoolStatus::Cancelled) { return None; }
    if pool.aggregate.as_ref().is_some_and(|aggregate| aggregate.delivered && aggregate.generation == pool.generation) { return None; }
    if pool.items.iter().any(|item| !TERMINAL.contains(&item.status)) { return None; }
    Some(pool.items.iter().map(aggregate_item_result).collect())
}

pub fn aggregate_item_result(item: &super::types::WorkpoolItem) -> WorkpoolAggregateResult {
    match item.status {
        WorkpoolItemStatus::Completed => WorkpoolAggregateResult::Data { key: item.key.clone(), data: item.data.clone().unwrap_or(Json::Null) },
        _ => { let error = item.error.clone().unwrap_or_else(|| super::types::WorkpoolYieldError { code: "cancelled".into(), message: "Pool item did not complete.".into() }); WorkpoolAggregateResult::Error { key: item.key.clone(), code: error.code, message: error.message } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::manager::types::{ManagerStartSpec, ResolvedChildPlan};
    use super::super::types::{WorkpoolAgent, WorkpoolAggregate, WorkpoolItem, WorkpoolWorker, WorkpoolWorkerStatus, WorkpoolMode, WorkpoolSpec};

    fn pool(status: WorkpoolStatus, items: Vec<WorkpoolItem>, aggregate: Option<WorkpoolAggregate>) -> WorkpoolRecord {
        WorkpoolRecord { version: 1, pool_id: "wp_test".into(), name: "pool".into(), parent_session_id: "parent".into(), root_session_id: "root".into(), generation: 1, revision: 0, mode: WorkpoolMode::Fresh, agent: WorkpoolAgent::Category { prompt: "p".into(), model: None, category: "quick".into() }, worker_spec: WorkpoolSpec { start: ManagerStartSpec::default(), plan: ResolvedChildPlan::default() }, status, kernel_tool_names: None, items, workers: vec![], aggregate }
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
    fn delivered_same_generation_is_not_redelivered() { assert_eq!(aggregate_results(&pool(WorkpoolStatus::Closing, vec![], Some(WorkpoolAggregate { generation: 1, delivered: true, accepted: None }))), None); }

    #[test]
    fn nonterminal_items_keep_aggregate_pending() { assert_eq!(aggregate_results(&pool(WorkpoolStatus::Closing, vec![item("a", WorkpoolItemStatus::Assigned)], None)), None); }

    #[test]
    fn cancelled_item_becomes_keyed_cancelled_error() { assert_eq!(aggregate_item_result(&item("a", WorkpoolItemStatus::Cancelled)), WorkpoolAggregateResult::Error { key: "a".into(), code: "cancelled".into(), message: "Pool item did not complete.".into() }); }

    #[test]
    fn failed_item_preserves_error_details() { let mut value = item("a", WorkpoolItemStatus::Error); value.error = Some(super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "stale".into() }); assert_eq!(aggregate_item_result(&value), WorkpoolAggregateResult::Error { key: "a".into(), code: "kernel_tool_stale".into(), message: "stale".into() }); }

    #[test]
    fn finished_requires_closed_pool_and_idle_workers() { let mut value = pool(WorkpoolStatus::Closing, vec![item("a", WorkpoolItemStatus::Completed)], None); value.workers.push(WorkpoolWorker { task_id: "st".into(), run_epoch: 0, status: WorkpoolWorkerStatus::Idle, completed_turns: 0, idle_since: 0 }); assert!(pool_work_is_finished(&value)); }

    #[test]
    fn busy_worker_prevents_finished_state() { let mut value = pool(WorkpoolStatus::Closing, vec![], None); value.workers.push(WorkpoolWorker { task_id: "st".into(), run_epoch: 0, status: WorkpoolWorkerStatus::Busy, completed_turns: 0, idle_since: 0 }); assert!(!pool_work_is_finished(&value)); }
}
