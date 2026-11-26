use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use senpi_task::host::HostError;
use senpi_task::lifecycle::{
    DestroyCause, LifecycleDeps, ResidencyRegistry, ResidentHandle, ResidentKind, TaskLifecycle,
    TaskSettings, create_task_lifecycle,
};
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

struct Resident(Arc<dyn ManagedChildHandle>);
impl ResidentHandle for Resident {
    fn task_id(&self) -> &str {
        self.0.task_id()
    }
    fn is_same_managed(&self, handle: &Arc<dyn ManagedChildHandle>) -> Option<bool> {
        Some(Arc::ptr_eq(&self.0, handle))
    }
    fn kind(&self) -> ResidentKind {
        ResidentKind::InProcess
    }
    fn pid(&self) -> Option<i64> {
        self.0.pid()
    }
    fn abort(&self) -> Result<(), HostError> {
        self.0.abort()
    }
    fn terminate(&self) -> Result<(), HostError> {
        self.0.terminate()
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.0.dispose()
    }
}

#[derive(Default)]
struct Registry(Mutex<Weak<TaskManager>>);
impl Registry {
    fn manager(&self) -> Arc<TaskManager> {
        self.0
            .lock()
            .expect("lock test manager registry")
            .upgrade()
            .expect("test manager is alive")
    }
}
impl ResidencyRegistry for Registry {
    fn get(&self, id: &str) -> Option<Arc<dyn ResidentHandle>> {
        self.manager()
            .get_resident_handle(id)
            .map(|handle| Arc::new(Resident(handle)) as Arc<dyn ResidentHandle>)
    }
    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        self.manager()
            .resident_task_ids()
            .iter()
            .filter_map(|id| self.get(id))
            .collect()
    }
    fn forget(&self, id: &str) {
        self.manager().forget(id);
    }
    fn has_pending_sends(&self, id: &str) -> bool {
        self.manager().has_pending_sends(id)
    }
}

struct LifecycleDestruction(TaskLifecycle);
impl senpi_task::steering::DestructionPort for LifecycleDestruction {
    fn supports_fenced_eviction(&self) -> bool {
        true
    }
    fn destroy_resident_task(&self, id: &str, cause: DestroyCause) -> Result<(), HostError> {
        self.0
            .destroy_resident_task(id, cause)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
    fn evict_managed_if_current(
        &self,
        id: &str,
        handle: &Arc<dyn ManagedChildHandle>,
    ) -> Result<(), HostError> {
        self.0
            .evict_managed_if_current(id, handle)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
}

struct ImmediateHandle {
    task_id: String,
    text: String,
    cloud: bool,
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
            final_response: self.text.clone(),
        }
    }
    fn evict_after_outcome(&self) -> bool {
        self.cloud
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
                RunnerFailureKind::SessionCreateFailed,
                "simulated cloud outage",
            )));
        }
        Ok(Arc::new(ImmediateHandle {
            task_id: spec.task_id.clone(),
            text: format!("{}:{}", self.label, spec.prompt),
            cloud: self.label == "cloud",
        }))
    }
}

fn make_manager(
    dir: &std::path::Path,
    local_calls: Arc<AtomicUsize>,
    cloud_calls: Arc<AtomicUsize>,
    cloud_fails: bool,
    cloud_offload: CloudOffloadConfig,
    with_lifecycle: bool,
) -> (Arc<TaskManager>, TaskRecordStore) {
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
    let registry = Arc::new(Registry::default());
    if with_lifecycle {
        let lifecycle = create_task_lifecycle(LifecycleDeps::new(
            Arc::new(store.clone()),
            registry.clone(),
            TaskSettings::resolve(&serde_json::json!({})).expect("valid test lifecycle settings"),
        ));
        options.destruction = Some(Arc::new(LifecycleDestruction(lifecycle)));
    }
    let manager = Arc::new(create_task_manager(options));
    *registry.0.lock().expect("lock test manager registry") = Arc::downgrade(&manager);
    (manager, store)
}

#[test]
fn cloud_offload_without_lifecycle_fails_before_starting_the_runner() {
    let dir = tempfile::tempdir().unwrap();
    let local_calls = Arc::new(AtomicUsize::new(0));
    let cloud_calls = Arc::new(AtomicUsize::new(0));
    let (manager, _) = make_manager(
        dir.path(),
        local_calls,
        cloud_calls.clone(),
        false,
        CloudOffloadConfig {
            mode: CloudOffloadMode::Always,
            ..CloudOffloadConfig::default()
        },
        false,
    );
    let result = manager.start(&ManagerStartSpec {
        prompt: "must not acquire a cloud lease".into(),
        parent_session_id: "parent-missing-lifecycle".into(),
        depth: 1,
        ..ManagerStartSpec::default()
    });
    let StartResult::StartFailed(failure) = result else {
        panic!("cloud without lifecycle must fail: {result:?}");
    };
    assert_eq!(failure.execution_mode, ExecutionMode::Cloud);
    assert_eq!(cloud_calls.load(Ordering::SeqCst), 0);
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
        true,
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
        .wait_for(&local_started.task_id, None, Some(Duration::from_secs(5)))
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
        .wait_for(&cloud_started.task_id, None, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(cloud_done.status, TaskStatus::Completed);
    assert_eq!(cloud_done.execution_mode, ExecutionMode::Cloud.as_str());
    assert_eq!(
        cloud_done.final_response.as_deref(),
        Some("cloud:isolated edit task")
    );
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
        true,
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
        .wait_for(&started.task_id, None, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.final_response.as_deref(), Some("local:resilient task"));
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
