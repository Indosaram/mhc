use senpi_task::dag::manager::{DagManager, DagRunSummary};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadGuardDagRun {
    pub run_id: String,
    pub name: String,
    pub status: String,
}

pub struct DagReloadSourceDeps {
    pub manager: Box<dyn DagReloadRunManager>,
    pub session_id: Box<dyn Fn() -> Option<String> + Send + Sync>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DagReloadSnapshotError;

pub trait DagReloadRunManager: Send + Sync {
    fn list(&self, session_id: &str) -> Vec<DagRunSummary>;
    fn snapshot_name(&self, run_id: &str, session_id: &str) -> Result<String, DagReloadSnapshotError>;
}

impl DagReloadRunManager for DagManager {
    fn list(&self, session_id: &str) -> Vec<DagRunSummary> { DagManager::list(self, session_id, None).unwrap_or_default() }
    fn snapshot_name(&self, run_id: &str, session_id: &str) -> Result<String, DagReloadSnapshotError> { DagManager::snapshot(self, &run_id.to_owned(), session_id).map(|snapshot| snapshot.name).map_err(|_| DagReloadSnapshotError) }
}

pub struct DagReloadSource {
    deps: DagReloadSourceDeps,
}

pub fn create_dag_reload_source(deps: DagReloadSourceDeps) -> DagReloadSource {
    DagReloadSource { deps }
}

impl DagReloadSource {
    pub fn live_runs(&self) -> Vec<crate::reload_guard::ReloadGuardDagRun> {
        let Some(session_id) = (self.deps.session_id)() else { return Vec::new() };
        self.deps.manager.list(&session_id).into_iter().filter_map(|summary| {
            if summary.status.is_terminal() { return None; }
            let name = self.deps.manager.snapshot_name(&summary.run_id, &session_id).unwrap_or_else(|_| summary.run_id.clone());
            Some(crate::reload_guard::ReloadGuardDagRun { run_id: summary.run_id, name, status: summary.status.as_str().to_owned() })
        }).collect()
    }
}

#[cfg(test)]
#[path = "dag_reload_source_tests.rs"]
mod tests;
