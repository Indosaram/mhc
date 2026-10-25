use std::sync::{Arc, Mutex};

use crate::host::HostError;
use crate::manager::child_handle::{ManagedChildHandle, WorkpoolChildIdentity};
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::manager_tests::fakes::{category_planner, lock, started};
use crate::manager::types::{
    ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec, ManagerConfig,
    TaskManagerOptions,
};
use crate::manager::{TaskManager, create_task_manager};
use crate::runners::RunnerOutcome;
use crate::state::{TaskRecord, TaskStatus};
use crate::steering::SendOutcome;
use crate::store::{StateDirConfig, TaskRecordStore};

#[derive(Default)]
struct RebindState {
    identity: Option<WorkpoolChildIdentity>,
    refuse: bool,
    fail_delivery: bool,
    followups: Vec<String>,
    started: Option<std::sync::mpsc::Sender<String>>,
}

struct RebindHandle {
    task_id: String,
    state: Arc<Mutex<RebindState>>,
    outcome: Mutex<std::sync::mpsc::Receiver<RunnerOutcome>>,
    _settle: std::sync::mpsc::Sender<RunnerOutcome>,
}
impl ManagedChildHandle for RebindHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        Some("resident".into())
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, _: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        let mut s = lock(&self.state);
        s.followups.push(text.into());
        if s.fail_delivery {
            Err(HostError {
                message: "delivery failed".into(),
            })
        } else {
            Ok(())
        }
    }
    fn rebind_workpool_identity(&self, id: WorkpoolChildIdentity) -> Option<Result<(), HostError>> {
        let mut s = lock(&self.state);
        s.identity = Some(id);
        if s.refuse {
            Some(Err(HostError {
                message: "rebind refused".into(),
            }))
        } else {
            Some(Ok(()))
        }
    }
    fn abort(&self) -> Result<(), HostError> {
        let _ = self._settle.send(RunnerOutcome::Cancelled);
        Ok(())
    }
    fn subscribe(&self, _: crate::manager::ManagedChildListener) -> crate::manager::Unsubscribe {
        Box::new(|| {})
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        lock(&self.outcome)
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("fixture outcome must settle within bounded timeout")
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), HostError> {
        let _ = self._settle.send(RunnerOutcome::Cancelled);
        Ok(())
    }
}

struct RebindRunner {
    state: Arc<Mutex<RebindState>>,
}
impl ManagedRunner for RebindRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        let (settle, outcome) = std::sync::mpsc::channel();
        if let Some(started) = &lock(&self.state).started {
            started
                .send(spec.task_id.clone())
                .expect("start subscriber");
        }
        Ok(Arc::new(RebindHandle {
            task_id: spec.task_id.clone(),
            state: self.state.clone(),
            outcome: Mutex::new(outcome),
            _settle: settle,
        }))
    }
}

fn rebind_manager(
    state: Arc<Mutex<RebindState>>,
) -> (TaskManager, TaskRecordStore, Arc<tempfile::TempDir>) {
    let root = Arc::new(tempfile::tempdir().expect("temp dir"));
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: root.path().into(),
        task_state_dir: None,
    });
    let runner: Arc<dyn ManagedRunner> = Arc::new(RebindRunner { state });
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: runner.clone(),
            process: runner,
        },
        category_planner(&[]),
        root.path().to_string_lossy(),
    );
    options.config = ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(1),
            ..Default::default()
        },
        ..ManagerConfig::default()
    };
    (create_task_manager(options), store, root)
}

fn terminal_record(store: &TaskRecordStore, id: &str) -> TaskRecord {
    let record = store.load(id).expect("load").expect("record");
    let terminal = TaskRecord {
        status: TaskStatus::Completed,
        final_response: Some("prior".into()),
        ..record
    };
    store.replace(&terminal).expect("terminal record");
    terminal
}

fn start_spec() -> crate::manager::types::ManagerStartSpec {
    crate::manager::types::ManagerStartSpec {
        parent_session_id: "parent".into(),
        root_session_id: Some("root".into()),
        depth: 1,
        category: Some("quick".into()),
        model: Some("test/model".into()),
        ..Default::default()
    }
}

#[test]
fn refused_rebind_burns_epoch_and_does_not_deliver() {
    let state = Arc::new(Mutex::new(RebindState {
        refuse: true,
        ..Default::default()
    }));
    let (manager, store, _root) = rebind_manager(state.clone());
    let started = started(manager.start(&start_spec()));
    let prior = terminal_record(&store, &started.task_id);
    let result = manager
        .revive_workpool_task(&started.task_id, prior.notification.run_epoch + 1, "turn")
        .expect("refusal outcome");
    assert!(matches!(result, SendOutcome::NotContinuable { .. }));
    let settled = store
        .load(&started.task_id)
        .expect("record")
        .expect("saved");
    assert_eq!(
        settled.notification.run_epoch,
        prior.notification.run_epoch + 1
    );
    assert_eq!(settled.status, TaskStatus::Error);
    let state = lock(&state);
    assert_eq!(
        state
            .identity
            .as_ref()
            .map(|identity| identity.state_dir.as_str()),
        Some(store.state_dir().to_string_lossy().as_ref())
    );
    assert!(state.followups.is_empty());
}

#[test]
fn delivery_failure_keeps_new_epoch_and_new_identity_and_marks_uncertain() {
    let state = Arc::new(Mutex::new(RebindState {
        fail_delivery: true,
        ..Default::default()
    }));
    let (manager, store, _root) = rebind_manager(state.clone());
    let started = started(manager.start(&start_spec()));
    let prior = terminal_record(&store, &started.task_id);
    let epoch = prior.notification.run_epoch + 1;
    let next = manager.start(&start_spec());
    let next = match next {
        crate::manager::types::StartResult::Started(task) => task,
        _ => panic!("queued task start"),
    };
    assert_eq!(next.status, TaskStatus::Pending);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    lock(&state).started = Some(started_tx);
    let result = manager
        .revive_workpool_task(&started.task_id, epoch, "turn")
        .expect("uncertain outcome");
    assert!(matches!(result, SendOutcome::NotContinuable { .. }));
    let latest = store
        .load(&started.task_id)
        .expect("record")
        .expect("saved");
    assert_eq!(latest.notification.run_epoch, epoch);
    assert_eq!(latest.status, TaskStatus::Error);
    let state = lock(&state);
    assert_eq!(
        state.identity.as_ref().map(|identity| identity.run_epoch),
        Some(epoch as u32)
    );
    assert_eq!(state.followups, ["turn"]);
    drop(state);
    assert_eq!(
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("queued runner starts after uncertain revival"),
        next.task_id
    );
    manager
        .cancel_task(&next.task_id, None, Default::default())
        .expect("cleanup queued runner");
}

#[test]
fn burnt_epoch_is_refused_before_record_or_identity_changes() {
    let state = Arc::new(Mutex::new(RebindState::default()));
    let (manager, store, _root) = rebind_manager(state.clone());
    let started = started(manager.start(&start_spec()));
    let mut prior = terminal_record(&store, &started.task_id);
    prior.notification.run_epoch = 8;
    store.replace(&prior).expect("burned epoch");
    let result = manager
        .revive_workpool_task(&started.task_id, 7, "turn")
        .expect("stale refusal");
    assert!(matches!(result, SendOutcome::NotContinuable { .. }));
    assert_eq!(
        store
            .load(&started.task_id)
            .expect("record")
            .expect("saved")
            .notification
            .run_epoch,
        8
    );
    assert!(lock(&state).identity.is_none());
}

#[test]
fn failed_rebind_settles_error_and_releases_slot_for_queued_start() {
    let state = Arc::new(Mutex::new(RebindState {
        refuse: true,
        ..Default::default()
    }));
    let (manager, store, _root) = rebind_manager(state.clone());
    let task = started(manager.start(&start_spec()));
    let record = store.load(&task.task_id).expect("load").expect("running");
    store
        .replace(&TaskRecord {
            status: TaskStatus::Completed,
            final_response: Some("done".into()),
            ..record.clone()
        })
        .expect("terminal record");
    let epoch = record.notification.run_epoch + 1;
    assert!(manager.workpool_worker_eligible(&task.task_id, "parent", epoch));
    let queued = manager.start(&start_spec());
    let queued = match queued {
        crate::manager::types::StartResult::Started(task) => task,
        _ => panic!("second task start"),
    };
    assert_eq!(queued.status, TaskStatus::Pending);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    lock(&state).started = Some(started_tx);
    let result = manager
        .revive_workpool_task(&task.task_id, epoch, "work")
        .expect("rebind refused");
    assert!(matches!(result, SendOutcome::NotContinuable { .. }));
    let settled = store
        .load(&task.task_id)
        .expect("load settled")
        .expect("record");
    assert_eq!(settled.status, TaskStatus::Error);
    assert_eq!(settled.notification.run_epoch, epoch);
    assert_eq!(
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("queued runner starts after failed revival"),
        queued.task_id
    );
    manager
        .cancel_task(&queued.task_id, None, Default::default())
        .expect("cleanup queued runner");
}
