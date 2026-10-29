use std::sync::{Arc, Mutex};

use crate::manager::TaskManager;
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::manager_tests::fakes::{FakeRunner, Project, wait_until};
use crate::manager::types::{ManagedRunner, ManagedRunners};
use crate::manager::types::{
    ManagerConfig, ManagerStartSpec, ResolvedChildPlan, TaskManagerOptions,
};
use crate::manager::workpool_admission::ManagerWorkpoolAdmission;
use crate::state::{TaskRecord, TaskStatus};
use crate::store::TaskRecordStore;
use crate::workpool::ports::{WorkpoolAdmission, WorkpoolRequest};
use crate::workpool::store::WorkpoolStore;
use crate::workpool::types::{
    WorkpoolAgent, WorkpoolCaller, WorkpoolDelivery, WorkpoolDeliveryPhase, WorkpoolEvent,
    WorkpoolItem, WorkpoolItemStatus, WorkpoolMode, WorkpoolRecord, WorkpoolSpec, WorkpoolStatus,
    WorkpoolWorker, WorkpoolWorkerStatus,
};

struct Request {
    finished: std::sync::mpsc::Sender<()>,
    store: WorkpoolStore,
    pool: WorkpoolRecord,
    item: WorkpoolItem,
    worker: Option<WorkpoolWorker>,
    current: Arc<std::sync::atomic::AtomicBool>,
    events: Arc<Mutex<Vec<WorkpoolEvent>>>,
    binds: Arc<Mutex<Vec<(String, u32)>>>,
}
impl Drop for Request {
    fn drop(&mut self) {
        // The dispatch closure drops its admission guard before releasing this request.
        let _receiver_closed = self.finished.send(());
    }
}
impl WorkpoolRequest for Request {
    fn pool(&self) -> &WorkpoolRecord {
        &self.pool
    }
    fn item(&self) -> &WorkpoolItem {
        &self.item
    }
    fn worker(&self) -> Option<&WorkpoolWorker> {
        self.worker.as_ref()
    }
    fn current(&self) -> bool {
        self.current.load(std::sync::atomic::Ordering::Acquire)
    }
    fn authorize(&self) -> Result<(), crate::workpool::ports::WorkpoolAdmissionError> {
        Ok(())
    }
    fn bind(&mut self, task_id: &str, epoch: u32) -> bool {
        self.binds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((task_id.into(), epoch));
        self.store
            .mutate(&self.pool.pool_id, |pool| {
                let mut pool = pool.clone();
                let item = pool
                    .items
                    .iter_mut()
                    .find(|item| item.item_id == self.item.item_id)
                    .expect("assigned item");
                item.status = WorkpoolItemStatus::Assigned;
                item.binding = Some(crate::workpool::types::WorkpoolBinding {
                    task_id: task_id.into(),
                    run_epoch: epoch,
                    generation: pool.generation,
                });
                item.delivery = Some(WorkpoolDelivery {
                    phase: WorkpoolDeliveryPhase::Queued,
                    message_sha256: None,
                    entry_ids: None,
                });
                pool
            })
            .expect("persist binding");
        true
    }
    fn event(&self, event: WorkpoolEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}

struct Harness {
    manager: Arc<TaskManager>,
    runner: Arc<FakeRunner>,
    store: TaskRecordStore,
    project: Project,
    admission: ManagerWorkpoolAdmission,
}
fn harness(limit: usize) -> Harness {
    let project = Project::new();
    let store = project.store();
    let runner = FakeRunner::new();
    let runner_port: Arc<dyn ManagedRunner> = runner.clone();
    let planner = Arc::new(|spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec.model.clone().unwrap_or_else(|| "test/model".into()),
            ..ResolvedChildPlan::default()
        })
    });
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: runner_port.clone(),
            process: runner_port,
        },
        planner,
        project.cwd(),
    );
    options.config = ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(limit),
            ..TaskConcurrencyConfig::default()
        },
        ..ManagerConfig::default()
    };
    let manager = Arc::new(TaskManager::new(options));
    let admission = ManagerWorkpoolAdmission::new(manager.clone());
    Harness {
        manager,
        runner,
        store,
        project,
        admission,
    }
}

fn pool(
    h: &Harness,
    task_id: &str,
    worker: Option<WorkpoolWorker>,
) -> (WorkpoolRecord, WorkpoolItem) {
    let item = WorkpoolItem {
        key: "k".into(),
        input: serde_json::json!({"value":"work"}),
        item_id: "wi_00000000000000000000000000000000".into(),
        status: WorkpoolItemStatus::Queued,
        binding: None,
        delivery: None,
        data: None,
        error: None,
        yield_sha256: None,
    };
    let start = ManagerStartSpec {
        prompt: "base".into(),
        parent_session_id: "parent".into(),
        root_session_id: Some("root".into()),
        depth: 1,
        category: Some("quick".into()),
        model: Some("test/model".into()),
        ..ManagerStartSpec::default()
    };
    let record = WorkpoolRecord {
        hold: None,
        version: 1,
        pool_id: "wp_00000000000000000000000000000000".into(),
        name: "pool".into(),
        parent_session_id: "parent".into(),
        root_session_id: "root".into(),
        generation: 1,
        revision: 0,
        mode: WorkpoolMode::Fresh,
        agent: WorkpoolAgent::Category {
            prompt: "base".into(),
            model: Some("test/model".into()),
            category: "quick".into(),
        },
        worker_spec: WorkpoolSpec {
            start,
            plan: ResolvedChildPlan {
                model: "test/model".into(),
                ..ResolvedChildPlan::default()
            },
        },
        status: WorkpoolStatus::Open,
        kernel_tool_names: None,
        items: vec![item.clone()],
        workers: worker.into_iter().collect(),
        aggregate: None,
    };
    let _ = (&h.project, task_id);
    (record, item)
}

type RequestFixture = (
    Box<dyn WorkpoolRequest>,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<Mutex<Vec<WorkpoolEvent>>>,
    Arc<Mutex<Vec<(String, u32)>>>,
    std::sync::mpsc::Receiver<()>,
);
fn request(
    h: &Harness,
    pool: WorkpoolRecord,
    item: WorkpoolItem,
    worker: Option<WorkpoolWorker>,
) -> RequestFixture {
    let (finished, completion) = std::sync::mpsc::channel();
    let current = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let events = Arc::new(Mutex::new(Vec::new()));
    let binds = Arc::new(Mutex::new(Vec::new()));
    (
        Box::new(Request {
            finished,
            store: WorkpoolStore::new(h.store.state_dir()),
            pool,
            item,
            worker,
            current: current.clone(),
            events: events.clone(),
            binds: binds.clone(),
        }),
        current,
        events,
        binds,
        completion,
    )
}

fn save_pool(h: &Harness, pool: &mut WorkpoolRecord) {
    let store = WorkpoolStore::new(h.store.state_dir());
    let caller = WorkpoolCaller {
        session_id: "parent".into(),
        root_session_id: "root".into(),
        depth: 0,
        cwd: h.project.cwd(),
    };
    let input = crate::workpool::types::WorkpoolCreate {
        name: pool.name.clone(),
        agent: pool.agent.clone(),
        mode: Some(pool.mode),
        tools: None,
    };
    let created = store
        .create(&caller, &input, pool.mode, pool.worker_spec.clone())
        .expect("persist pool");
    store
        .mutate(&created.pool_id, |old| {
            let mut next = old.clone();
            next.items = pool.items.clone();
            next.workers = pool.workers.clone();
            next
        })
        .expect("persist pool assignments");
    pool.pool_id = created.pool_id;
}

#[test]
fn queued_cancel_removes_waiter_and_releasing_head_does_not_run_cancelled_request() {
    let h = harness(1);
    assert!(h.manager.reserve_slot("test/model", "st_00000001", 0));
    let (mut pool, item) = pool(&h, "", None);
    save_pool(&h, &mut pool);
    let (request, _current, _events, _binds, _finished) = request(&h, pool, item, None);
    let handle = h.admission.request(request);
    assert!(handle.held.is_none());
    (handle.cancel)();
    h.manager
        .release_reserved_slot("st_00000001", "test/model", 0);
    wait_until("head capacity released", || {
        h.manager.has_free_slot("test/model")
    });
    assert!(h.runner.specs().is_empty());
}

#[test]
fn valid_reuse_binds_next_epoch_and_uses_real_manager_revival_path() {
    let h = harness(3);
    let start = ManagerStartSpec {
        parent_session_id: "parent".into(),
        root_session_id: Some("root".into()),
        depth: 1,
        category: Some("quick".into()),
        model: Some("test/model".into()),
        ..ManagerStartSpec::default()
    };
    let started = match h.manager.start(&start) {
        crate::manager::types::StartResult::Started(task) => task,
        _ => panic!("real manager task start"),
    };
    h.runner
        .wait_handle(&started.task_id)
        .enable_workpool_rebinding();
    let current = h
        .store
        .load(&started.task_id)
        .expect("load running worker")
        .expect("record");
    h.store
        .replace(&TaskRecord {
            status: TaskStatus::Completed,
            notification: crate::state::TaskNotification {
                run_epoch: current.notification.run_epoch,
                ..current.notification.clone()
            },
            final_response: Some("done".into()),
            ..current
        })
        .expect("terminalize reusable worker");
    let worker = WorkpoolWorker {
        task_id: started.task_id.clone(),
        run_epoch: 0,
        status: WorkpoolWorkerStatus::Idle,
        completed_turns: 0,
        idle_since: 0,
    };
    let (mut pool, _item) = pool(&h, &started.task_id, Some(worker.clone()));
    pool.items[0].status = WorkpoolItemStatus::Assigned;
    pool.items[0].binding = Some(crate::workpool::types::WorkpoolBinding {
        task_id: started.task_id.clone(),
        run_epoch: 1,
        generation: 1,
    });
    pool.items[0].delivery = Some(WorkpoolDelivery {
        phase: WorkpoolDeliveryPhase::Queued,
        message_sha256: None,
        entry_ids: None,
    });
    save_pool(&h, &mut pool);
    let item = pool.items[0].clone();
    let (request, _current, events, binds, _finished) = request(&h, pool, item, Some(worker));
    let _handle = h.admission.request(request);
    wait_until("real worker revival", || {
        events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|event| event.kind == crate::workpool::types::WorkpoolEventKind::Dispatched)
    });
    assert_eq!(
        binds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [(started.task_id.clone(), 1)]
    );
    assert_eq!(
        h.runner
            .wait_handle(&started.task_id)
            .follow_up_calls()
            .len(),
        1
    );
    assert_eq!(
        h.runner
            .wait_handle(&started.task_id)
            .workpool_identity()
            .expect("rebound identity")
            .run_epoch,
        1
    );
}

#[test]
fn fresh_prepared_workpool_launch_carries_epoch_zero_to_manager_runner_spec() {
    let h = harness(2);
    let caller = WorkpoolCaller {
        session_id: "parent".into(),
        root_session_id: "root".into(),
        depth: 0,
        cwd: h.project.cwd(),
    };
    let agent = WorkpoolAgent::Category {
        prompt: "work".into(),
        model: Some("test/model".into()),
        category: "quick".into(),
    };
    let resolved = h
        .manager
        .resolve_workpool_spec(&caller, &agent)
        .expect("resolve worker");
    let (task_id, _) = crate::state::create_task_id(None)
        .map(|id| (id.to_string(), ()))
        .expect("allocate task id");
    let (record, managed) = h
        .manager
        .allocate_workpool_record(&resolved, &task_id, i64::from(std::process::id()))
        .expect("prepare record");
    let managed = crate::manager::types::ManagedStartSpec {
        workpool_run_epoch: Some(u32::try_from(record.notification.run_epoch).expect("epoch fits")),
        ..managed
    };
    h.store.save(&record).expect("persist prelaunch record");
    h.manager
        .launch_prepared(record, managed)
        .expect("launch through manager");
    wait_until("runner receives fresh epoch", || {
        h.runner.specs().iter().any(|spec| spec.task_id == task_id)
    });
    assert_eq!(
        h.runner
            .specs()
            .iter()
            .find(|spec| spec.task_id == task_id)
            .and_then(|spec| spec.workpool_run_epoch),
        Some(0)
    );
}

#[test]
fn burnt_worker_epoch_is_not_reused() {
    let h = harness(2);
    let start = ManagerStartSpec {
        parent_session_id: "parent".into(),
        root_session_id: Some("root".into()),
        depth: 1,
        category: Some("quick".into()),
        model: Some("test/model".into()),
        ..ManagerStartSpec::default()
    };
    let started = match h.manager.start(&start) {
        crate::manager::types::StartResult::Started(task) => task,
        _ => panic!("real manager task start"),
    };
    h.runner.wait_handle(&started.task_id);
    let current = h
        .store
        .load(&started.task_id)
        .expect("load running worker")
        .expect("record");
    h.store
        .replace(&TaskRecord {
            status: TaskStatus::Completed,
            notification: crate::state::TaskNotification {
                run_epoch: current.notification.run_epoch,
                ..current.notification.clone()
            },
            final_response: Some("done".into()),
            ..current
        })
        .expect("terminal task");
    let worker = WorkpoolWorker {
        task_id: started.task_id.clone(),
        run_epoch: 0,
        status: WorkpoolWorkerStatus::Idle,
        completed_turns: 0,
        idle_since: 0,
    };
    let (mut pool, _item) = pool(&h, &started.task_id, Some(worker.clone()));
    pool.items[0].status = WorkpoolItemStatus::Assigned;
    pool.items[0].binding = Some(crate::workpool::types::WorkpoolBinding {
        task_id: started.task_id.clone(),
        run_epoch: 2,
        generation: 1,
    });
    pool.items[0].delivery = Some(WorkpoolDelivery {
        phase: WorkpoolDeliveryPhase::Queued,
        message_sha256: None,
        entry_ids: None,
    });
    save_pool(&h, &mut pool);
    let item = pool.items[0].clone();
    let (request, _current, events, binds, _finished) = request(&h, pool, item, Some(worker));
    let _handle = h.admission.request(request);
    wait_until("burnt epoch denial", || {
        events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|event| event.kind == crate::workpool::types::WorkpoolEventKind::AdmissionFailed)
    });
    assert!(
        binds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(
        h.runner
            .wait_handle(&started.task_id)
            .follow_up_calls()
            .is_empty()
    );
}

#[test]
fn a_paused_root_holds_the_turn_without_dispatching_or_acknowledging() {
    let h = harness(1);
    h.admission.barrier().close("root", 3);
    let (mut pool, item) = pool(&h, "", None);
    save_pool(&h, &mut pool);
    let (request, _current, events, binds, _finished) = request(&h, pool, item, None);
    let handle = h.admission.request(request);
    let held = handle.held.clone().expect("held custody");
    assert_eq!(held.pause_seq, 3);
    assert_eq!(held.root_session_id, "root");
    assert!(
        binds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(
        events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(h.runner.specs().is_empty());
    assert!(h.admission.barrier().in_flight("root").is_empty());
}

#[test]
fn delivery_held_follows_the_root_barrier_state() {
    let h = harness(1);
    assert!(!h.admission.delivery_held("root"));
    h.admission.barrier().close("root", 2);
    assert!(h.admission.delivery_held("root"));
    assert!(!h.admission.delivery_held("other"));
}

#[test]
fn an_open_root_admission_dispatches_and_leaves_no_ticket() {
    let h = harness(1);
    assert!(h.manager.reserve_slot("test/model", "st_00000001", 0));
    let (mut pool, item) = pool(&h, "", None);
    save_pool(&h, &mut pool);
    let (request, _current, _events, _binds, finished) = request(&h, pool, item, None);
    let handle = h.admission.request(request);
    assert!(handle.held.is_none());
    h.manager
        .release_reserved_slot("st_00000001", "test/model", 0);
    finished.recv_timeout(std::time::Duration::from_secs(30)).expect("admission dispatch finished");
    assert!(!h.runner.specs().is_empty());
    assert!(h.admission.barrier().in_flight("root").is_empty());
}
