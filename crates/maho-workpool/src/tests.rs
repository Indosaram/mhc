//! Acceptance tests of `docs/cleanroom/specs/workpool.md` §10 (one `acceptance_NN_*` function or
//! more per numbered test), over an in-memory [`WorkerHost`]. Asynchronous steps await the exact
//! engine event with a bounded timeout.
use crate::engine::{CreateRequest, PoolEngine, PoolEvent, choose_idle};
use crate::host::{BoxFuture, DeliveryDone, StartedWorker, ToolCatalog, WorkerHost, WorkerStart};
use crate::model::*;
use crate::store::PoolStore;
use crate::tool::{self, Caller};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const S: &str = "parent-session";

#[derive(Default)]
struct FakeState {
    next: u64,
    runs: HashMap<String, u64>,
    ended: HashMap<String, tokio::sync::watch::Sender<u64>>,
    started: Vec<(String, WorkerStart)>,
    follow_ups: Vec<(String, String)>,
    cancelled: Vec<String>,
    deliveries: Vec<(String, String, Value)>,
    pending: Vec<DeliveryDone>,
    hold_acks: bool,
    limit: Option<usize>,
    deny_policy: bool,
    catalog: ToolCatalog,
}

#[derive(Default)]
struct Fake {
    state: Mutex<FakeState>,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn with(f: impl FnOnce(&mut FakeState)) -> Arc<Self> {
        let fake = Self::new();
        f(&mut fake.state.lock().unwrap());
        fake
    }
    fn s(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }
    /// A task known from an earlier process: one run, ended or still active.
    fn seed(&self, task_id: &str, active: bool) {
        let mut s = self.s();
        s.runs.insert(task_id.to_owned(), 1);
        s.ended.insert(task_id.to_owned(), tokio::sync::watch::channel(u64::from(!active)).0);
    }
    /// Ends the current run of `task_id`.
    fn end(&self, task_id: &str) {
        self.s().ended[task_id].send_modify(|n| *n += 1);
    }
    fn active(s: &FakeState, task_id: &str) -> bool {
        s.runs.get(task_id).copied().unwrap_or(0) > *s.ended[task_id].borrow()
    }
    fn started_ids(&self) -> Vec<String> {
        self.s().started.iter().map(|(id, _)| id.clone()).collect()
    }
    fn deliveries(&self) -> Vec<(String, String, Value)> {
        self.s().deliveries.clone()
    }
    fn settle_pending(&self, outcome: Result<(), String>) {
        let pending: Vec<DeliveryDone> = std::mem::take(&mut self.s().pending);
        for done in pending {
            done(outcome.clone());
        }
    }
}

impl WorkerHost for Fake {
    fn check_policy(&self, _agent: &AgentSpec) -> Result<(), String> {
        if self.s().deny_policy { Err("spawning is not allowed here".into()) } else { Ok(()) }
    }
    fn resolve(&self, agent: &AgentSpec) -> Result<Value, String> {
        Ok(json!({"model": agent.model()}))
    }
    fn tool_catalog(&self, _agent: &AgentSpec) -> ToolCatalog {
        self.s().catalog.clone()
    }
    fn tools_available(&self, _tools: &[String]) -> bool {
        true
    }
    fn free_slots(&self) -> Option<usize> {
        let s = self.s();
        let active = s.runs.keys().filter(|id| Self::active(&s, id)).count();
        s.limit.map(|limit| limit.saturating_sub(active))
    }
    fn start(&self, request: &WorkerStart) -> Result<StartedWorker, PoolError> {
        let mut s = self.s();
        s.next += 1;
        let id = format!("worker-{}", s.next);
        s.runs.insert(id.clone(), 1);
        s.ended.insert(id.clone(), tokio::sync::watch::channel(0).0);
        s.started.push((id.clone(), request.clone()));
        Ok(StartedWorker { task_id: id, session_id: None })
    }
    fn follow_up(&self, task_id: &str, message: &str) -> Result<(), PoolError> {
        let mut s = self.s();
        *s.runs.get_mut(task_id).unwrap() += 1;
        s.follow_ups.push((task_id.to_owned(), message.to_owned()));
        Ok(())
    }
    fn wait_run(&self, task_id: &str) -> BoxFuture<()> {
        let s = self.s();
        let target = s.runs[task_id];
        let mut rx = s.ended[task_id].subscribe();
        Box::pin(async move {
            let _ = rx.wait_for(|ended| *ended >= target).await;
        })
    }
    fn run_active(&self, task_id: &str) -> Option<bool> {
        let s = self.s();
        s.runs.contains_key(task_id).then(|| Self::active(&s, task_id))
    }
    fn cancel(&self, task_id: &str) {
        self.s().cancelled.push(task_id.to_owned());
    }
    fn deliver(&self, session: &str, text: String, details: Value, done: DeliveryDone) {
        let hold = {
            let mut s = self.s();
            s.deliveries.push((session.to_owned(), text, details));
            s.hold_acks
        };
        if hold {
            self.s().pending.push(done);
        } else {
            done(Ok(()));
        }
    }
}

fn agent() -> AgentSpec {
    AgentSpec::Category { category: "quick".into(), prompt: "Square the number.".into(), model: None }
}

fn req(name: &str, mode: PoolMode) -> CreateRequest {
    CreateRequest { name: name.into(), agent: agent(), mode, tools: Vec::new() }
}

fn engine(dir: &tempfile::TempDir, fake: &Arc<Fake>) -> PoolEngine {
    PoolEngine::new(PoolStore::in_state_dir(dir.path()), Arc::clone(fake) as Arc<dyn WorkerHost>)
}

fn parent() -> Caller {
    Caller::Parent { session: S.into() }
}

fn worker(pool_id: &str, task_id: &str) -> Caller {
    Caller::Worker { pool_id: pool_id.into(), task_id: task_id.into() }
}

fn call(engine: &PoolEngine, caller: &Caller, params: Value) -> tool::ToolOutcome {
    tool::run(engine, caller, &params)
}

fn code(engine: &PoolEngine, caller: &Caller, params: Value) -> String {
    let outcome = call(engine, caller, params);
    assert!(outcome.is_error, "expected an error, got {}", outcome.text);
    assert!(outcome.text.starts_with(outcome.code().unwrap()), "error text starts with its code");
    outcome.code().unwrap().to_owned()
}

fn items(keys: &[&str]) -> Vec<(String, Value)> {
    keys.iter().enumerate().map(|(i, k)| ((*k).to_owned(), json!(i))).collect()
}

fn yields(engine: &PoolEngine, pool: &str, task: &str, results: Value) -> Vec<Value> {
    engine.yield_results(pool, task, results.as_array().unwrap()).unwrap()
}

async fn until(rx: &mut tokio::sync::broadcast::Receiver<PoolEvent>, want: impl Fn(&PoolEvent) -> bool) -> PoolEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(event) if want(&event) => return event,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("event stream ended: {error}"),
            }
        }
    })
    .await
    .expect("the awaited pool event arrives within the timeout")
}

fn record_files(dir: &tempfile::TempDir) -> usize {
    std::fs::read_dir(dir.path().join("workpools"))
        .map(|entries| entries.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
        .unwrap_or(0)
}

// ---- 1-8: create, push, close, scope -----------------------------------------------------------

#[tokio::test]
async fn acceptance_01_create_defaults_and_id_shape() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let out = call(&e, &parent(), json!({"op": "create", "name": "batch", "agent": {"category": "quick", "prompt": "go"}}));
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(out.details["mode"], "keep_alive");
    assert_eq!(out.details["generation"], 1);
    assert_eq!(out.details["status"], "open");
    let id = out.details["pool_id"].as_str().unwrap();
    assert_eq!(id.len(), 35);
    assert!(id.starts_with("wp_"));
    validate_pool_id(id).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&out.text).unwrap(), out.details);
}

#[tokio::test]
async fn acceptance_02_invalid_create_inputs() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let p = parent();
    let both = json!({"op": "create", "name": "n", "agent": {"category": "quick", "subagent_type": "task", "prompt": "go"}});
    assert_eq!(code(&e, &p, both), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "create", "name": "  ", "agent": {"category": "quick", "prompt": "go"}})), "invalid_input");
    let with_id = json!({"op": "create", "name": "n", "pool_id": "wp_00000000000000000000000000000000", "agent": {"category": "quick", "prompt": "go"}});
    assert_eq!(code(&e, &p, with_id), "invalid_input");
    // Neither kind, extra agent field, blank prompt, bad mode, missing agent, unknown op.
    assert_eq!(code(&e, &p, json!({"op": "create", "name": "n", "agent": {"prompt": "go"}})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "create", "name": "n", "agent": {"category": "q", "prompt": "go", "x": 1}})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "create", "name": "n", "agent": {"category": "q", "prompt": " "}})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "create", "name": "n", "mode": "eager", "agent": {"category": "q", "prompt": "go"}})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "create", "name": "n"})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "explode"})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"name": "n"})), "invalid_input");
    // Pool ids: invalid_input at the tool boundary, invalid_pool_id at the store boundary.
    assert_eq!(code(&e, &p, json!({"op": "inspect", "pool_id": "wp_ABC"})), "invalid_input");
    assert_eq!(e.inspect(S, "wp_ABC").unwrap_err().code, "invalid_pool_id");
    // Items: exactly {key, input}; blank keys refused.
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    assert_eq!(code(&e, &p, json!({"op": "push", "pool_id": pool, "items": [{"key": "a"}]})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "push", "pool_id": pool, "items": [{"key": " ", "input": 1}]})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "push", "pool_id": pool, "items": [{"key": "a", "input": 1, "x": 2}]})), "invalid_input");
    assert_eq!(code(&e, &p, json!({"op": "close", "pool_id": pool, "items": []})), "invalid_input");
    assert_eq!(record_files(&dir), 1);
}

#[tokio::test]
async fn acceptance_03_create_is_idempotent_per_name() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.catalog.parent = vec!["x".into(), "y".into()]));
    let e = engine(&dir, &fake);
    let a = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap();
    assert_eq!(e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id, a.pool_id);
    assert_eq!(e.create(S, &req("n", PoolMode::Fresh)).unwrap_err().code, "pool_name_conflict");
    // Tool sets compare ignoring order and duplicates.
    let mut t1 = req("t", PoolMode::KeepAlive);
    t1.tools = vec!["x".into(), "y".into()];
    let mut t2 = req("t", PoolMode::KeepAlive);
    t2.tools = vec!["y".into(), "x".into(), "x".into()];
    assert_eq!(e.create(S, &t1).unwrap().pool_id, e.create(S, &t2).unwrap().pool_id);
    // A closed pool with that name conflicts.
    e.close(S, &a.pool_id).unwrap();
    assert_eq!(e.create(S, &req("n", PoolMode::KeepAlive)).unwrap_err().code, "pool_name_conflict");
}

#[tokio::test]
async fn acceptance_04_policy_denied_stores_nothing() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.deny_policy = true));
    let e = engine(&dir, &fake);
    let out = call(&e, &parent(), json!({"op": "create", "name": "n", "agent": {"category": "quick", "prompt": "go"}}));
    assert_eq!(out.code(), Some("policy_denied"));
    assert_eq!(record_files(&dir), 0);
}

#[tokio::test]
async fn acceptance_05_push_trims_keys_and_compares_canonically() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.limit = Some(0)));
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    let one = call(&e, &parent(), json!({"op": "push", "pool_id": pool, "items": [{"key": " a ", "input": {"b": 2, "a": 1}}]}));
    assert_eq!(one.details["item_ids"][0]["key"], "a");
    let item_id = one.details["item_ids"][0]["item_id"].as_str().unwrap().to_owned();
    assert!(is_item_id(&item_id));
    let revision = e.inspect(S, &pool).unwrap().revision;
    let two = call(&e, &parent(), json!({"op": "push", "pool_id": pool, "items": [{"key": "a", "input": {"a": 1, "b": 2}}]}));
    assert_eq!(two.details["item_ids"][0]["item_id"], item_id.as_str());
    assert_eq!(e.inspect(S, &pool).unwrap().revision, revision);
}

#[tokio::test]
async fn acceptance_06_conflicting_push_stores_nothing() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.limit = Some(0)));
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &pool, vec![("a".into(), json!(1))]).unwrap();
    let err = e.push(S, &pool, vec![("new".into(), json!(2)), ("a".into(), json!(3))]).unwrap_err();
    assert_eq!(err.code, "duplicate_key_conflict");
    let record = e.inspect(S, &pool).unwrap();
    assert_eq!(record.items.len(), 1);
    assert_eq!(record.items[0].key, "a");
}

#[tokio::test]
async fn acceptance_07_push_after_close_and_empty_push() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    let empty = call(&e, &parent(), json!({"op": "push", "pool_id": pool, "items": []}));
    assert!(!empty.is_error);
    assert_eq!(empty.details["item_ids"], json!([]));
    let closed = call(&e, &parent(), json!({"op": "close", "pool_id": pool}));
    assert_eq!(closed.details["status"], "closing");
    assert_eq!(code(&e, &parent(), json!({"op": "push", "pool_id": pool, "items": []})), "pool_closed");
}

#[tokio::test]
async fn acceptance_08_inspect_scope_and_unknown_pool() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    let other = Caller::Parent { session: "other".into() };
    assert_eq!(code(&e, &other, json!({"op": "inspect", "pool_id": pool})), "scope_denied");
    assert_eq!(code(&e, &other, json!({"op": "cancel", "pool_id": pool})), "scope_denied");
    assert_eq!(code(&e, &parent(), json!({"op": "inspect", "pool_id": "wp_00000000000000000000000000000000"})), "pool_not_found");
    let shape = call(&e, &parent(), json!({"op": "inspect", "pool_id": pool})).details;
    for field in ["pool_id", "name", "mode", "status", "generation", "revision", "items", "workers", "aggregate"] {
        assert!(shape.get(field).is_some(), "inspect has {field}");
    }
}

// ---- 9-12: scope of yield, dispatch modes ------------------------------------------------------

#[tokio::test]
async fn acceptance_09_yield_and_parent_ops_are_scoped() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &pool, items(&["a"])).unwrap();
    let task = fake.started_ids()[0].clone();
    let results = json!({"op": "yield", "results": [{"key": "a", "data": 1}]});
    assert_eq!(code(&e, &parent(), results.clone()), "scope_denied");
    let w = worker(&pool, &task);
    assert_eq!(code(&e, &w, json!({"op": "push", "pool_id": pool, "items": []})), "scope_denied");
    assert_eq!(code(&e, &Caller::Anonymous, json!({"op": "inspect", "pool_id": pool})), "scope_denied");
    assert_eq!(code(&e, &Caller::Anonymous, results), "scope_denied");
    // A session that is not bound to the pool at all.
    let stranger = worker(&pool, "not-a-worker");
    assert_eq!(code(&e, &stranger, json!({"op": "yield", "results": [{"key": "a", "data": 1}]})), "worker_unassigned");
}

#[tokio::test]
async fn acceptance_10_fresh_pool_starts_one_worker_per_item() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::Fresh)).unwrap().pool_id;
    e.push(S, &pool, items(&["a", "b", "c"])).unwrap();
    let ids = fake.started_ids();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
    // Each first prompt carries its item as a JSON assignment.
    let record = e.inspect(S, &pool).unwrap();
    for ((_, start), item) in fake.s().started.iter().zip(&record.items) {
        assert!(start.prompt.starts_with("Square the number."));
        assert!(start.prompt.contains(&item.item_id));
        assert!(start.prompt.contains("\"generation\":1"));
    }
    assert!(record.items.iter().all(|i| i.status == ItemStatus::Assigned));
}

#[tokio::test]
async fn acceptance_11_keep_alive_reuses_one_worker_over_three_epochs() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.limit = Some(1)));
    let e = engine(&dir, &fake);
    let mut rx = e.subscribe();
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &pool, items(&["k1", "k2", "k3"])).unwrap();
    let task = fake.started_ids()[0].clone();
    for (epoch, key) in [(1_u64, "k1"), (2, "k2"), (3, "k3")] {
        let receipts = yields(&e, &pool, &task, json!([{"key": key, "data": epoch}]));
        assert_eq!(receipts[0]["receipt"], "accepted");
        fake.end(&task);
        if epoch < 3 {
            until(&mut rx, |ev| matches!(ev, PoolEvent::Dispatched { run_epoch, .. } if *run_epoch == epoch + 1)).await;
        } else {
            until(&mut rx, |ev| matches!(ev, PoolEvent::RunEnded { .. })).await;
        }
    }
    let record = e.inspect(S, &pool).unwrap();
    assert_eq!(fake.started_ids(), vec![task.clone()]);
    assert_eq!(fake.s().follow_ups.len(), 2);
    assert_eq!(record.workers.len(), 1);
    assert_eq!(record.workers[0].run_epoch, 3);
    assert_eq!(record.workers[0].completed_turns, 3);
    assert_eq!(record.workers[0].status, WorkerStatus::Idle);
    assert!(record.items.iter().all(|i| i.status == ItemStatus::Completed));
}

#[test]
fn acceptance_12_idle_worker_choice() {
    let w = |id: &str, turns, idle| PoolWorker {
        task_id: id.into(),
        run_epoch: 1,
        status: WorkerStatus::Idle,
        completed_turns: turns,
        idle_since: idle,
        session_id: None,
        retired: false,
    };
    let workers = vec![w("z", 1, 2), w("b", 0, 9), w("a", 0, 9)];
    assert_eq!(workers[choose_idle(&workers).unwrap()].task_id, "a");
    let mut busy = workers;
    busy[2].status = WorkerStatus::Busy;
    assert_eq!(busy[choose_idle(&busy).unwrap()].task_id, "b");
}

// ---- 13-16: yield ------------------------------------------------------------------------------

#[tokio::test]
async fn acceptance_13_yield_accepted_duplicate_conflict() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    let item_id = e.push(S, &pool, items(&["k"])).unwrap()[0].1.clone();
    let task = fake.started_ids()[0].clone();
    let w = worker(&pool, &task);
    let first = call(&e, &w, json!({"op": "yield", "results": [{"key": "k", "data": 2}]}));
    assert_eq!(first.details["receipts"][0], json!({"receipt": "accepted", "key": "k", "item_id": item_id}));
    let item = &e.inspect(S, &pool).unwrap().items[0];
    assert_eq!(item.status, ItemStatus::Completed);
    assert_eq!(item.data, Some(json!(2)));
    let again = call(&e, &w, json!({"op": "yield", "results": [{"key": "k", "data": 2}]}));
    assert_eq!(again.details["receipts"][0]["receipt"], "duplicate");
    let other = call(&e, &w, json!({"op": "yield", "results": [{"key": "k", "data": 3}]}));
    assert_eq!(other.details["receipts"][0]["receipt"], "refused");
    assert_eq!(other.details["receipts"][0]["code"], "yield_conflict");
    assert_eq!(e.inspect(S, &pool).unwrap().items[0].data, Some(json!(2)));
    // The accepted yield is in the worker's event log.
    assert_eq!(e.store().events(&task).len(), 1);
}

#[tokio::test]
async fn acceptance_14_malformed_entries_refused_others_accepted() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &pool, items(&["k"])).unwrap();
    let task = fake.started_ids()[0].clone();
    let receipts = yields(&e, &pool, &task, json!([
        {"key": "k", "data": 1, "error": {"code": "x", "message": "y"}},
        {"key": "k", "error": {"code": "x", "message": "y", "extra": true}},
        {"key": "k", "error": {"code": " ", "message": "y"}},
        {"data": 1},
        {"key": "k", "data": 7},
    ]));
    for r in &receipts[..4] {
        assert_eq!(r["receipt"], "refused");
        assert_eq!(r["code"], "invalid_input");
    }
    assert_eq!(receipts[0]["key"], "k");
    assert!(receipts[3].get("key").is_none());
    assert_eq!(receipts[4]["receipt"], "accepted");
    assert_eq!(e.inspect(S, &pool).unwrap().items[0].data, Some(json!(7)));
}

#[tokio::test]
async fn acceptance_15_yield_for_previous_epoch_is_stale() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.limit = Some(1)));
    let e = engine(&dir, &fake);
    let mut rx = e.subscribe();
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &pool, items(&["x", "y"])).unwrap();
    let task = fake.started_ids()[0].clone();
    yields(&e, &pool, &task, json!([{"key": "x", "data": 1}]));
    fake.end(&task);
    until(&mut rx, |ev| matches!(ev, PoolEvent::Dispatched { run_epoch: 2, .. })).await;
    let receipts = yields(&e, &pool, &task, json!([{"key": "x", "data": 1}, {"key": "nope", "data": 1}, {"key": "y", "data": 2}]));
    assert_eq!(receipts[0]["code"], "stale_assignment");
    assert_eq!(receipts[1]["code"], "stale_assignment");
    assert_eq!(receipts[2]["receipt"], "accepted");
}

#[tokio::test]
async fn acceptance_16_run_end_without_yield_is_item_missing_yield() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let mut rx = e.subscribe();
    let pool = e.create(S, &req("n", PoolMode::Fresh)).unwrap().pool_id;
    e.push(S, &pool, items(&["k"])).unwrap();
    let task = fake.started_ids()[0].clone();
    fake.end(&task);
    until(&mut rx, |ev| matches!(ev, PoolEvent::RunEnded { task_id, .. } if *task_id == task)).await;
    let record = e.inspect(S, &pool).unwrap();
    assert_eq!(record.items[0].status, ItemStatus::Error);
    assert_eq!(record.items[0].error.as_ref().unwrap().code, "item_missing_yield");
    assert_eq!(record.workers[0].status, WorkerStatus::Idle);
    assert_eq!(record.workers[0].completed_turns, 1);
    // After the run ended the worker's binding is stale.
    assert_eq!(e.yield_results(&pool, &task, &[json!({"key": "k", "data": 1})]).unwrap()[0]["code"], "stale_assignment");
}

// ---- 17-20: aggregate --------------------------------------------------------------------------

#[tokio::test]
async fn acceptance_17_close_empty_pool_delivers_empty_aggregate() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.close(S, &pool).unwrap();
    let deliveries = fake.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].0, S);
    assert_eq!(serde_json::from_str::<Value>(&deliveries[0].1).unwrap(), json!([]));
    assert_eq!(deliveries[0].2, json!({"pool_id": pool, "generation": 1, "results": []}));
    let aggregate = e.inspect(S, &pool).unwrap().aggregate.unwrap();
    assert_eq!((aggregate.generation, aggregate.delivered, aggregate.accepted), (1, true, true));
    e.close(S, &pool).unwrap();
    assert_eq!(fake.deliveries().len(), 1, "a confirmed aggregate is never sent twice");
}

#[tokio::test]
async fn acceptance_18_aggregate_in_push_order_with_errors() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::Fresh)).unwrap().pool_id;
    e.push(S, &pool, items(&["b", "a"])).unwrap();
    let ids = fake.started_ids();
    yields(&e, &pool, &ids[0], json!([{"key": "b", "data": {"v": 1}}]));
    yields(&e, &pool, &ids[1], json!([{"key": "a", "error": {"code": "bad_input", "message": "not a number"}}]));
    e.close(S, &pool).unwrap();
    let deliveries = fake.deliveries();
    assert_eq!(deliveries.len(), 1);
    let expected = json!([
        {"key": "b", "data": {"v": 1}},
        {"key": "a", "error": {"code": "bad_input", "message": "not a number"}},
    ]);
    assert_eq!(serde_json::from_str::<Value>(&deliveries[0].1).unwrap(), expected);
    assert_eq!(deliveries[0].2["results"], expected);
}

#[tokio::test]
async fn acceptance_19_no_aggregate_while_items_queued_or_assigned() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.limit = Some(1)));
    let e = engine(&dir, &fake);
    let mut rx = e.subscribe();
    let pool = e.create(S, &req("n", PoolMode::Fresh)).unwrap().pool_id;
    e.push(S, &pool, items(&["a", "b"])).unwrap();
    e.close(S, &pool).unwrap();
    let record = e.inspect(S, &pool).unwrap();
    assert_eq!(record.items[0].status, ItemStatus::Assigned);
    assert_eq!(record.items[1].status, ItemStatus::Queued);
    assert!(fake.deliveries().is_empty());
    let first = fake.started_ids()[0].clone();
    yields(&e, &pool, &first, json!([{"key": "a", "data": 1}]));
    assert!(fake.deliveries().is_empty(), "b is still queued");
    fake.end(&first);
    until(&mut rx, |ev| matches!(ev, PoolEvent::Dispatched { task_id, .. } if *task_id != first)).await;
    assert!(fake.deliveries().is_empty(), "b is assigned");
    let second = fake.started_ids()[1].clone();
    yields(&e, &pool, &second, json!([{"key": "b", "data": 2}]));
    assert_eq!(fake.deliveries().len(), 1);
}

#[tokio::test]
async fn acceptance_20_aggregate_redelivery_on_session_start() {
    // Acknowledged: never delivered again.
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let acked = e.create(S, &req("acked", PoolMode::KeepAlive)).unwrap().pool_id;
    e.close(S, &acked).unwrap();
    assert_eq!(fake.deliveries().len(), 1);
    drop(e);
    let fake2 = Fake::new();
    engine(&dir, &fake2).reattach(S);
    assert!(fake2.deliveries().is_empty());

    // Accepted but unacknowledged: delivered exactly once on the next start.
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.hold_acks = true));
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("pending", PoolMode::KeepAlive)).unwrap().pool_id;
    e.close(S, &pool).unwrap();
    assert_eq!(fake.deliveries().len(), 1);
    let aggregate = e.inspect(S, &pool).unwrap().aggregate.unwrap();
    assert!(aggregate.accepted && !aggregate.delivered);
    drop(e);
    let fake2 = Fake::with(|s| s.hold_acks = true);
    let e2 = engine(&dir, &fake2);
    let mut rx = e2.subscribe();
    e2.reattach(S);
    e2.reattach(S);
    assert_eq!(fake2.deliveries().len(), 1);
    fake2.settle_pending(Ok(()));
    until(&mut rx, |ev| matches!(ev, PoolEvent::AggregateDelivered { .. })).await;
    assert!(e2.inspect(S, &pool).unwrap().aggregate.unwrap().delivered);
    let fake3 = Fake::new();
    engine(&dir, &fake3).reattach(S);
    assert!(fake3.deliveries().is_empty());
}

#[tokio::test]
async fn aggregate_failure_waits_for_the_next_flush_point() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.hold_acks = true));
    let e = engine(&dir, &fake);
    let mut rx = e.subscribe();
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.close(S, &pool).unwrap();
    fake.settle_pending(Err("session busy".into()));
    until(&mut rx, |ev| matches!(ev, PoolEvent::AggregateFailed { .. })).await;
    let aggregate = e.inspect(S, &pool).unwrap().aggregate.unwrap();
    assert!(!aggregate.accepted && !aggregate.delivered);
    assert_eq!(fake.deliveries().len(), 1, "inspect is not a flush point");
    e.close(S, &pool).unwrap();
    assert_eq!(fake.deliveries().len(), 2, "a repeated close retries");
}

// ---- 21-25: cancel, restart, corruption, hold, tools ------------------------------------------

#[tokio::test]
async fn acceptance_21_cancel_with_busy_worker() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::with(|s| s.limit = Some(1)));
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &pool, items(&["a", "b"])).unwrap();
    let task = fake.started_ids()[0].clone();
    let out = call(&e, &parent(), json!({"op": "cancel", "pool_id": pool}));
    assert_eq!(out.details, json!({"pool_id": pool, "status": "cancelled", "generation": 2}));
    assert_eq!(fake.s().cancelled, vec![task.clone()]);
    let record = e.inspect(S, &pool).unwrap();
    for item in &record.items {
        assert_eq!(item.status, ItemStatus::Cancelled);
        assert_eq!(item.error.as_ref().unwrap().code, "cancelled");
    }
    assert!(fake.deliveries().is_empty(), "cancel delivers no aggregate");
    assert_eq!(e.yield_results(&pool, &task, &[json!({"key": "a", "data": 1})]).unwrap_err().code, "stale_assignment");
    let again = e.cancel(S, &pool).unwrap();
    assert_eq!(again.revision, record.revision);
    assert_eq!(again.generation, 2);
    assert_eq!(fake.s().cancelled.len(), 1);
}

#[tokio::test]
async fn acceptance_22_restart_keeps_items_and_recovers_logged_yields() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::new();
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    let ids = e.push(S, &pool, items(&["a", "b"])).unwrap();
    let tasks = fake.started_ids();
    let before = e.inspect(S, &pool).unwrap();
    // The host process dies; a worker's yield still lands in its own log.
    drop(e);
    let view = PoolEngine::worker_view(PoolStore::in_state_dir(dir.path()));
    let receipts = view.yield_results(&pool, &tasks[0], &[json!({"key": "a", "data": "done"})]).unwrap();
    assert_eq!(receipts[0]["receipt"], "accepted");
    // A new host on the same state dir: worker 1 finished, worker 2 is gone.
    let fake2 = Fake::new();
    fake2.seed(&tasks[0], false);
    let e2 = engine(&dir, &fake2);
    e2.reattach(S);
    let after = e2.inspect(S, &pool).unwrap();
    let item_ids: Vec<&str> = after.items.iter().map(|i| i.item_id.as_str()).collect();
    assert_eq!(item_ids, ids.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>());
    assert_eq!(after.items.iter().map(|i| &i.key).collect::<Vec<_>>(), before.items.iter().map(|i| &i.key).collect::<Vec<_>>());
    assert_eq!(after.items[0].status, ItemStatus::Completed);
    assert_eq!(after.items[0].data, Some(json!("done")));
    assert_eq!(after.items[1].error.as_ref().unwrap().code, "delivery_uncertain");
    assert_eq!(after.worker(&tasks[0]).unwrap().status, WorkerStatus::Idle);
}

#[tokio::test]
async fn acceptance_23_corrupt_record_is_store_corrupt() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let pool = e.create(S, &req("n", PoolMode::KeepAlive)).unwrap().pool_id;
    std::fs::write(e.store().record_path(&pool), "{ not json").unwrap();
    assert_eq!(code(&e, &parent(), json!({"op": "inspect", "pool_id": pool})), "store_corrupt");
    assert_eq!(e.push(S, &pool, Vec::new()).unwrap_err().code, "store_corrupt");
    assert!(e.store().record_path(&pool).exists(), "a corrupt record is never dropped");
}

#[tokio::test]
async fn acceptance_24_stop_holds_pools_until_matching_resume() {
    let (dir, fake) = (tempfile::tempdir().unwrap(), Fake::new());
    let e = engine(&dir, &fake);
    let busy = e.create(S, &req("busy", PoolMode::KeepAlive)).unwrap().pool_id;
    e.push(S, &busy, items(&["a"])).unwrap();
    assert_eq!(fake.started_ids().len(), 1);
    e.hold(S, 5);
    assert_eq!(e.inspect(S, &busy).unwrap().workers[0].status, WorkerStatus::Paused);
    e.push(S, &busy, vec![("b".into(), json!(1))]).unwrap();
    let empty = e.create(S, &req("empty", PoolMode::KeepAlive)).unwrap().pool_id;
    e.close(S, &empty).unwrap();
    assert_eq!(fake.started_ids().len(), 1, "nothing is dispatched while held");
    assert!(fake.deliveries().is_empty(), "no aggregate while held");
    assert!(!e.release(S, 4), "a resume for an older stop is ignored");
    e.push(S, &busy, Vec::new()).unwrap();
    assert_eq!(fake.started_ids().len(), 1);
    assert!(fake.deliveries().is_empty());
    assert!(e.release(S, 5));
    assert_eq!(fake.started_ids().len(), 2, "dispatch continues after resume");
    assert_eq!(fake.deliveries().len(), 1);
    assert_eq!(e.inspect(S, &busy).unwrap().workers[0].status, WorkerStatus::Busy);
}

#[tokio::test]
async fn acceptance_25_extra_tool_checks() {
    let fake = Fake::with(|s| {
        s.catalog = ToolCatalog {
            parent: vec!["lookup".into(), "read".into(), "secret".into()],
            worker: vec!["read".into()],
            denied: vec!["secret".into()],
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let e = engine(&dir, &fake);
    let create = |tools: Value| json!({"op": "create", "name": "t", "agent": {"category": "quick", "prompt": "go"}, "tools": tools});
    assert_eq!(code(&e, &parent(), create(json!(["missing_tool"]))), "kernel_tool_missing");
    assert_eq!(code(&e, &parent(), create(json!(["read"]))), "tool_name_collision");
    assert_eq!(code(&e, &parent(), create(json!(["workpool"]))), "tool_name_collision");
    assert_eq!(code(&e, &parent(), create(json!(["secret"]))), "policy_denied");
    assert_eq!(record_files(&dir), 0);
    let ok = call(&e, &parent(), create(json!(["lookup"])));
    assert!(!ok.is_error, "{}", ok.text);
    let pool = ok.details["pool_id"].as_str().unwrap().to_owned();
    e.push(S, &pool, items(&["a"])).unwrap();
    assert_eq!(fake.s().started[0].1.tools, vec!["lookup".to_owned()]);
}
