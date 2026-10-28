//! The pool engine: lifecycle (spec §3), dispatch (§4), yield (§5), extra tools (§6), aggregate
//! delivery (§7), restart and pause (§8).
//!
//! One engine owns the pools of one task state dir. Every operation loads the record from disk,
//! changes it under the engine's lock and writes it back, so a corrupt record is reported on the
//! next touch and restarts lose nothing. Worker processes use a *worker view* engine
//! ([`PoolEngine::worker_view`]) that only appends yields to the worker's own event log; the owner
//! engine folds those logs into the record. Each file therefore has a single writer process.
use crate::host::{BoxFuture, DeliveryDone, WorkerHost, WorkerStart};
use crate::model::{
    AgentSpec, AggregateState, Binding, ItemError, ItemStatus, PoolError, PoolItem, PoolMode,
    PoolRecord, PoolStatus, PoolWorker, WorkerStatus, canonical, codes, new_item_id, new_pool_id,
    now_ms,
};
use crate::store::{PoolStore, YieldEvent};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

/// `custom_type` of the aggregate message (spec §7, interface fact).
pub const AGGREGATE_MESSAGE_TYPE: &str = "workpool-aggregate";

/// Observable engine events (used for notices and by tests to await exact state changes).
#[derive(Debug, Clone, PartialEq)]
pub enum PoolEvent {
    Dispatched { pool_id: String, item_id: String, task_id: String, run_epoch: u64 },
    RunEnded { pool_id: String, task_id: String },
    AggregateAccepted { pool_id: String, generation: u64 },
    AggregateDelivered { pool_id: String, generation: u64 },
    AggregateFailed { pool_id: String, generation: u64, reason: String },
    Released { session: String },
}

/// Validated `create` input.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateRequest {
    pub name: String,
    pub agent: AgentSpec,
    pub mode: PoolMode,
    pub tools: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// A state change (yield, run end, release): sends a first aggregate only.
    Auto,
    /// A repeated `close`: also retries one whose delivery was reported failed.
    Close,
    /// Session start: also redelivers an accepted, unconfirmed one (once).
    Start,
}

#[derive(Default)]
struct Runtime {
    /// Parent session -> stop sequence of the active hold.
    holds: HashMap<String, u64>,
    stopped: bool,
    /// Worker tasks with a run watcher.
    watching: HashSet<String>,
    /// Pools whose aggregate delivery is awaiting the host's answer in this process.
    in_flight: HashSet<String>,
}

struct Inner {
    store: PoolStore,
    host: Option<Arc<dyn WorkerHost>>,
    io: Mutex<()>,
    runtime: Mutex<Runtime>,
    events: tokio::sync::broadcast::Sender<PoolEvent>,
}

/// See the module docs.
#[derive(Clone)]
pub struct PoolEngine {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn spawn_detached(future: BoxFuture<()>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => drop(handle.spawn(future)),
        Err(_) => {
            let spawned = std::thread::Builder::new().name("workpool-watch".into()).spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    runtime.block_on(future);
                }
            });
            if let Err(error) = spawned {
                maho_ext_api::diagnostic(&format!("workpool: cannot watch a worker run: {error}"));
            }
        }
    }
}

fn not_found(pool_id: &str) -> PoolError {
    PoolError::new(codes::POOL_NOT_FOUND, format!("no pool {pool_id}"))
}

fn nonblank(value: &str, what: &str) -> Result<String, PoolError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(PoolError::new(codes::INVALID_INPUT, format!("{what} must not be blank")))
    } else {
        Ok(trimmed.to_owned())
    }
}

/// Trimmed copy of an agent spec; blank strings are `invalid_input`.
///
/// # Errors
/// A blank `category`/`subagent_type`, `prompt` or `model`.
pub fn normalize_agent(agent: &AgentSpec) -> Result<AgentSpec, PoolError> {
    let model = |m: &Option<String>| m.as_deref().map(|m| nonblank(m, "agent.model")).transpose();
    Ok(match agent {
        AgentSpec::Category { category, prompt, model: m } => AgentSpec::Category {
            category: nonblank(category, "agent.category")?,
            prompt: nonblank(prompt, "agent.prompt")?,
            model: model(m)?,
        },
        AgentSpec::Subagent { subagent_type, prompt, model: m } => AgentSpec::Subagent {
            subagent_type: nonblank(subagent_type, "agent.subagent_type")?,
            prompt: nonblank(prompt, "agent.prompt")?,
            model: model(m)?,
        },
    })
}

/// Index of the idle worker an item goes to: fewest `completed_turns`, then oldest
/// `idle_since`, then task id (spec §4).
pub fn choose_idle(workers: &[PoolWorker]) -> Option<usize> {
    workers
        .iter()
        .enumerate()
        .filter(|(_, w)| w.status == WorkerStatus::Idle && !w.retired)
        .min_by(|(_, a), (_, b)| {
            (a.completed_turns, a.idle_since, &a.task_id).cmp(&(b.completed_turns, b.idle_since, &b.task_id))
        })
        .map(|(index, _)| index)
}

/// The assignment message of one item (spec §4).
pub fn assignment_message(record: &PoolRecord, item: &PoolItem) -> String {
    let payload = json!({
        "pool_id": record.pool_id,
        "generation": record.generation,
        "items": [{"item_id": item.item_id, "key": item.key, "input": item.input}],
    });
    format!(
        "Work pool assignment:\n{payload}\n\nFor every item above, report its result with the `workpool` tool, \
op \"yield\": results [{{\"key\": <key>, \"data\": <result>}}] on success or \
[{{\"key\": <key>, \"error\": {{\"code\": <code>, \"message\": <text>}}}}] on failure. \
Report each key exactly once. Do the work yourself; do not delegate it to other agents."
    )
}

/// The aggregate results in push order (spec §7).
pub fn aggregate_results(record: &PoolRecord) -> Vec<Value> {
    record.items.iter().map(PoolItem::result_json).collect()
}

/// A validated yield entry: `(key, {data} | {error})`.
fn parse_result(entry: &Value) -> Result<(String, Value), (Option<String>, String)> {
    let key_of = |entry: &Value| {
        entry.get("key").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned)
    };
    let fail = |message: &str| Err((key_of(entry), message.to_owned()));
    let Some(object) = entry.as_object() else { return fail("a result must be an object") };
    let Some(key) = key_of(entry) else { return fail("result.key must be a non-blank string") };
    if object.len() != 2 {
        return fail("a result has exactly two fields: key and data, or key and error");
    }
    if let Some(data) = object.get("data") {
        return Ok((key, json!({"data": data})));
    }
    let Some(error) = object.get("error").and_then(Value::as_object) else {
        return fail("a result has exactly two fields: key and data, or key and error");
    };
    let text = |name: &str| {
        error.get(name).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
    };
    match (error.len(), text("code"), text("message")) {
        (2, Some(code), Some(message)) => Ok((key, json!({"error": {"code": code, "message": message}}))),
        _ => fail("result.error has exactly two non-blank string fields: code and message"),
    }
}

fn apply_result(item: &mut PoolItem, result: &Value) {
    if let Some(data) = result.get("data") {
        item.status = ItemStatus::Completed;
        item.data = Some(data.clone());
        item.error = None;
    } else if let Some(error) = result.get("error") {
        let field = |name: &str| error.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
        item.status = ItemStatus::Error;
        item.data = None;
        item.error = Some(ItemError { code: field("code"), message: field("message") });
    }
}

fn is_uncertain(item: &PoolItem) -> bool {
    item.status == ItemStatus::Error && item.error.as_ref().is_some_and(|e| e.code == codes::DELIVERY_UNCERTAIN)
}

fn receipt(kind: &str, key: &str, item_id: &str) -> Value {
    json!({"receipt": kind, "key": key, "item_id": item_id})
}

fn refused(key: Option<&str>, code: &str, message: &str) -> Value {
    let mut out = json!({"receipt": "refused", "code": code, "message": message});
    if let Some(key) = key {
        out["key"] = json!(key);
    }
    out
}

impl PoolEngine {
    /// The owner engine of a state dir: dispatches through `host` and delivers aggregates.
    pub fn new(store: PoolStore, host: Arc<dyn WorkerHost>) -> Self {
        Self::build(store, Some(host))
    }

    /// A worker process's view: `yield` appends to the worker's log; nothing is dispatched,
    /// written to pool records or delivered.
    pub fn worker_view(store: PoolStore) -> Self {
        Self::build(store, None)
    }

    fn build(store: PoolStore, host: Option<Arc<dyn WorkerHost>>) -> Self {
        let (events, _) = tokio::sync::broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                store,
                host,
                io: Mutex::default(),
                runtime: Mutex::default(),
                events,
            }),
        }
    }

    pub fn store(&self) -> &PoolStore {
        &self.inner.store
    }

    /// Subscribes to engine events; subscribe before triggering the change you await.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<PoolEvent> {
        self.inner.events.subscribe()
    }

    fn emit(&self, events: Vec<PoolEvent>) {
        for event in events {
            let _ = self.inner.events.send(event);
        }
    }

    fn load_owned(&self, session: &str, pool_id: &str) -> Result<PoolRecord, PoolError> {
        let record = self.inner.store.load(pool_id)?.ok_or_else(|| not_found(pool_id))?;
        if record.parent_session != session {
            return Err(PoolError::new(codes::SCOPE_DENIED, "the pool belongs to another session"));
        }
        Ok(record)
    }

    fn commit(&self, record: &mut PoolRecord) -> Result<(), PoolError> {
        record.revision += 1;
        self.inner.store.save(record)
    }

    fn is_held(&self, session: &str) -> bool {
        lock(&self.inner.runtime).holds.contains_key(session)
    }

    fn is_stopped(&self) -> bool {
        lock(&self.inner.runtime).stopped
    }

    // ---- create / push / close / cancel / inspect -------------------------------------------

    /// `create` (spec §3): policy, resolution and tool binding, then idempotent lookup by name.
    ///
    /// # Errors
    /// `invalid_input`, `policy_denied`, `admission_refused`, `kernel_tool_missing`,
    /// `tool_name_collision`, `pool_name_conflict`, `store_corrupt`.
    pub fn create(&self, session: &str, request: &CreateRequest) -> Result<PoolRecord, PoolError> {
        let host = self.inner.host.as_ref().ok_or_else(|| PoolError::new(codes::SCOPE_DENIED, "workers cannot create pools"))?;
        let name = nonblank(&request.name, "name")?;
        let agent = normalize_agent(&request.agent)?;
        let mut tools = BTreeSet::new();
        for tool in &request.tools {
            tools.insert(nonblank(tool, "tools[]")?);
        }
        let tools: Vec<String> = tools.into_iter().collect();
        host.check_policy(&agent).map_err(|message| PoolError::new(codes::POLICY_DENIED, message))?;
        let resolved = host.resolve(&agent).map_err(|message| PoolError::new(codes::ADMISSION_REFUSED, message))?;
        if !tools.is_empty() {
            let catalog = host.tool_catalog(&agent);
            for tool in &tools {
                if tool == crate::tool::TOOL_NAME || catalog.worker.contains(tool) {
                    return Err(PoolError::new(codes::TOOL_NAME_COLLISION, format!("the worker already has a tool named {tool}")));
                }
                if !catalog.parent.contains(tool) {
                    return Err(PoolError::new(codes::KERNEL_TOOL_MISSING, format!("no tool named {tool} in this session")));
                }
                if catalog.denied.contains(tool) {
                    return Err(PoolError::new(codes::POLICY_DENIED, format!("the worker's tool policy denies {tool}")));
                }
            }
        }
        let _io = lock(&self.inner.io);
        for (_, loaded) in self.inner.store.load_all() {
            let Ok(existing) = loaded else { continue };
            if existing.parent_session != session || existing.name != name {
                continue;
            }
            if existing.status == PoolStatus::Open
                && existing.agent == agent
                && existing.mode == request.mode
                && existing.resolved == resolved
                && existing.tools == tools
            {
                return Ok(existing);
            }
            return Err(PoolError::new(codes::POOL_NAME_CONFLICT, format!("a pool named {name} already exists with a different configuration or is no longer open")));
        }
        let record = PoolRecord {
            pool_id: new_pool_id(),
            parent_session: session.to_owned(),
            name,
            agent,
            mode: request.mode,
            status: PoolStatus::Open,
            generation: 1,
            revision: 0,
            items: Vec::new(),
            workers: Vec::new(),
            aggregate: None,
            tools,
            resolved,
        };
        self.inner.store.save(&record)?;
        Ok(record)
    }

    /// `push` (spec §3): returns `(key, item_id)` in request order once persisted.
    ///
    /// # Errors
    /// `invalid_input`, `pool_not_found`, `scope_denied`, `pool_closed`,
    /// `duplicate_key_conflict`, `store_corrupt`.
    pub fn push(&self, session: &str, pool_id: &str, items: Vec<(String, Value)>) -> Result<Vec<(String, String)>, PoolError> {
        let ids = {
            let _io = lock(&self.inner.io);
            let mut record = self.load_owned(session, pool_id)?;
            if record.status != PoolStatus::Open {
                return Err(PoolError::new(codes::POOL_CLOSED, format!("pool {pool_id} is {}", record.status.as_str())));
            }
            let mut known: HashMap<String, (String, String)> = record
                .items
                .iter()
                .map(|i| (i.key.clone(), (i.item_id.clone(), canonical(&i.input))))
                .collect();
            let mut added = Vec::new();
            let mut ids = Vec::new();
            for (key, input) in items {
                let key = nonblank(&key, "items[].key")?;
                let text = canonical(&input);
                if let Some((item_id, existing)) = known.get(&key) {
                    if *existing != text {
                        return Err(PoolError::new(codes::DUPLICATE_KEY_CONFLICT, format!("key {key} is already in the pool with different input")));
                    }
                    ids.push((key, item_id.clone()));
                    continue;
                }
                let item_id = new_item_id();
                known.insert(key.clone(), (item_id.clone(), text));
                ids.push((key.clone(), item_id.clone()));
                added.push(PoolItem { key, input, item_id, status: ItemStatus::Queued, data: None, error: None, binding: None });
            }
            if !added.is_empty() {
                record.items.extend(added);
                self.commit(&mut record)?;
            }
            ids
        };
        self.pump(pool_id);
        Ok(ids)
    }

    /// `close` (spec §3); also a flush point for a failed aggregate (§7).
    ///
    /// # Errors
    /// `pool_not_found`, `scope_denied`, `store_corrupt`.
    pub fn close(&self, session: &str, pool_id: &str) -> Result<PoolRecord, PoolError> {
        let record = {
            let _io = lock(&self.inner.io);
            let mut record = self.load_owned(session, pool_id)?;
            if record.status == PoolStatus::Open {
                record.status = PoolStatus::Closing;
                self.commit(&mut record)?;
            }
            record
        };
        self.maybe_deliver(pool_id, Flush::Close);
        Ok(record)
    }

    /// `cancel` (spec §3).
    ///
    /// # Errors
    /// `pool_not_found`, `scope_denied`, `store_corrupt`.
    pub fn cancel(&self, session: &str, pool_id: &str) -> Result<PoolRecord, PoolError> {
        let (record, cancel) = {
            let _io = lock(&self.inner.io);
            let mut record = self.load_owned(session, pool_id)?;
            if record.status == PoolStatus::Cancelled {
                return Ok(record);
            }
            self.fold_logs(&mut record);
            record.status = PoolStatus::Cancelled;
            record.generation += 1;
            for item in &mut record.items {
                if matches!(item.status, ItemStatus::Queued | ItemStatus::Assigned) {
                    item.status = ItemStatus::Cancelled;
                    item.data = None;
                    item.error = Some(ItemError::new(codes::CANCELLED, "the pool was cancelled"));
                }
            }
            let mut cancel = Vec::new();
            for worker in &mut record.workers {
                if worker.status != WorkerStatus::Idle {
                    cancel.push(worker.task_id.clone());
                }
                worker.retired = true;
            }
            self.commit(&mut record)?;
            (record, cancel)
        };
        if let Some(host) = &self.inner.host {
            for task_id in cancel {
                host.cancel(&task_id);
            }
        }
        Ok(record)
    }

    /// `inspect` (spec §1): the record with every worker log folded in.
    ///
    /// # Errors
    /// `pool_not_found`, `scope_denied`, `store_corrupt`.
    pub fn inspect(&self, session: &str, pool_id: &str) -> Result<PoolRecord, PoolError> {
        let record = {
            let _io = lock(&self.inner.io);
            let mut record = self.load_owned(session, pool_id)?;
            if self.inner.host.is_some() && self.fold_logs(&mut record) {
                self.commit(&mut record)?;
            }
            record
        };
        self.maybe_deliver(pool_id, Flush::Auto);
        Ok(record)
    }

    // ---- yield --------------------------------------------------------------------------------

    /// Applies logged yields of every worker to items still waiting for them. Returns whether
    /// the record changed.
    fn fold_logs(&self, record: &mut PoolRecord) -> bool {
        let delivered_gen = record.aggregate.as_ref().filter(|a| a.accepted || a.delivered).map(|a| a.generation);
        let mut changed = false;
        let tasks: Vec<String> = record.workers.iter().map(|w| w.task_id.clone()).collect();
        for task_id in tasks {
            for event in self.inner.store.events(&task_id) {
                if event.pool_id != record.pool_id || event.generation != record.generation {
                    continue;
                }
                let Some(item) = record.items.iter_mut().find(|i| i.item_id == event.item_id) else { continue };
                let bound = item.binding.as_ref().is_some_and(|b| {
                    (b.task_id == event.task_id || (b.task_id.is_empty() && b.run_epoch == 1))
                        && b.run_epoch == event.run_epoch
                        && b.generation == event.generation
                });
                let waiting = item.status == ItemStatus::Assigned
                    || (is_uncertain(item) && delivered_gen != Some(record.generation));
                if bound && waiting {
                    apply_result(item, &event.result);
                    changed = true;
                }
            }
        }
        changed
    }

    /// `yield` from worker task `task_id` (spec §5): per-entry receipts in request order.
    ///
    /// # Errors
    /// `pool_not_found`, `worker_unassigned`, `stale_assignment`, `store_corrupt`.
    pub fn yield_results(&self, pool_id: &str, task_id: &str, results: &[Value]) -> Result<Vec<Value>, PoolError> {
        let owner = self.inner.host.is_some();
        let receipts = {
            let _io = lock(&self.inner.io);
            let mut record = self.inner.store.load(pool_id)?.ok_or_else(|| not_found(pool_id))?;
            let mut changed = owner && self.fold_logs(&mut record);
            if record.status == PoolStatus::Cancelled {
                if changed {
                    self.commit(&mut record)?;
                }
                return Err(PoolError::new(codes::STALE_ASSIGNMENT, "the pool was cancelled"));
            }
            // A worker whose start is still being committed by the owner yields against the
            // pending (empty-task) binding of run epoch 1.
            let pending = |b: &Binding| b.task_id.is_empty() && b.run_epoch == 1;
            let run_epoch = match record.worker(task_id) {
                Some(worker) => worker.run_epoch,
                None if record.items.iter().any(|i| i.binding.as_ref().is_some_and(pending)) => 1,
                None => return Err(PoolError::new(codes::WORKER_UNASSIGNED, "this session has no work pool assignment")),
            };
            let generation = record.generation;
            let current = |b: &Binding| (b.task_id == task_id || pending(b)) && b.run_epoch == run_epoch && b.generation == generation;
            if !record.items.iter().any(|i| i.binding.as_ref().is_some_and(|b| b.task_id == task_id || pending(b))) {
                return Err(PoolError::new(codes::WORKER_UNASSIGNED, "this worker has no assignment"));
            }
            if !record.items.iter().any(|i| i.binding.as_ref().is_some_and(current)) {
                return Err(PoolError::new(codes::STALE_ASSIGNMENT, "this worker's assignment belongs to an earlier run or generation"));
            }
            let mut logged: HashMap<String, Value> = self
                .inner
                .store
                .events(task_id)
                .into_iter()
                .filter(|e| e.pool_id == record.pool_id && e.generation == generation && e.run_epoch == run_epoch)
                .map(|e| (e.item_id, e.result))
                .collect();
            let mut receipts = Vec::new();
            for entry in results {
                let (key, result) = match parse_result(entry) {
                    Ok(parsed) => parsed,
                    Err((key, message)) => {
                        receipts.push(refused(key.as_deref(), codes::INVALID_INPUT, &message));
                        continue;
                    }
                };
                let Some(index) = record.items.iter().position(|i| i.key == key) else {
                    receipts.push(refused(Some(&key), codes::STALE_ASSIGNMENT, "this key is not assigned to this run"));
                    continue;
                };
                let item_id = record.items[index].item_id.clone();
                let bound_now = record.items[index].binding.as_ref().is_some_and(current);
                if bound_now && let Some(previous) = logged.get(&item_id) {
                    if canonical(previous) == canonical(&result) {
                        receipts.push(receipt("duplicate", &key, &item_id));
                    } else {
                        receipts.push(refused(Some(&key), codes::YIELD_CONFLICT, "this key was already reported with a different result"));
                    }
                    continue;
                }
                let item = &record.items[index];
                let assignable = item.binding.as_ref().is_some_and(current)
                    && (item.status == ItemStatus::Assigned || is_uncertain(item));
                if !assignable {
                    receipts.push(refused(Some(&key), codes::STALE_ASSIGNMENT, "this key is not assigned to this run"));
                    continue;
                }
                let event = YieldEvent {
                    pool_id: record.pool_id.clone(),
                    generation: record.generation,
                    task_id: task_id.to_owned(),
                    run_epoch,
                    item_id: item_id.clone(),
                    key: key.clone(),
                    result: result.clone(),
                };
                if let Err(error) = self.inner.store.append_event(&event) {
                    receipts.push(refused(Some(&key), error.code, &error.message));
                    continue;
                }
                logged.insert(item_id.clone(), result.clone());
                if owner {
                    apply_result(&mut record.items[index], &result);
                    changed = true;
                }
                receipts.push(receipt("accepted", &key, &item_id));
            }
            if changed {
                self.commit(&mut record)?;
            }
            receipts
        };
        if owner {
            self.maybe_deliver(pool_id, Flush::Auto);
        }
        Ok(receipts)
    }

    // ---- dispatch -----------------------------------------------------------------------------

    /// Dispatches queued items of `pool_id` while workers or slots are available.
    pub fn pump(&self, pool_id: &str) {
        let Some(host) = self.inner.host.clone() else { return };
        loop {
            let mut events = Vec::new();
            let progressed = {
                let _io = lock(&self.inner.io);
                self.dispatch_one(&host, pool_id, &mut events)
            };
            self.emit(events);
            if !progressed {
                break;
            }
        }
        self.maybe_deliver(pool_id, Flush::Auto);
    }

    /// One dispatch step under the lock; returns whether another step may make progress.
    fn dispatch_one(&self, host: &Arc<dyn WorkerHost>, pool_id: &str, events: &mut Vec<PoolEvent>) -> bool {
        if self.is_stopped() {
            return false;
        }
        let Ok(Some(mut record)) = self.inner.store.load(pool_id) else { return false };
        if record.status == PoolStatus::Cancelled || self.is_held(&record.parent_session) {
            return false;
        }
        let Some(index) = record.items.iter().position(|i| i.status == ItemStatus::Queued) else { return false };
        let generation = record.generation;
        if record.mode == PoolMode::KeepAlive
            && let Some(w) = choose_idle(&record.workers)
        {
            let task_id = record.workers[w].task_id.clone();
            let run_epoch = record.workers[w].run_epoch + 1;
            record.workers[w].run_epoch = run_epoch;
            record.workers[w].status = WorkerStatus::Busy;
            record.items[index].status = ItemStatus::Assigned;
            record.items[index].binding = Some(Binding { task_id: task_id.clone(), run_epoch, generation, delivered: false });
            if self.commit(&mut record).is_err() {
                return false;
            }
            let message = assignment_message(&record, &record.items[index]);
            match host.follow_up(&task_id, &message) {
                Ok(()) => {
                    if let Some(binding) = record.items[index].binding.as_mut() {
                        binding.delivered = true;
                    }
                    let item_id = record.items[index].item_id.clone();
                    if self.commit(&mut record).is_err() {
                        return false;
                    }
                    events.push(PoolEvent::Dispatched { pool_id: pool_id.to_owned(), item_id, task_id: task_id.clone(), run_epoch });
                    self.watch(pool_id, &task_id);
                }
                Err(error) => {
                    record.items[index].fail(error.code, error.message);
                    record.workers[w].status = WorkerStatus::Idle;
                    record.workers[w].retired = true;
                    if self.commit(&mut record).is_err() {
                        return false;
                    }
                }
            }
            return true;
        }
        if host.free_slots() == Some(0) {
            return false;
        }
        if !record.tools.is_empty() && !host.tools_available(&record.tools) {
            record.items[index].fail(codes::TOOLS_UNAVAILABLE, "the session that provides this pool's extra tools is gone");
            return self.commit(&mut record).is_ok();
        }
        // Persist the intent first: a crash before the host answers leaves an empty binding,
        // which the next reattach marks `delivery_uncertain`.
        record.items[index].status = ItemStatus::Assigned;
        record.items[index].binding = Some(Binding { task_id: String::new(), run_epoch: 1, generation, delivered: false });
        if self.commit(&mut record).is_err() {
            return false;
        }
        let assignment = assignment_message(&record, &record.items[index]);
        let request = WorkerStart {
            pool_id: record.pool_id.clone(),
            pool_name: record.name.clone(),
            agent: record.agent.clone(),
            tools: record.tools.clone(),
            prompt: format!("{}\n\n{assignment}", record.agent.prompt()),
        };
        match host.start(&request) {
            Ok(started) => {
                record.workers.push(PoolWorker {
                    task_id: started.task_id.clone(),
                    run_epoch: 1,
                    status: WorkerStatus::Busy,
                    completed_turns: 0,
                    idle_since: now_ms(),
                    session_id: started.session_id,
                    retired: false,
                });
                record.items[index].binding = Some(Binding { task_id: started.task_id.clone(), run_epoch: 1, generation, delivered: true });
                let item_id = record.items[index].item_id.clone();
                if self.commit(&mut record).is_err() {
                    return false;
                }
                events.push(PoolEvent::Dispatched { pool_id: pool_id.to_owned(), item_id, task_id: started.task_id.clone(), run_epoch: 1 });
                self.watch(pool_id, &started.task_id);
            }
            Err(error) => {
                record.items[index].binding = None;
                record.items[index].fail(error.code, error.message);
                if self.commit(&mut record).is_err() {
                    return false;
                }
            }
        }
        true
    }

    fn watch(&self, pool_id: &str, task_id: &str) {
        let Some(host) = &self.inner.host else { return };
        if !lock(&self.inner.runtime).watching.insert(task_id.to_owned()) {
            return;
        }
        let wait = host.wait_run(task_id);
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let pool_id = pool_id.to_owned();
        let task_id = task_id.to_owned();
        spawn_detached(Box::pin(async move {
            wait.await;
            let Some(inner) = weak.upgrade() else { return };
            lock(&inner.runtime).watching.remove(&task_id);
            PoolEngine { inner }.run_ended(&pool_id, &task_id);
        }));
    }

    /// Marks the worker's run ended (spec §4, §5): idle, `completed_turns` + 1, unreported items
    /// `item_missing_yield` (or `delivery_uncertain` when delivery was never confirmed); then
    /// dispatch continues.
    pub fn run_ended(&self, pool_id: &str, task_id: &str) {
        {
            let _io = lock(&self.inner.io);
            if let Ok(Some(mut record)) = self.inner.store.load(pool_id) {
                self.fold_logs(&mut record);
                Self::end_run(&mut record, task_id);
                if let Err(error) = self.commit(&mut record) {
                    maho_ext_api::diagnostic(&format!("workpool: {error}"));
                }
            }
        }
        self.emit(vec![PoolEvent::RunEnded { pool_id: pool_id.to_owned(), task_id: task_id.to_owned() }]);
        self.pump(pool_id);
    }

    fn end_run(record: &mut PoolRecord, task_id: &str) {
        let generation = record.generation;
        let mode = record.mode;
        let Some(worker) = record.worker_mut(task_id) else { return };
        let epoch = worker.run_epoch;
        if worker.status != WorkerStatus::Idle {
            worker.status = WorkerStatus::Idle;
            worker.completed_turns += 1;
            worker.idle_since = now_ms();
        }
        if mode == PoolMode::Fresh {
            worker.retired = true;
        }
        for item in &mut record.items {
            let Some(binding) = &item.binding else { continue };
            if item.status != ItemStatus::Assigned || binding.task_id != task_id || binding.run_epoch != epoch || binding.generation != generation {
                continue;
            }
            if binding.delivered {
                item.fail(codes::ITEM_MISSING_YIELD, "the worker's run ended without reporting this key");
            } else {
                item.fail(codes::DELIVERY_UNCERTAIN, "it is unknown whether the assignment reached the worker");
            }
        }
    }

    // ---- aggregate ----------------------------------------------------------------------------

    fn maybe_deliver(&self, pool_id: &str, flush: Flush) {
        let Some(host) = self.inner.host.clone() else { return };
        let (session, generation, results) = {
            let _io = lock(&self.inner.io);
            if self.is_stopped() {
                return;
            }
            let Ok(Some(mut record)) = self.inner.store.load(pool_id) else { return };
            let folded = self.fold_logs(&mut record);
            let ready = record.status == PoolStatus::Closing && record.all_terminal() && !self.is_held(&record.parent_session);
            let in_flight = lock(&self.inner.runtime).in_flight.contains(pool_id);
            let send = ready
                && !in_flight
                && match &record.aggregate {
                    Some(a) if a.generation == record.generation && a.delivered => false,
                    Some(a) if a.generation == record.generation && a.accepted => flush == Flush::Start,
                    Some(a) if a.generation == record.generation => flush != Flush::Auto,
                    _ => true,
                };
            if !send {
                if folded && let Err(error) = self.commit(&mut record) {
                    maho_ext_api::diagnostic(&format!("workpool: {error}"));
                }
                return;
            }
            record.aggregate = Some(AggregateState { generation: record.generation, delivered: false, accepted: true });
            if self.commit(&mut record).is_err() {
                return;
            }
            lock(&self.inner.runtime).in_flight.insert(pool_id.to_owned());
            (record.parent_session.clone(), record.generation, aggregate_results(&record))
        };
        self.emit(vec![PoolEvent::AggregateAccepted { pool_id: pool_id.to_owned(), generation }]);
        let text = serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_owned());
        let details = json!({"pool_id": pool_id, "generation": generation, "results": results});
        let weak = Arc::downgrade(&self.inner);
        let id = pool_id.to_owned();
        let parent = session.clone();
        let done: DeliveryDone = Box::new(move |outcome| {
            if let Some(inner) = weak.upgrade() {
                PoolEngine { inner }.delivery_settled(&id, &parent, generation, outcome);
            }
        });
        host.deliver(&session, text, details, done);
    }

    fn delivery_settled(&self, pool_id: &str, session: &str, generation: u64, outcome: Result<(), String>) {
        lock(&self.inner.runtime).in_flight.remove(pool_id);
        {
            let _io = lock(&self.inner.io);
            if let Ok(Some(mut record)) = self.inner.store.load(pool_id)
                && let Some(aggregate) = record.aggregate.as_mut()
                && aggregate.generation == generation
            {
                match &outcome {
                    Ok(()) => aggregate.delivered = true,
                    Err(_) => aggregate.accepted = false,
                }
                if let Err(error) = self.commit(&mut record) {
                    maho_ext_api::diagnostic(&format!("workpool: {error}"));
                }
            }
        }
        let event = match outcome {
            Ok(()) => PoolEvent::AggregateDelivered { pool_id: pool_id.to_owned(), generation },
            Err(reason) => {
                if let Some(host) = &self.inner.host {
                    host.notice(session, &format!("workpool {pool_id}: the aggregate could not be delivered ({reason}); it is retried on the next close or session start"));
                }
                PoolEvent::AggregateFailed { pool_id: pool_id.to_owned(), generation, reason }
            }
        };
        self.emit(vec![event]);
    }

    // ---- restart, pause, shutdown -------------------------------------------------------------

    /// Session start (spec §8): reattaches every pool owned by `session`.
    pub fn reattach(&self, session: &str) {
        let Some(host) = self.inner.host.clone() else { return };
        lock(&self.inner.runtime).stopped = false;
        let mut pools = Vec::new();
        for (pool_id, loaded) in self.inner.store.load_all() {
            match loaded {
                Ok(record) if record.parent_session == session => pools.push(pool_id),
                Ok(_) => {}
                Err(error) => host.notice(session, &format!("workpool {pool_id}: {error}")),
            }
        }
        for pool_id in pools {
            {
                let _io = lock(&self.inner.io);
                let Ok(Some(mut record)) = self.inner.store.load(&pool_id) else { continue };
                let before = record.clone();
                self.fold_logs(&mut record);
                self.reconcile(&host, &mut record);
                if record != before && self.commit(&mut record).is_err() {
                    continue;
                }
            }
            self.pump(&pool_id);
            self.maybe_deliver(&pool_id, Flush::Start);
        }
    }

    fn reconcile(&self, host: &Arc<dyn WorkerHost>, record: &mut PoolRecord) {
        let held = self.is_held(&record.parent_session);
        for item in &mut record.items {
            if item.status == ItemStatus::Assigned && item.binding.as_ref().is_none_or(|b| b.task_id.is_empty() || !b.delivered) {
                item.fail(codes::DELIVERY_UNCERTAIN, "the host stopped before the assignment was confirmed");
            }
        }
        let tasks: Vec<(String, WorkerStatus)> = record.workers.iter().map(|w| (w.task_id.clone(), w.status)).collect();
        for (task_id, status) in tasks {
            if status == WorkerStatus::Idle {
                continue;
            }
            match host.run_active(&task_id) {
                Some(true) if record.status != PoolStatus::Cancelled => {
                    if let Some(worker) = record.worker_mut(&task_id) {
                        worker.status = if held { WorkerStatus::Paused } else { WorkerStatus::Busy };
                    }
                    self.watch(&record.pool_id.clone(), &task_id);
                }
                Some(true) => host.cancel(&task_id),
                Some(false) => Self::end_run(record, &task_id),
                None => {
                    for item in &mut record.items {
                        if item.status == ItemStatus::Assigned && item.binding.as_ref().is_some_and(|b| b.task_id == task_id) {
                            item.fail(codes::DELIVERY_UNCERTAIN, "the worker task is gone");
                        }
                    }
                    Self::end_run(record, &task_id);
                    if let Some(worker) = record.worker_mut(&task_id) {
                        worker.retired = true;
                    }
                }
            }
        }
    }

    /// The user stopped `session`'s run at stop sequence `seq` (spec §8): its pools are held.
    pub fn hold(&self, session: &str, seq: u64) {
        {
            let mut runtime = lock(&self.inner.runtime);
            let entry = runtime.holds.entry(session.to_owned()).or_insert(seq);
            *entry = (*entry).max(seq);
        }
        self.set_paused(session, true);
    }

    /// Resume for stop sequence `seq`; a resume for an older stop is ignored. Returns whether the
    /// hold was released.
    pub fn release(&self, session: &str, seq: u64) -> bool {
        {
            let mut runtime = lock(&self.inner.runtime);
            match runtime.holds.get(session) {
                Some(held) if seq >= *held => {
                    runtime.holds.remove(session);
                }
                _ => return false,
            }
        }
        let pools = self.set_paused(session, false);
        self.emit(vec![PoolEvent::Released { session: session.to_owned() }]);
        for pool_id in pools {
            self.pump(&pool_id);
        }
        true
    }

    fn set_paused(&self, session: &str, paused: bool) -> Vec<String> {
        let (from, to) = if paused { (WorkerStatus::Busy, WorkerStatus::Paused) } else { (WorkerStatus::Paused, WorkerStatus::Busy) };
        let _io = lock(&self.inner.io);
        let mut pools = Vec::new();
        for (pool_id, loaded) in self.inner.store.load_all() {
            let Ok(mut record) = loaded else { continue };
            if record.parent_session != session {
                continue;
            }
            let mut changed = false;
            for worker in &mut record.workers {
                if worker.status == from {
                    worker.status = to;
                    changed = true;
                }
            }
            if changed && let Err(error) = self.commit(&mut record) {
                maho_ext_api::diagnostic(&format!("workpool: {error}"));
            }
            pools.push(pool_id);
        }
        pools
    }

    /// Session shutdown: stops dispatch and delivery without changing records.
    pub fn shutdown(&self) {
        lock(&self.inner.runtime).stopped = true;
    }
}
