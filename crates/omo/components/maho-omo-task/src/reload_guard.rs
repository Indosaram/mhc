use senpi_task::{manager::TaskManager, state::{TaskRecord, TaskStatus}};
use maho_ext_api::ReloadVetoDecision;
use crate::status_row_format::task_identity;

pub fn evaluate_reload_veto(records: &[TaskRecord]) -> Option<ReloadVetoDecision> {
    let running: Vec<_> = records.iter().filter(|record| record.status == TaskStatus::Running).collect();
    if running.is_empty() { return None }
    Some(ReloadVetoDecision { cancelled: true, reason: Some(format!("{} subagent(s) still running: {} - wait for them to finish or cancel them (task_cancel) before reloading.", running.len(), running.into_iter().map(task_identity).collect::<Vec<_>>().join(", "))) })
}
pub fn manager_reload_veto(manager: &TaskManager) -> Option<ReloadVetoDecision> {
    evaluate_reload_veto(&manager.resident_task_ids().iter().filter_map(|id| manager.get(id)).collect::<Vec<_>>())
}
