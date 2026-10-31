use std::sync::Arc;
use maho_ext_api::{EvalHandleHost, EvalHandleCallContext, EvalHandleKind, EvalHandleRef, EvalHandlePhase, EvalHandleOutcome};
use maho_omo_task::eval_handle_host::{NativeEvalHandleHost, TaskMutationObserver};
use senpi_task::{manager::{TaskManager, types::{ManagedRunner, ManagedRunnerResult, ManagedStartSpec, ManagedRunners, TaskManagerOptions, ResolvedChildPlan}}, store::{StateDirConfig, TaskRecordStore}, state::{create_task_record, TaskRecordInput, TaskStatus}, workpool::engine::WorkpoolEngine};

struct NoLaunch;
impl ManagedRunner for NoLaunch { fn start(&self, _: &ManagedStartSpec) -> ManagedRunnerResult { panic!("read-only fixture must not launch") } }
fn context() -> EvalHandleCallContext { EvalHandleCallContext { owner_session_id: "parent".into(), signal: None } }

#[tokio::test]
async fn watches_committed_task_settlement_and_rejects_successor_epoch() {
    let root = tempfile::tempdir().expect("root");
    let store = TaskRecordStore::new(&StateDirConfig { project_dir: root.path().into(), task_state_dir: None });
    let manager = Arc::new(TaskManager::new(TaskManagerOptions::new(store.clone(), ManagedRunners { in_process: Arc::new(NoLaunch), process: Arc::new(NoLaunch) }, Arc::new(|_| Ok(ResolvedChildPlan { model: "faux/faux".into(), ..Default::default() })), root.path().to_string_lossy())));
    let observer = Arc::new(TaskMutationObserver::default());
    let changes = observer.clone();
    store.set_mutation_listener(Some(Arc::new(move || changes.notify())));
    let pools = WorkpoolEngine::new(store.state_dir(), maho_omo_task::workpool_admission::create_workpool_admission(manager.clone()));
    let host = NativeEvalHandleHost::new(manager, pools, observer, Arc::new(|| Some("parent".into())));
    let mut record = create_task_record(TaskRecordInput { parent_session_id: "parent".into(), root_session_id: "parent".into(), ..Default::default() }, Some(1)).expect("record");
    record.status = TaskStatus::Running;
    store.save(&record).expect("save running");
    let reference = EvalHandleRef { kind: EvalHandleKind::Agent, id: record.task_id.clone(), run_epoch: record.notification.run_epoch };
    assert!(host.result(reference.clone(), EvalHandleCallContext { owner_session_id: "foreign".into(), signal: None }).await.is_err());
    let mut watch = host.watch(vec![reference.clone()], context()).await.expect("watch");
    assert_eq!(watch.initial[0].phase, EvalHandlePhase::Pending);
    let update = std::future::poll_fn(|cx| watch.updates.as_mut().poll_next(cx));
    record.status = TaskStatus::Completed;
    record.final_response = Some("done".into());
    store.replace(&record).expect("commit completed");
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), update).await.expect("settlement bound").expect("settled update");
    assert_eq!(settled.phase, EvalHandlePhase::Succeeded);
    assert!(matches!(host.result(reference.clone(), context()).await.expect("result"), EvalHandleOutcome::Fulfilled { value, .. } if value == "done"));
    record.notification.run_epoch += 1;
    record.status = TaskStatus::Running;
    store.replace(&record).expect("commit successor");
    assert!(host.result(reference, context()).await.is_err());
    let successor = EvalHandleRef { kind: EvalHandleKind::Agent, id: record.task_id.clone(), run_epoch: record.notification.run_epoch };
    let mut successor_watch = host.watch(vec![successor], context()).await.expect("successor watch");
    record.notification.run_epoch += 1;
    store.replace(&record).expect("replace pending watched run");
    let lost = tokio::time::timeout(std::time::Duration::from_secs(5), std::future::poll_fn(|cx| successor_watch.updates.as_mut().poll_next(cx))).await.expect("lost update bound").expect("lost update");
    assert_eq!(lost.phase, EvalHandlePhase::Lost);
    assert_eq!(lost.host_status, "no_longer_current_run");
    (successor_watch.close)();
    let abort = maho_ai::utils::abort::AbortController::new();
    let mut aborted_watch = host.watch(vec![EvalHandleRef { kind: EvalHandleKind::Agent, id: record.task_id.clone(), run_epoch: record.notification.run_epoch }], EvalHandleCallContext { owner_session_id: "parent".into(), signal: Some(abort.signal()) }).await.expect("abortable watch");
    abort.abort(None);
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5), std::future::poll_fn(|cx| aborted_watch.updates.as_mut().poll_next(cx))).await.expect("abort wake bound").is_none());
    (watch.close)();
    assert!(std::future::poll_fn(|cx| watch.updates.as_mut().poll_next(cx)).await.is_none());
    store.set_mutation_listener(None);
}

#[tokio::test]
async fn shared_pool_mutation_settles_epoch_zero_handle_without_task_mutations() {
    use senpi_task::workpool::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolCreate, WorkpoolMode, WorkpoolSpec, WorkpoolStatus};
    let root = tempfile::tempdir().expect("root");
    let store = TaskRecordStore::new(&StateDirConfig { project_dir: root.path().into(), task_state_dir: None });
    let manager = Arc::new(TaskManager::new(TaskManagerOptions::new(store.clone(), ManagedRunners { in_process: Arc::new(NoLaunch), process: Arc::new(NoLaunch) }, Arc::new(|_| Ok(ResolvedChildPlan { model: "faux/faux".into(), ..Default::default() })), root.path().to_string_lossy())));
    let engine = Arc::new(tokio::sync::Mutex::new(WorkpoolEngine::new(store.state_dir(), maho_omo_task::workpool_admission::create_workpool_admission(manager.clone()))));
    let pool_store = engine.lock().await.store.clone();
    let caller = WorkpoolCaller { session_id: "parent".into(), root_session_id: "parent".into(), depth: 0, cwd: root.path().to_string_lossy().into_owned() };
    let pool = pool_store.create(&caller, &WorkpoolCreate { name: "empty".into(), agent: WorkpoolAgent::Category { prompt: "work".into(), model: None, category: "quick".into() }, mode: Some(WorkpoolMode::Fresh), tools: None }, WorkpoolMode::Fresh, WorkpoolSpec { start: Default::default(), plan: ResolvedChildPlan { model: "faux/faux".into(), ..Default::default() } }).expect("persist empty pool");
    let host = NativeEvalHandleHost::with_shared_workpools(manager, engine, Arc::new(TaskMutationObserver::default()), Arc::new(|| Some("parent".into()))).expect("shared pool host");
    let reference = EvalHandleRef { kind: EvalHandleKind::Workpool, id: pool.pool_id.clone(), run_epoch: 0 };
    let mut watch = host.watch(vec![reference.clone()], context()).await.expect("watch pool");
    assert_eq!(watch.initial[0].phase, EvalHandlePhase::Pending);
    pool_store.mutate(&pool.pool_id, |prior| { let mut next = prior.clone(); next.status = WorkpoolStatus::Closing; next }).expect("close empty pool");
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), std::future::poll_fn(|cx| watch.updates.as_mut().poll_next(cx))).await.expect("pool settlement bound").expect("pool update");
    assert_eq!(settled.phase, EvalHandlePhase::Succeeded);
    assert!(matches!(host.result(reference.clone(), context()).await.expect("pool result"), EvalHandleOutcome::Fulfilled { value, .. } if value == serde_json::json!([])));
    assert!(!host.cancel(reference, context()).await.expect("settled pool cancellation").cancelled);
    (watch.close)();
}

// eval-handle-host.test.ts parity: owner fencing, terminal-before-watch, live cancel, stale cancel, pool cancel and foreign pools.
fn failure_code<T>(result: Result<T, maho_ext_api::ExtensionFailure>) -> String { match result { Err(error) => error.code.unwrap_or_default(), Ok(_) => panic!("expected an eval handle failure") } }
fn task_fixture(root: &std::path::Path) -> (TaskRecordStore, Arc<TaskManager>, NativeEvalHandleHost) {
    let store = TaskRecordStore::new(&StateDirConfig { project_dir: root.into(), task_state_dir: None });
    let manager = Arc::new(TaskManager::new(TaskManagerOptions::new(store.clone(), ManagedRunners { in_process: Arc::new(NoLaunch), process: Arc::new(NoLaunch) }, Arc::new(|_| Ok(ResolvedChildPlan { model: "faux/faux".into(), ..Default::default() })), root.to_string_lossy())));
    let pools = WorkpoolEngine::new(store.state_dir(), maho_omo_task::workpool_admission::create_workpool_admission(manager.clone()));
    let host = NativeEvalHandleHost::new(manager.clone(), pools, Arc::new(TaskMutationObserver::default()), Arc::new(|| Some("parent".into())));
    (store, manager, host)
}
fn saved_task(store: &TaskRecordStore, status: TaskStatus) -> senpi_task::state::TaskRecord {
    let mut record = create_task_record(TaskRecordInput { parent_session_id: "parent".into(), root_session_id: "parent".into(), ..Default::default() }, Some(1)).expect("record");
    record.status = status;
    if status == TaskStatus::Completed { record.final_response = Some("done".into()); }
    store.save(&record).expect("save");
    record
}
fn agent_ref(record: &senpi_task::state::TaskRecord) -> EvalHandleRef { EvalHandleRef { kind: EvalHandleKind::Agent, id: record.task_id.clone(), run_epoch: record.notification.run_epoch } }

#[tokio::test]
async fn foreign_owner_is_forbidden_and_unknown_task_is_not_found() {
    let root = tempfile::tempdir().expect("root");
    let (store, _manager, host) = task_fixture(root.path());
    let record = saved_task(&store, TaskStatus::Running);
    assert_eq!(failure_code(host.watch(vec![agent_ref(&record)], EvalHandleCallContext { owner_session_id: "someone-else".into(), signal: None }).await), "eval_handle_forbidden");
    let unknown = EvalHandleRef { id: "st_0000000000000000000000000".into(), ..agent_ref(&record) };
    assert_eq!(failure_code(host.result(unknown, context()).await), "eval_handle_not_found");
}

#[tokio::test]
async fn run_finished_before_watch_arrives_once_in_initial_without_update() {
    let root = tempfile::tempdir().expect("root");
    let (store, _manager, host) = task_fixture(root.path());
    let record = saved_task(&store, TaskStatus::Completed);
    let mut watch = host.watch(vec![agent_ref(&record)], context()).await.expect("watch");
    assert_eq!(watch.initial.len(), 1);
    assert_eq!(watch.initial[0].phase, EvalHandlePhase::Succeeded);
    let idle = std::future::poll_fn(|cx| std::task::Poll::Ready(watch.updates.as_mut().poll_next(cx).is_pending())).await;
    assert!(idle, "a run settled before the watch must not produce an update");
    assert!(matches!(host.result(agent_ref(&record), context()).await.expect("result"), EvalHandleOutcome::Fulfilled { value, .. } if value == "done"));
    (watch.close)();
}

#[tokio::test]
async fn cancelling_live_run_cancels_it_and_repeat_reports_already_ended() {
    let root = tempfile::tempdir().expect("root");
    let (store, _manager, host) = task_fixture(root.path());
    let record = saved_task(&store, TaskStatus::Running);
    let first = host.cancel(agent_ref(&record), context()).await.expect("cancel live run");
    assert!(first.cancelled);
    assert_eq!(first.phase, EvalHandlePhase::Cancelled);
    assert_eq!(store.load(&record.task_id).expect("load").expect("record").status, TaskStatus::Cancelled);
    let second = host.cancel(agent_ref(&record), context()).await.expect("cancel again");
    assert!(!second.cancelled);
    assert_eq!(second.phase, EvalHandlePhase::Cancelled);
}

#[tokio::test]
async fn cancelling_stale_handle_is_refused_and_successor_keeps_running() {
    let root = tempfile::tempdir().expect("root");
    let (store, _manager, host) = task_fixture(root.path());
    let mut record = saved_task(&store, TaskStatus::Running);
    let stale = agent_ref(&record);
    record.notification.run_epoch += 1;
    store.replace(&record).expect("successor run");
    assert_eq!(failure_code(host.cancel(stale, context()).await), "eval_handle_stale");
    assert_eq!(store.load(&record.task_id).expect("load").expect("record").status, TaskStatus::Running);
}

#[tokio::test]
async fn pool_cancel_through_handle_and_foreign_pool_is_forbidden() {
    use senpi_task::workpool::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolCreate, WorkpoolMode, WorkpoolSpec};
    let root = tempfile::tempdir().expect("root");
    let (store, manager, _) = task_fixture(root.path());
    let engine = Arc::new(tokio::sync::Mutex::new(WorkpoolEngine::new(store.state_dir(), maho_omo_task::workpool_admission::create_workpool_admission(manager.clone()))));
    let pool_store = engine.lock().await.store.clone();
    let create = |session: &str, name: &str| pool_store.create(&WorkpoolCaller { session_id: session.into(), root_session_id: session.into(), depth: 0, cwd: root.path().to_string_lossy().into_owned() }, &WorkpoolCreate { name: name.into(), agent: WorkpoolAgent::Category { prompt: "work".into(), model: None, category: "quick".into() }, mode: Some(WorkpoolMode::Fresh), tools: None }, WorkpoolMode::Fresh, WorkpoolSpec { start: Default::default(), plan: ResolvedChildPlan { model: "faux/faux".into(), ..Default::default() } }).expect("persist pool");
    let owned = create("parent", "owned");
    let foreign = create("other", "foreign");
    let host = NativeEvalHandleHost::with_shared_workpools(manager, engine, Arc::new(TaskMutationObserver::default()), Arc::new(|| Some("parent".into()))).expect("shared pool host");
    let foreign_ref = EvalHandleRef { kind: EvalHandleKind::Workpool, id: foreign.pool_id.clone(), run_epoch: 0 };
    assert_eq!(failure_code(host.watch(vec![foreign_ref], context()).await), "eval_handle_forbidden");
    let reference = EvalHandleRef { kind: EvalHandleKind::Workpool, id: owned.pool_id.clone(), run_epoch: 0 };
    let receipt = host.cancel(reference.clone(), context()).await.expect("cancel pool");
    assert!(receipt.cancelled);
    assert_eq!(receipt.phase, EvalHandlePhase::Cancelled);
    assert!(matches!(host.result(reference, context()).await.expect("pool result"), EvalHandleOutcome::Rejected { error, .. } if error.code == "eval_handle_cancelled"));
}

// pool-refs.ts loadOwnedPool parity: an inspect error other than scope_denied (here store_corrupt) reads as not found.
#[tokio::test]
async fn corrupt_pool_record_reports_not_found() {
    let root = tempfile::tempdir().expect("root");
    let (store, _manager, host) = task_fixture(root.path());
    let id = "wp_00000000000000000000000000000000";
    let dir = store.state_dir().join("workpools");
    std::fs::create_dir_all(&dir).expect("workpool dir");
    std::fs::write(dir.join(format!("{id}.json")), "{").expect("corrupt record");
    let reference = EvalHandleRef {
        kind: EvalHandleKind::Workpool,
        id: id.into(),
        run_epoch: 0,
    };
    match host.result(reference, context()).await {
        Err(error) => {
            assert_eq!(error.code.as_deref(), Some("eval_handle_not_found"));
            assert_eq!(error.message, format!("no workpool {id}"));
        }
        Ok(_) => panic!("expected an eval handle failure"),
    }
}
