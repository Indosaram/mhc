use std::sync::Arc;

use crate::lifecycle::RespawnResult;
use crate::lifecycle::port::{RespawnDisposition, RespawnFailureCode};
use crate::manager::manager_tests::fakes::{FakeRunner, Project};
use crate::manager::types::{ManagedRunner, ManagedRunners, ManagerStartSpec, ResolvedChildPlan, TaskManagerOptions};
use crate::manager::TaskManager;
use crate::state::{create_task_record, ResidencyState, TaskRecord, TaskRecordInput, TaskStatus};
use crate::workpool::store::WorkpoolStore;
use crate::workpool::types::*;

#[test]
fn workpool_respawn_refuses_non_terminal_worker() {
    let project = Project::new();
    let manager = manager(&project, FakeRunner::new());
    let record = record();
    let pool = pool(&record, WorkpoolWorkerStatus::Busy);
    WorkpoolStore::new(project.store().state_dir()).save(&pool).expect("save pool");
    let result = manager.respawn(&record, Some(std::path::Path::new("/session")));
    assert!(matches!(result, RespawnResult::Failed { disposition: RespawnDisposition::Retryable, code: RespawnFailureCode::ToolsUnavailable, .. }));
}

#[test]
fn workpool_respawn_injects_worker_tool() {
    let project = Project::new();
    let runner = FakeRunner::new();
    let mut record = record();
    // Terminal record is required to pass the worker-resume guard and reach tool injection.
    record.status = TaskStatus::Completed;
    let pool = pool(&record, WorkpoolWorkerStatus::Idle);
    WorkpoolStore::new(project.store().state_dir()).save(&pool).expect("save pool");
    let manager = manager(&project, runner.clone());
    let result = manager.respawn(&record, Some(std::path::Path::new("/session")));
    assert!(!matches!(result, RespawnResult::Failed { .. }));
    let specs = runner.resumed_specs.lock().expect("resume specs");
    assert!(specs[0].member_scoped_tools.as_ref().expect("member tools").iter().any(|tool| tool.name() == "workpool"));
}

fn manager(project: &Project, runner: Arc<FakeRunner>) -> TaskManager {
    let port: Arc<dyn ManagedRunner> = runner;
    let planner = Arc::new(|spec: &ManagerStartSpec| Ok(ResolvedChildPlan { model: spec.model.clone().unwrap_or_default(), ..Default::default() }));
    TaskManager::new(TaskManagerOptions::new(project.store(), ManagedRunners { in_process: port.clone(), process: port }, planner, project.cwd()))
}

fn record() -> TaskRecord {
    let input = TaskRecordInput { name: Some("worker".into()), parent_session_id: "parent".into(), root_session_id: "parent".into(), execution_mode: "in_process".into(), model: "test/model".into(), ..Default::default() };
    let mut record = create_task_record(input, None).expect("record");
    record.task_id = "st_workpool_respawn".into();
    record.status = TaskStatus::Running;
    record.residency_state = ResidencyState::Orphaned;
    record
}

fn pool(record: &TaskRecord, status: WorkpoolWorkerStatus) -> WorkpoolRecord {
    WorkpoolRecord { version: 1, pool_id: "pool_respawn".into(), name: "pool".into(), parent_session_id: record.parent_session_id.clone(), root_session_id: record.root_session_id.clone(), generation: 0, revision: 0, mode: WorkpoolMode::KeepAlive, agent: WorkpoolAgent::Category { prompt: "p".into(), model: None, category: "x".into() }, worker_spec: WorkpoolSpec { start: ManagerStartSpec::default(), plan: ResolvedChildPlan::default() }, status: WorkpoolStatus::Open, kernel_tool_names: None, items: vec![], workers: vec![WorkpoolWorker { task_id: record.task_id.clone(), run_epoch: 0, status, completed_turns: 1, idle_since: 0 }], hold: None, aggregate: None }
}
