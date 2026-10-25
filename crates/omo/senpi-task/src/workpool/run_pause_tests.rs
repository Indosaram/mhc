//! Stop/Resume scenarios ported from run-pause{,-workers}.test.ts.
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, watch};
use super::engine::WorkpoolEngine;
use super::ports::*;
use super::run_pause::ResumePoolResult;
use super::types::*;
use super::worker_turn::*;
use crate::state::{TaskRecord, TaskRecordInput, TaskStatus, create_task_record};
use crate::store::{StateDirConfig, TaskRecordStore};

struct Admission {
    tasks: TaskRecordStore,
    requests: Mutex<Vec<Box<dyn WorkpoolRequest>>>,
    cancelled: Arc<Mutex<Vec<String>>>,
    root_held: AtomicBool,
}
impl WorkpoolAdmission for Admission {
    fn tasks(&self) -> &TaskRecordStore { &self.tasks }
    fn bind_pool_kernel_tools(&self, _: &str, _: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError> { Ok(()) }
    fn unbind_pool_kernel_tools(&self, _: &str) {}
    fn resolve(&self, _: &WorkpoolCaller, _: &WorkpoolAgent) -> Result<WorkpoolSpec, WorkpoolAdmissionError> { Ok(WorkpoolSpec { start: Default::default(), plan: Default::default() }) }
    fn authorize_spawn(&self, _: &WorkpoolAgent, _: &str) -> Result<(), WorkpoolAdmissionError> { Ok(()) }
    fn has_free_slot(&self, _: &str) -> bool { true }
    fn request(&self, request: Box<dyn WorkpoolRequest>) -> WorkpoolRequestHandle {
        let key = request.item().key.clone(); let cancelled = self.cancelled.clone();
        self.requests.lock().expect("requests").push(request);
        WorkpoolRequestHandle::dispatched(Box::new(move || cancelled.lock().expect("cancelled").push(key)))
    }
    fn delivery_held(&self, _: &str) -> bool { self.root_held.load(Ordering::SeqCst) }
    fn drain(&self) {}
    fn get(&self, id: &str) -> Option<TaskRecord> { self.tasks.load(id).expect("task load") }
    fn task_run_epoch(&self, id: &str) -> Option<u32> { self.get(id).and_then(|task| u32::try_from(task.notification.run_epoch).ok()) }
    fn pending(&self, _: &str) -> bool { false }
    fn cancel<'a>(&'a self, _: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolAdmissionError>> + Send + 'a>> { Box::pin(async { Ok(()) }) }
    fn wait_for<'a>(&'a self, _: &'a str, cancel: &'a mut watch::Receiver<bool>) -> Pin<Box<dyn Future<Output = Result<TaskRecord, WorkpoolAdmissionError>> + Send + 'a>> {
        Box::pin(async move {
            while !*cancel.borrow_and_update() { cancel.changed().await.map_err(|e| WorkpoolAdmissionError(e.to_string()))?; }
            Err(WorkpoolAdmissionError("cancelled".into()))
        })
    }
}
fn fixture(root: &std::path::Path) -> (WorkpoolEngine, Arc<Admission>, WorkpoolCaller, mpsc::UnboundedReceiver<WorkpoolEvent>) {
    let admission = Arc::new(Admission { tasks: TaskRecordStore::new(&StateDirConfig { project_dir: root.into(), ..Default::default() }), requests: Mutex::default(), cancelled: Arc::default(), root_held: AtomicBool::new(false) });
    let (tx, rx) = mpsc::unbounded_channel();
    let engine = WorkpoolEngine::with_events(root, admission.clone(), Arc::new(move |event| { tx.send(event).expect("event receiver"); }));
    let caller = WorkpoolCaller { session_id: "parent".into(), root_session_id: "root".into(), depth: 0, cwd: root.to_string_lossy().into() };
    (engine, admission, caller, rx)
}
fn create(engine: &mut WorkpoolEngine, caller: &WorkpoolCaller) -> WorkpoolRecord {
    engine.create(caller, &WorkpoolCreate { name: "pool".into(), agent: WorkpoolAgent::Category { prompt: "work".into(), model: None, category: "quick".into() }, mode: Some(WorkpoolMode::Fresh), tools: None }).expect("create")
}
async fn waiting(rx: &mut mpsc::UnboundedReceiver<WorkpoolEvent>, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut seen = 0;
        while seen < count { if rx.recv().await.expect("event").kind == WorkpoolEventKind::Waiting { seen += 1; } }
    }).await.expect("waiting event deadline");
}
#[tokio::test(flavor = "current_thread")]
async fn stop_revokes_pending_request_and_survives_reload_with_monotonic_release() {
    let root = tempfile::tempdir().expect("root");
    let (mut engine, admission, caller, mut events) = fixture(root.path());
    let pool = create(&mut engine, &caller);
    engine.push(&caller, &pool.pool_id, &[WorkpoolInput { key: "one".into(), input: 1.into() }]).expect("push");
    waiting(&mut events, 1).await;
    engine.pause_for_run(&caller, &pool.pool_id, 9).expect("pause");
    let mut requests = admission.requests.lock().expect("requests");
    assert!(!requests[0].current()); assert!(!requests[0].bind("st_12345678", 0));
    drop(requests);
    assert_eq!(*admission.cancelled.lock().expect("cancelled"), ["one"]);
    let held = engine.inspect(&caller, &pool.pool_id).expect("inspect");
    assert_eq!(held.generation, pool.generation); assert_eq!(held.items[0].status, WorkpoolItemStatus::Queued);
    engine.dispose();
    let recovered = WorkpoolEngine::new(root.path(), admission.clone());
    recovered.attach(&caller.session_id, None).expect("attach");
    assert!(!recovered.dispatcher.take_scheduled(&pool.pool_id));
    assert_eq!(recovered.inspect(&caller, &pool.pool_id).expect("reload").hold.expect("hold").pause_seq, 9);
    assert_eq!(recovered.resume_for_run(&caller, &pool.pool_id, 8, None).expect("stale"), ResumePoolResult::Stale);
    recovered.pause_for_run(&caller, &pool.pool_id, 10).expect("new pause");
    recovered.pause_for_run(&caller, &pool.pool_id, 9).expect("older pause");
    assert_eq!(recovered.resume_for_run(&caller, &pool.pool_id, 9, None).expect("stale"), ResumePoolResult::Stale);
    assert_eq!(recovered.resume_for_run(&caller, &pool.pool_id, 10, None).expect("resume"), ResumePoolResult::Released);
    recovered.dispose();
}
#[tokio::test(flavor = "current_thread")]
async fn two_busy_workers_and_waiters_preserve_assignments_and_resume_in_order() {
    let root = tempfile::tempdir().expect("root");
    let (mut engine, admission, caller, mut events) = fixture(root.path());
    let pool = create(&mut engine, &caller);
    let keys = ["one", "two", "three", "four", "five", "six"];
    engine.push(&caller, &pool.pool_id, &keys.iter().map(|key| WorkpoolInput { key: (*key).into(), input: (*key).into() }).collect::<Vec<_>>()).expect("push");
    waiting(&mut events, 6).await;
    for index in 0..2 {
        let mut task = create_task_record(TaskRecordInput { parent_session_id: "parent".into(), root_session_id: "root".into(), ..Default::default() }, None).expect("task");
        task.status = TaskStatus::Running; admission.tasks.save(&task).expect("save");
        let mut requests = admission.requests.lock().expect("requests"); let request = &mut requests[index];
        assert!(request.bind(&task.task_id, 0));
        let turn = (pool.pool_id.clone(), pool.generation, task.task_id.clone(), 0);
        append_assigned_messages(&engine.store, &admission.tasks, &turn).expect("append");
        let begun = begin_worker_turn(&engine.store, &admission.tasks, &turn, None).expect("begin").expect("prompt");
        acknowledge_worker_turn(&engine.store, &admission.tasks, &turn, &begun.entries).expect("ack");
        request.event(WorkpoolEvent { pool_id: pool.pool_id.clone(), kind: WorkpoolEventKind::Dispatched, item_id: Some(request.item().item_id.clone()), task_id: Some(task.task_id), run_epoch: Some(0), error: None });
    }
    engine.pause_for_run(&caller, &pool.pool_id, 7).expect("pause");
    assert!(!admission.requests.lock().expect("requests")[2].current());
    let held = engine.store.load(&pool.pool_id).expect("pool");
    assert!(held.workers.iter().all(|w| w.status == WorkpoolWorkerStatus::Paused));
    assert_eq!(held.items.iter().map(|i| i.status).collect::<Vec<_>>(), [WorkpoolItemStatus::Assigned, WorkpoolItemStatus::Assigned, WorkpoolItemStatus::Queued, WorkpoolItemStatus::Queued, WorkpoolItemStatus::Queued, WorkpoolItemStatus::Queued]);
    assert_eq!(admission.cancelled.lock().expect("cancelled").len(), 4);
    for worker in &held.workers {
        admission.tasks.mutate(&worker.task_id, |task| { let mut next = task.clone(); next.status = TaskStatus::Paused; next }).expect("pause task");
        super::reconcile::reconcile_worker_turn(&engine.store, &admission.tasks, &(pool.pool_id.clone(), pool.generation, worker.task_id.clone(), 0)).expect("paused reconcile");
    }
    assert_eq!(engine.store.load(&pool.pool_id).expect("held items").items, held.items);
    for worker in &held.workers {
        admission.tasks.mutate(&worker.task_id, |task| { let mut next = task.clone(); next.status = TaskStatus::Running; next }).expect("resume task");
    }
    assert_eq!(engine.resume_for_run(&caller, &pool.pool_id, 7, None).expect("resume"), ResumePoolResult::Released);
    waiting(&mut events, 4).await;
    assert!(engine.store.load(&pool.pool_id).expect("pool").workers.iter().all(|w| w.status == WorkpoolWorkerStatus::Busy));
    assert_eq!(admission.requests.lock().expect("requests")[6..].iter().map(|r| r.item().key.clone()).collect::<Vec<_>>(), ["three", "four", "five", "six"]);
    engine.dispose();
}
#[tokio::test(flavor = "current_thread")]
async fn pool_and_root_holds_suppress_aggregate_until_both_release() {
    let root = tempfile::tempdir().expect("root");
    let (mut engine, admission, caller, _events) = fixture(root.path());
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = delivered.clone();
    engine.set_aggregate_port(Arc::new(move |_: super::aggregate::WorkpoolAggregateMessage, _: super::aggregate::WorkpoolAggregateReceipts| { count.fetch_add(1, Ordering::SeqCst); Ok(()) }));
    let pool = create(&mut engine, &caller);
    engine.pause_for_run(&caller, &pool.pool_id, 1).expect("pause"); engine.close(&caller, &pool.pool_id).expect("close");
    assert!(!engine.flush_aggregate(&pool.pool_id).expect("pool hold")); assert_eq!(delivered.load(Ordering::SeqCst), 0);
    admission.root_held.store(true, Ordering::SeqCst);
    engine.resume_for_run(&caller, &pool.pool_id, 1, None).expect("resume");
    assert!(!engine.flush_aggregate(&pool.pool_id).expect("root hold")); assert_eq!(delivered.load(Ordering::SeqCst), 0);
    admission.root_held.store(false, Ordering::SeqCst);
    assert!(engine.flush_aggregate(&pool.pool_id).expect("released")); assert_eq!(delivered.load(Ordering::SeqCst), 1);
    engine.dispose();
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_and_paused_workers_restore_and_unheld_credentials_reach_admission() {
    let root = tempfile::tempdir().expect("root");
    let (mut engine, admission, caller, mut events) = fixture(root.path());
    let pool = create(&mut engine, &caller);
    let mut tasks = Vec::new();
    for status in [TaskStatus::Completed, TaskStatus::Paused] {
        let mut task = create_task_record(TaskRecordInput { parent_session_id: "parent".into(), root_session_id: "root".into(), ..Default::default() }, None).expect("task");
        task.status = status; admission.tasks.save(&task).expect("save"); tasks.push(task);
    }
    engine.store.mutate(&pool.pool_id, |p| {
        let mut next = p.clone();
        for task in &tasks { next.workers.push(WorkpoolWorker { task_id: task.task_id.clone(), run_epoch: 0, status: WorkpoolWorkerStatus::Busy, completed_turns: 0, idle_since: 0 }); }
        next
    }).expect("workers");
    engine.pause_for_run(&caller, &pool.pool_id, 1).expect("pause");
    engine.resume_for_run(&caller, &pool.pool_id, 1, None).expect("resume");
    let resumed = engine.store.load(&pool.pool_id).expect("pool");
    assert_eq!(resumed.workers[0].status, WorkpoolWorkerStatus::Idle); assert_eq!(resumed.workers[0].completed_turns, 1);
    assert_eq!(resumed.workers[1].status, WorkpoolWorkerStatus::Paused);
    assert_eq!(engine.owned_task_ids().expect("inventory"), tasks.iter().map(|t| t.task_id.clone()).collect::<std::collections::BTreeSet<_>>());
    let barrier = crate::pause::admission_barrier::AdmissionBarrier::new();
    barrier.close(&caller.session_id, 1);
    let credential = barrier.resume(&caller.session_id, 1, "resume").expect("credential");
    assert_eq!(engine.resume_for_run(&caller, &pool.pool_id, 1, Some(credential.clone())).expect("unheld"), ResumePoolResult::NotHeld);
    engine.push(&caller, &pool.pool_id, &[WorkpoolInput { key: "next".into(), input: 1.into() }]).expect("push");
    waiting(&mut events, 1).await;
    assert_eq!(admission.requests.lock().expect("requests")[0].resume_credential(), Some(credential));
    engine.dispose();
}
