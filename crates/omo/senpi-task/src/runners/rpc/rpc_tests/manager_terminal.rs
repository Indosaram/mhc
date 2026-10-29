//! `manager/rpc-terminal-outcome.test.ts`: a real manager over the RPC process runner and the fake
//! RPC child binary.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::fake_child::{ChildGuard, spawn_fake_child};
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::runner::create_rpc_managed_runner;
use crate::manager::types::{
    ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec, ManagerConfig,
    ManagerStartSpec, ResolvedChildPlan, StartResult, TaskManagerOptions,
};
use crate::manager::{TaskManager, create_task_manager};
use crate::runners::rpc::process::RpcSpawnDescriptor;
use crate::runners::rpc_process::{RpcProcessRunner, RpcProcessRunnerOptions};
use crate::runners::types::RpcRunnerSpec;
use crate::runners::{RunnerFailureKind, RunnerOutcome};
use crate::state::TaskStatus;
use crate::store::{StateDirConfig, TaskRecordStore};

const WAIT: Duration = Duration::from_secs(10);

/// The in-process slot is never selected (`default_execution_mode: process`).
struct UnusedRunner;

impl ManagedRunner for UnusedRunner {
    fn start(&self, _spec: &ManagedStartSpec) -> ManagedRunnerResult {
        unreachable!("in-process runner is not selected in these tests")
    }
}

struct Harness {
    manager: TaskManager,
    store: TaskRecordStore,
    _children: Arc<Mutex<Vec<ChildGuard>>>,
    _project: tempfile::TempDir,
}

fn create_manager() -> Harness {
    let project = tempfile::tempdir().expect("project");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: project.path().to_path_buf(),
        task_state_dir: None,
    });
    let children: Arc<Mutex<Vec<ChildGuard>>> = Arc::default();
    let tracked = Arc::clone(&children);
    let process_runner = RpcProcessRunner::new(RpcProcessRunnerOptions {
        model_admission: Some(Arc::new(|_spec: &RpcRunnerSpec| Ok(()))),
        spawn_child: Some(Arc::new(move |descriptor: &RpcSpawnDescriptor| {
            let env: Vec<(&str, &str)> = descriptor
                .env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let child = spawn_fake_child(&env);
            tracked
                .lock()
                .expect("children")
                .push(ChildGuard(Arc::clone(&child)));
            child
        })),
        ..RpcProcessRunnerOptions::default()
    });
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: Arc::new(UnusedRunner),
            process: create_rpc_managed_runner(process_runner),
        },
        Arc::new(|spec: &ManagerStartSpec| {
            Ok(ResolvedChildPlan {
                model: spec.model.clone().unwrap_or_default(),
                ..ResolvedChildPlan::default()
            })
        }),
        project.path().to_string_lossy().into_owned(),
    );
    options.config = ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(2),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth: 2,
        default_execution_mode: ExecutionMode::Process,
    };
    Harness {
        manager: create_task_manager(options),
        store,
        _children: children,
        _project: project,
    }
}

fn process_spec(prompt: &str, run_in_background: bool) -> ManagerStartSpec {
    ManagerStartSpec {
        prompt: prompt.to_string(),
        parent_session_id: "parent-1".to_string(),
        depth: 1,
        execution_mode: Some(ExecutionMode::Process),
        model: Some("fixture/model".to_string()),
        run_in_background,
        ..ManagerStartSpec::default()
    }
}

fn stored_status(store: &TaskRecordStore, task_id: &str) -> Option<TaskStatus> {
    store
        .load(task_id)
        .expect("load")
        .map(|record| record.status)
}

#[test]
fn given_prompt_preflight_rejection_when_process_task_starts_then_record_becomes_typed_start_failure()
 {
    let harness = create_manager();

    let result = harness
        .manager
        .start(&process_spec("prompt-error:model missing", false));

    let StartResult::StartFailed(failure) = result else {
        panic!("expected start_failed, got {result:?}");
    };
    assert_eq!(
        stored_status(&harness.store, &failure.task_id),
        Some(TaskStatus::Error)
    );
    assert_eq!(failure.error_message, "Child prompt failed to start.");
}

#[test]
fn given_provider_failure_after_prompt_admission_when_rpc_turn_ends_then_task_is_child_turn_failed()
{
    let harness = create_manager();

    let result = harness
        .manager
        .start(&process_spec("turn-error:provider unavailable", true));

    let StartResult::Started(started) = result else {
        panic!("expected started, got {result:?}");
    };
    let handle = harness
        .manager
        .get_resident_handle(&started.task_id)
        .expect("resident handle");
    let outcome = handle.wait_for_outcome();
    let terminal = harness
        .manager
        .wait_for(&started.task_id, None, Some(WAIT))
        .expect("terminal");
    match outcome {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
            assert_eq!(failure.message, "provider unavailable");
        }
        other => panic!("expected error outcome, got {other:?}"),
    }
    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(
        terminal.error_message.as_deref(),
        Some("provider unavailable")
    );
    assert_eq!(
        stored_status(&harness.store, &started.task_id),
        Some(TaskStatus::Error)
    );
}
