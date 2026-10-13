use std::sync::Arc;
use senpi_task::{manager::TaskManager, state::{TaskRecord, TaskStatus}};
use maho_ext_api::ReloadVetoDecision;
use crate::status_row_format::task_identity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadGuardDagRun { pub run_id: String, pub name: String, pub status: String }
pub trait ReloadGuardDagSource: Send + Sync { fn live_runs(&self) -> Vec<ReloadGuardDagRun>; }

pub fn dag_reload_veto(source: &dyn ReloadGuardDagSource) -> Option<ReloadVetoDecision> {
    let live = source.live_runs();
    if live.is_empty() { return None; }
    Some(ReloadVetoDecision { cancelled: true, reason: Some(format!("{} DAG run(s) still live: {} - wait for them to finish before reloading.", live.len(), live.into_iter().map(|run| format!("{} ({}) [{}]", run.name, run.run_id, run.status)).collect::<Vec<_>>().join(", "))) })
}
pub fn wire_dag_reload_guard(api: &mut maho_ext_api::ExtensionApi, source: Arc<dyn ReloadGuardDagSource>) {
    api.on(maho_ext_api::EventKind::SessionBeforeReload, Arc::new(move |_, _| {
        let result = dag_reload_veto(source.as_ref()).map_or(maho_ext_api::EventResult::None, |veto| {
            maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                cancel: Some(veto.cancelled), reason: veto.reason, ..Default::default()
            })
        });
        Box::pin(async move { Ok(result) })
    }));
}

pub fn evaluate_reload_veto(records: &[TaskRecord]) -> Option<ReloadVetoDecision> {
    let running: Vec<_> = records.iter().filter(|record| record.status == TaskStatus::Running).collect();
    if running.is_empty() { return None }
    Some(ReloadVetoDecision { cancelled: true, reason: Some(format!("{} subagent(s) still running: {} - wait for them to finish or cancel them (task_cancel) before reloading.", running.len(), running.into_iter().map(task_identity).collect::<Vec<_>>().join(", "))) })
}
pub fn manager_reload_veto(manager: &TaskManager) -> Option<ReloadVetoDecision> {
    evaluate_reload_veto(&manager.resident_task_ids().iter().filter_map(|id| manager.get(id)).collect::<Vec<_>>())
}
pub fn wire_reload_guard(api:&mut maho_ext_api::ExtensionApi,manager:std::sync::Arc<TaskManager>) {
    api.on(maho_ext_api::EventKind::SessionBeforeReload,std::sync::Arc::new(move |_,_| {
        let result=manager_reload_veto(&manager).map_or(maho_ext_api::EventResult::None,|veto| maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { cancel:Some(veto.cancelled),reason:veto.reason,..Default::default() }));
        Box::pin(async move { Ok(result) })
    }));
}
