use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use senpi_task::host::HostError;
use senpi_task::manager::cloud_router::{CloudOffloadConfig, CloudOffloadMode};
use senpi_task::manager::execution_mode::ExecutionMode;
use senpi_task::manager::types::{
    ManagedRunner, ManagedRunnerError, ManagedRunnerResult, ManagedRunners, ManagedStartSpec,
    ManagerStartSpec, ResolvedChildPlan, StartResult, TaskManagerOptions,
};
use senpi_task::manager::{
    ManagedChildHandle, ManagedChildListener, TaskManager, Unsubscribe, create_task_manager,
};
use senpi_task::runners::{RunnerFailure, RunnerFailureKind, RunnerOutcome};
use senpi_task::state::TaskStatus;
use senpi_task::store::{StateDirConfig, TaskRecordStore};

struct ImmediateHandle {
    task_id: String,
    text: String,
}

impl ManagedChildHandle for ImmediateHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        Some(format!("sess-{}", self.task_id))
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        RunnerOutcome::Completed {
            final_text: Some(self.text.clone()),
        }
    }
    fn last_assistant_text(&self) -> Option<String> {
        Some(self.text.clone())
    }
    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

struct CountingRunner {
    label: String,
    calls: Arc<AtomicUsize>,
    fail_on_start: bool,
}

impl ManagedRunner for CountingRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_on_start {
            return Err(ManagedRunnerError::Runner(RunnerFailure::new(
                RunnerFailureKind::SpawnError,
                "simulated cloud outage",
            )));
        }
        Ok(Arc::new(ImmediateHandle {
            task_id: spec.task_id.clone(),
            text: format!("{}:{}", self.label, spec.prompt),
        }))
    }
}

fn make_manager(
    dir: &std::path::Path,
    local_calls: Arc<AtomicUsize>,
    cloud_calls: Arc<AtomicUsize>,
    cloud_fails: bool,
    cloud_offload: CloudOffloadConfig,
) -> (TaskManager, TaskRecordStore) {
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: dir.to_path_buf(),
        task_state_dir: Some(dir.join("state")),
    });
    let local_runner = Arc::new(CountingRunner {
        label: "local".into(),
        calls: local_calls,
        fail_on_start: false,
    });
    let cloud_runner = Arc::new(CountingRunner {
        label: "cloud".into(),
        calls: cloud_calls,
        fail_on_start: cloud_fails,
    });
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: local_runner.clone(),
            process: local_runner,
        },
        Arc::new(|_| {
            Ok(ResolvedChildPlan {
                model: "hms/deepseek-v4.1-flash-free".into(),
                ..ResolvedChildPlan::default()
            })
        }),
        dir.to_string_lossy(),
    );
    options.cloud_runner = Some(cloud_runner);
    options.cloud_available = Some(Arc::new(|| true));
    options.cloud_offload = cloud_offload;
    (create_task_manager(options), store)
}

#[test]
fn auto_offload_routes_isolated_to_cloud_and_keeps_simple_foreground_local() {
    let dir = tempfile::tempdir().unwrap();
    let local_calls = Arc::new(AtomicUsize::new(0));
    let cloud_calls = Arc::new(AtomicUsize::new(0));
    let (manager, _store) = make_manager(
        dir.path(),
        Arc::clone(&local_calls),
        Arc::clone(&cloud_calls),
        false,
        CloudOffloadConfig::default(),
    );

    let local_res = manager.start(&ManagerStartSpec {
        prompt: "quick local lookup".into(),
        parent_session_id: "parent-1".into(),
        depth: 1,
        ..ManagerStartSpec::default()
    });
    let StartResult::Started(local_started) = local_res else {
        panic!("expected local start");
    };
    let local_done = manager
        .wait_for(&local_started.task_id, Some(Duration::from_secs(5)), None)
        .unwrap();
    assert_eq!(local_done.status, TaskStatus::Completed);
    assert_eq!(local_done.execution_mode, ExecutionMode::InProcess.as_str());
    assert_eq!(local_calls.load(Ordering::SeqCst), 1);
    assert_eq!(cloud_calls.load(Ordering::SeqCst), 0);

    let cloud_res = manager.start(&ManagerStartSpec {
        prompt: "isolated edit task".into(),
        parent_session_id: "parent-1".into(),
        depth: 1,
        isolated: Some(true),
        ..ManagerStartSpec::default()
    });
    let StartResult::Started(cloud_started) = cloud_res else {
        panic!("expected cloud start");
    };
    let cloud_done = manager
        .wait_for(&cloud_started.task_id, Some(Duration::from_secs(5)), None)
        .unwrap();
    assert_eq!(cloud_done.status, TaskStatus::Completed);
    assert_eq!(cloud_done.execution_mode, ExecutionMode::Cloud.as_str());
    assert_eq!(cloud_done.result.as_deref(), Some("cloud:isolated edit task"));
    assert_eq!(cloud_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cloud_start_failure_automatically_falls_back_to_local_runner() {
    let dir = tempfile::tempdir().unwrap();
    let local_calls = Arc::new(AtomicUsize::new(0));
    let cloud_calls = Arc::new(AtomicUsize::new(0));
    let (manager, store) = make_manager(
        dir.path(),
        Arc::clone(&local_calls),
        Arc::clone(&cloud_calls),
        true,
        CloudOffloadConfig {
            mode: CloudOffloadMode::Always,
            fallback_to_local: true,
            ..CloudOffloadConfig::default()
        },
    );

    let res = manager.start(&ManagerStartSpec {
        prompt: "resilient task".into(),
        parent_session_id: "parent-2".into(),
        depth: 1,
        ..ManagerStartSpec::default()
    });
    let StartResult::Started(started) = res else {
        panic!("expected fallback start, got {res:?}");
    };
    let done = manager
        .wait_for(&started.task_id, Some(Duration::from_secs(5)), None)
        .unwrap();
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.result.as_deref(), Some("local:resilient task"));
    assert_eq!(cloud_calls.load(Ordering::SeqCst), 1);
    assert_eq!(local_calls.load(Ordering::SeqCst), 1);

    let log_path = store
        .state_dir()
        .join("logs")
        .join(format!("{}.jsonl", started.task_id));
    let log_text = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        log_text.contains("cloud_fallback_to_local"),
        "expected cloud_fallback_to_local event in {log_path:?}, got: {log_text}"
    );
}
