use std::sync::Arc;

use crate::lifecycle::RespawnResult;
use crate::lifecycle::port::{RespawnDisposition, RespawnFailureCode};
use crate::manager::TaskManager;
use crate::manager::manager_tests::fakes::{FakeRunner, Project};
use crate::manager::types::{
    ManagedRunner, ManagedRunners, ManagerStartSpec, ResolvedChildPlan, TaskManagerOptions,
};
use crate::state::{
    ResidencyState, SpawnSpecV1, TaskRecord, TaskRecordInput, TaskSpawnSpec, TaskStatus,
    create_task_record,
};
use crate::workpool::store::WorkpoolStore;
use crate::workpool::types::*;

#[test]
fn workpool_respawn_refuses_non_terminal_worker() {
    let project = Project::new();
    let manager = manager(&project, FakeRunner::new());
    let record = record();
    seed_pool(&project, &record, WorkpoolWorkerStatus::Busy);
    let result = manager.respawn(&record, Some(std::path::Path::new("/session")));
    assert!(matches!(
        result,
        RespawnResult::Failed {
            disposition: RespawnDisposition::Retryable,
            code: RespawnFailureCode::ToolsUnavailable,
            ..
        }
    ));
}

#[test]
fn workpool_respawn_injects_worker_tool() {
    let project = Project::new();
    let runner = FakeRunner::new();
    let mut record = record();
    // Terminal record is required to pass the worker-resume guard and reach tool injection.
    record.status = TaskStatus::Completed;
    seed_pool(&project, &record, WorkpoolWorkerStatus::Idle);
    // respawn persists its durable launch boundary through store.mutate, so the record has to be
    // in the store already; otherwise the launch fails with SpawnSpecUnavailable before reaching
    // tool injection.
    project.store().save(&record).expect("persist record");
    let manager = manager(&project, runner.clone());
    let result = manager.respawn(&record, Some(std::path::Path::new("/session")));
    if let RespawnResult::Failed { code, reason, .. } = &result {
        panic!("respawn failed with {code:?}: {reason}");
    }
    let specs = runner.resumed_specs.lock().expect("resume specs");
    assert!(
        specs[0]
            .member_scoped_tools
            .as_ref()
            .expect("member tools")
            .iter()
            .any(|tool| tool.name() == "workpool")
    );
}

fn seed_pool(project: &Project, record: &TaskRecord, status: WorkpoolWorkerStatus) {
    let store = WorkpoolStore::new(project.store().state_dir());
    let created = store
        .create(
            &caller(),
            &input(),
            WorkpoolMode::KeepAlive,
            WorkpoolSpec {
                start: ManagerStartSpec::default(),
                plan: ResolvedChildPlan::default(),
            },
        )
        .expect("create pool");
    let task_id = record.task_id.clone();
    store
        .mutate(&created.pool_id, move |pool| {
            let mut next = pool.clone();
            next.workers.push(WorkpoolWorker {
                task_id,
                run_epoch: 0,
                status,
                completed_turns: 1,
                idle_since: 0,
            });
            next
        })
        .expect("add worker");
}

fn caller() -> WorkpoolCaller {
    WorkpoolCaller {
        session_id: "parent".into(),
        root_session_id: "parent".into(),
        depth: 0,
        cwd: "/tmp".into(),
    }
}

fn input() -> WorkpoolCreate {
    WorkpoolCreate {
        name: "respawn-test".into(),
        agent: WorkpoolAgent::Category {
            prompt: "run".into(),
            model: None,
            category: "quick".into(),
        },
        mode: None,
        tools: None,
    }
}

fn manager(project: &Project, runner: Arc<FakeRunner>) -> TaskManager {
    let port: Arc<dyn ManagedRunner> = runner;
    let planner = Arc::new(|spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec.model.clone().unwrap_or_default(),
            ..Default::default()
        })
    });
    TaskManager::new(TaskManagerOptions::new(
        project.store(),
        ManagedRunners {
            in_process: port.clone(),
            process: port,
        },
        planner,
        project.cwd(),
    ))
}

fn record() -> TaskRecord {
    let input = TaskRecordInput {
        name: Some("worker".into()),
        parent_session_id: "parent".into(),
        root_session_id: "parent".into(),
        execution_mode: "in-process".into(),
        model: "test/model".into(),
        ..Default::default()
    };
    let mut record = create_task_record(input, None).expect("record");
    record.task_id = "st_00a0b001".into();
    record.status = TaskStatus::Running;
    record.residency_state = ResidencyState::Resident;
    // respawn rebuilds its launch spec from the recorded spawn spec; without one the launch is
    // refused with SpawnSpecUnavailable before tool injection can run.
    record.spawn_spec = Some(TaskSpawnSpec::V1(SpawnSpecV1 {
        cwd: "/tmp".into(),
        prompt: "resume".into(),
        instructions: None,
        member_scoped_tool_names: None,
        isolation: None,
    }));
    record
}
