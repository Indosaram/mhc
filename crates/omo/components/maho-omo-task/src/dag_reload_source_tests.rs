use std::sync::Arc;

use super::{create_dag_reload_source, DagReloadRunManager, DagReloadSourceDeps};
use crate::reload_guard::ReloadGuardDagRun;

fn manager() -> (tempfile::TempDir, senpi_task::dag::manager::DagManager) {
    let root = tempfile::tempdir().expect("temporary project");
    let store = Arc::new(senpi_task::dag::store::create_dag_file_store(
        &senpi_task::dag::store::DagStoreConfig::new(root.path()), Default::default(),
    ).expect("create DAG store"));
    let manager = senpi_task::dag::manager::create_dag_manager(senpi_task::dag::manager::DagManagerOptions {
        store, new_run_id: None, now: None, materialize_skills: None, settings: None,
    });
    (root, manager)
}

struct FakeReloadManager { runs: Vec<senpi_task::dag::manager::DagRunSummary>, fail_snapshot: bool }
impl DagReloadRunManager for FakeReloadManager {
    fn list(&self, _session: &str) -> Vec<senpi_task::dag::manager::DagRunSummary> { self.runs.clone() }
    fn snapshot_name(&self, run_id: &str, _session: &str) -> Result<String, super::DagReloadSnapshotError> {
        if self.fail_snapshot { Err(super::DagReloadSnapshotError) } else { self.runs.iter().find(|run| run.run_id == run_id).map(|run| run.name.clone()).ok_or(super::DagReloadSnapshotError) }
    }
}

fn summary(run_id: &str, status: senpi_task::dag::types::DagRunStatus, name: &str) -> senpi_task::dag::manager::DagRunSummary {
    senpi_task::dag::manager::DagRunSummary { run_id:run_id.into(), run_key:"key".into(), name:name.into(), parent_session_id:"parent".into(), status, created_at:"".into(), updated_at:"".into(), counts:Default::default() }
}

#[test]
fn given_paused_run_when_live_runs_are_read_then_it_is_reported_with_its_name() {
    let (_root, manager) = manager();
    manager.start(senpi_task::dag::manager::DagStartParams {
        parent_session_id: "parent".into(), root_session_id: "parent".into(),
        definition: senpi_task::dag::graph::DagDefinition {
            key: "key".into(), name: "release".into(), nodes: vec![senpi_task::dag::graph::DagNodeInput {
                id: "node".into(), prompt: "work".into(), target: senpi_task::dag::types::DagNodeTarget::Category("quick".into()),
                label: None, depends_on: None, task_summary: None, description: None, load_skills: None,
            }],
        },
    }).expect("start run");
    let source = create_dag_reload_source(DagReloadSourceDeps {
        manager: Box::new(manager),
        session_id: Box::new(|| Some("parent".into())),
    });
    assert_eq!(source.live_runs().iter().map(|run| (run.name.as_str(), run.status.as_str())).collect::<Vec<_>>(), [("release", "pending")]);
}

#[test]
fn given_running_and_paused_runs_when_live_runs_are_read_then_both_are_reported_with_names() {
    let manager = FakeReloadManager { runs:vec![summary("dag_1",senpi_task::dag::types::DagRunStatus::Running,"mass-ulw"), summary("dag_2",senpi_task::dag::types::DagRunStatus::Paused,"release")], fail_snapshot:false };
    let source = create_dag_reload_source(DagReloadSourceDeps { manager:Box::new(manager), session_id:Box::new(|| Some("parent".into())) });
    assert_eq!(source.live_runs(), [
        ReloadGuardDagRun { run_id:"dag_1".into(), name:"mass-ulw".into(), status:"running".into() },
        ReloadGuardDagRun { run_id:"dag_2".into(), name:"release".into(), status:"paused".into() },
    ]);
}

#[test]
fn given_no_session_when_live_runs_are_read_then_result_is_empty() {
    let (_root, manager) = manager();
    let source = create_dag_reload_source(DagReloadSourceDeps { manager: Box::new(manager), session_id: Box::new(|| None) });
    assert!(source.live_runs().is_empty());
}

#[test]
fn given_only_terminal_runs_when_live_runs_are_read_then_nothing_is_live() {
    let manager = FakeReloadManager { runs:vec![summary("done",senpi_task::dag::types::DagRunStatus::Completed,"done"), summary("failed",senpi_task::dag::types::DagRunStatus::Failed,"failed"), summary("cancelled",senpi_task::dag::types::DagRunStatus::Cancelled,"cancelled")], fail_snapshot:false };
    let source = create_dag_reload_source(DagReloadSourceDeps { manager:Box::new(manager), session_id: Box::new(|| Some("parent".into())) });
    assert!(source.live_runs().is_empty());
}

#[test]
fn given_run_pruned_between_list_and_snapshot_when_live_runs_are_read_then_run_id_is_fallback_name() {
    let manager = FakeReloadManager { runs:vec![summary("dag_1",senpi_task::dag::types::DagRunStatus::Running,"live name")], fail_snapshot:true };
    let source = create_dag_reload_source(DagReloadSourceDeps { manager:Box::new(manager), session_id: Box::new(|| Some("parent".into())) });
    assert_eq!(source.live_runs(), [ReloadGuardDagRun { run_id:"dag_1".into(), name:"dag_1".into(), status:"running".into() }]);
}
