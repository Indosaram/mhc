use std::collections::BTreeMap;
use senpi_task::state::{CostReportStatus, TaskRecord, TaskRunStats};

#[derive(Clone, Debug)]
pub struct LiveRunStats {
    pub run_start_epoch: i64,
    pub stats: TaskRunStats,
}
#[derive(Default)]
struct TaskEntry {
    settled_usd: f64,
    run_start_epoch: i64,
    unsettled: bool,
    live_usd: f64,
}
#[derive(Default)]
pub struct SubagentCostLedger { entries: BTreeMap<String, TaskEntry> }
pub fn create_subagent_cost_ledger() -> SubagentCostLedger { SubagentCostLedger::default() }
impl SubagentCostLedger {
    pub fn upsert_record(&mut self, record: &TaskRecord) {
        let unsettled = record.run_stats.is_none();
        let live_usd = self.entries.get(&record.task_id).filter(|previous| unsettled && previous.unsettled && previous.run_start_epoch == record.run_start_epoch).map_or(0.0, |previous| previous.live_usd);
        self.entries.insert(record.task_id.clone(), TaskEntry {
            settled_usd: finite_positive(record.prior_runs_cost_usd) + record.run_stats.as_ref().map_or(0.0, reported_cost),
            run_start_epoch: record.run_start_epoch, unsettled, live_usd,
        });
    }
    pub fn remove(&mut self, task_id: &str) { self.entries.remove(task_id); }
    pub fn upsert_live(&mut self, task_id: &str, live: Option<&LiveRunStats>) {
        if let Some(entry) = self.entries.get_mut(task_id).filter(|entry| entry.unsettled) {
            entry.live_usd = live.filter(|live| live.run_start_epoch == entry.run_start_epoch).map_or(0.0, |live| reported_cost(&live.stats));
        }
    }
    pub fn is_unsettled(&self, task_id: &str) -> bool { self.entries.get(task_id).is_some_and(|entry| entry.unsettled) }
    pub fn total(&self) -> f64 { self.entries.values().map(|entry| entry.settled_usd + entry.live_usd).sum() }
    pub fn clear(&mut self) { self.entries.clear(); }
}
fn reported_cost(stats: &TaskRunStats) -> f64 {
    if matches!(stats.cost_status, Some(CostReportStatus::Reported | CostReportStatus::Partial)) { finite_positive(stats.cost_usd) } else { 0.0 }
}
fn finite_positive(value: Option<f64>) -> f64 { value.filter(|value| value.is_finite() && *value > 0.0).unwrap_or(0.0) }
