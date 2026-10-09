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
}
