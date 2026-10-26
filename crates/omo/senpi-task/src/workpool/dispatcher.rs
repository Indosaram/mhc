use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use crate::pause::admission_barrier::ResumeCredential;

use super::ports::{WorkpoolAdmission, WorkpoolRequest};
use super::reconcile::{reconcile_worker_turn, uncertain_item_with_context};
use super::store::WorkpoolStore;
use super::types::{
    PoolId, WorkpoolDelivery, WorkpoolDeliveryPhase, WorkpoolError, WorkpoolErrorCode, WorkpoolEvent,
    WorkpoolEventError, WorkpoolEventKind, WorkpoolItem, WorkpoolItemStatus, WorkpoolMode,
    WorkpoolRecord, WorkpoolStatus, WorkpoolWorker, WorkpoolWorkerStatus,
};

type Emit = Arc<dyn Fn(WorkpoolEvent) + Send + Sync + 'static>;
type Authorize = Arc<dyn Fn(&WorkpoolRecord) -> Result<(), super::ports::WorkpoolAdmissionError> + Send + Sync + 'static>;
type Schedule = Arc<dyn Fn(PoolId) + Send + Sync + 'static>;

/// Returns idle workers in the same stable priority order as senpi.
pub fn order_idle_workers(workers: &[WorkpoolWorker]) -> Vec<WorkpoolWorker> {
    let mut result: Vec<_> = workers.iter().filter(|worker| worker.status == WorkpoolWorkerStatus::Idle).cloned().collect();
    result.sort_by(|left, right| {
        left.completed_turns.cmp(&right.completed_turns)
            .then_with(|| left.idle_since.cmp(&right.idle_since))
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    result
}

struct PendingRequest { pool_id: PoolId, worker_id: Option<String>, cancel: Option<Box<dyn FnOnce() + Send>> }

struct DispatcherState {
    holds: BTreeMap<PoolId, u64>,
    disposed: bool,
    credentials: BTreeMap<PoolId, ResumeCredential>,
    scheduled: BTreeSet<PoolId>,
    pumping: BTreeSet<PoolId>,
    pending: BTreeMap<String, PendingRequest>,
    completions: BTreeMap<String, CompletionObserver>,
}

struct CompletionObserver { cancel: watch::Sender<bool> }

pub struct WorkpoolDispatcher {
    store: WorkpoolStore,
    admission: Arc<dyn WorkpoolAdmission + Send + Sync>,
    emit: Emit,
    authorize: Authorize,
    state: Arc<Mutex<DispatcherState>>,
}

impl WorkpoolDispatcher {
    pub fn new(store: WorkpoolStore, admission: Arc<dyn WorkpoolAdmission + Send + Sync>, emit: Emit, authorize: Authorize) -> Self {
        Self { store, admission, emit, authorize, state: Arc::new(Mutex::new(DispatcherState {
            holds: BTreeMap::new(), credentials: BTreeMap::new(),
            disposed: false, scheduled: BTreeSet::new(), pumping: BTreeSet::new(), pending: BTreeMap::new(), completions: BTreeMap::new(),
        })) }
    }

    pub fn pause_for_run(&self, pool_id: &str, pause_seq: u64) {
        { let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
          if state.holds.get(pool_id).is_some_and(|seq| *seq >= pause_seq) { return; }
          state.holds.insert(pool_id.into(), pause_seq);
          state.credentials.remove(pool_id);
        }
        self.cancel(pool_id);
    }
    pub fn resume_for_run(&self, pool_id: &str, pause_seq: u64, credential: crate::pause::admission_barrier::ResumeCredential) -> bool {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.holds.get(pool_id).is_some_and(|seq| *seq != pause_seq) { return false; }
        state.holds.remove(pool_id);
        state.credentials.insert(pool_id.into(), credential);
        true
    }
    pub fn schedule(&self, pool_id: impl Into<PoolId>) {
        enqueue_schedule(self.store.clone(), Arc::clone(&self.admission), Arc::clone(&self.emit),
            Arc::clone(&self.authorize), Arc::clone(&self.state), pool_id.into());
    }

    pub fn attach(&self, pool_id: &PoolId) -> Result<(), WorkpoolError> {
        self.attach_with_credential(pool_id, None)
    }

    pub fn attach_with_credential(&self, pool_id: &PoolId, credential: Option<ResumeCredential>) -> Result<(), WorkpoolError> {
        if let Some(credential) = credential {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).credentials.insert(pool_id.clone(), credential);
        }
        let pool = self.store.load(pool_id)?;
        if pool.hold.is_some() { return Ok(()); }
        let tasks = self.admission.tasks();
        super::reconcile::reconcile_workpool_with_tasks(&self.store, tasks, pool_id, |task_id, epoch| {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.completions.contains_key(&format!("{task_id}:{epoch}")) || state.pending.values().any(|request| request.worker_id.as_deref() == Some(task_id))
        })?;
        let schedule: Schedule = {
            let store = self.store.clone(); let admission = self.admission.clone(); let emit = self.emit.clone();
            let authorize = self.authorize.clone(); let state = self.state.clone();
            Arc::new(move |id| enqueue_schedule(store.clone(), admission.clone(), emit.clone(), authorize.clone(), state.clone(), id))
        };
        for worker in self.store.load(pool_id)?.workers {
            if matches!(worker.status, WorkpoolWorkerStatus::Busy | WorkpoolWorkerStatus::Paused) {
                observe_completion(&pool, (worker.task_id, worker.run_epoch), &self.store, self.admission.clone(), self.emit.clone(), &self.state, schedule.clone());
            }
        }
        self.schedule(pool_id.clone());
        Ok(())
    }

    pub fn attach_all(&self) -> Result<(), WorkpoolError> {
        for pool in self.store.list_result()? {
            self.attach(&pool.pool_id)?;
        }
        Ok(())
    }

    pub fn cancel(&self, pool_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.credentials.remove(pool_id);
        state.scheduled.remove(pool_id);
        let cancelled: Vec<_> = state.pending.iter().filter_map(|(id, request)| (request.pool_id == pool_id).then_some(id.clone())).collect();
        let requests: Vec<_> = cancelled.into_iter().filter_map(|id| state.pending.remove(&id)).collect();
        drop(state);
        for mut request in requests { if let Some(cancel) = request.cancel.take() { cancel(); } }
    }

    pub fn stop_scheduling(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disposed = true;
        state.scheduled.clear();
        state.credentials.clear();
        let requests = std::mem::take(&mut state.pending);
        let completions = std::mem::take(&mut state.completions);
        drop(state);
        for (_, mut request) in requests { if let Some(cancel) = request.cancel.take() { cancel(); } }
        for (_, observer) in completions { let _ = observer.cancel.send(true); }
    }

    pub fn is_disposed(&self) -> bool { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed }

    pub fn take_scheduled(&self, pool_id: &str) -> bool { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).scheduled.remove(pool_id) }
}

fn pump(store: &WorkpoolStore, admission: &Arc<dyn WorkpoolAdmission + Send + Sync>, emit: &Emit, authorize: &Authorize,
    state: &Arc<Mutex<DispatcherState>>, pool_id: &str) -> Result<(), WorkpoolError> {
    if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).holds.contains_key(pool_id) { return Ok(()); }
    let pool = Arc::new(store.load(pool_id)?);
    if pool.hold.is_some() { return Ok(()); }
    if !matches!(pool.status, WorkpoolStatus::Open | WorkpoolStatus::Closing) { return Ok(()); }
    let mut reserved_workers: BTreeSet<String> = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.values()
        .filter_map(|pending| pending.worker_id.clone()).collect();
    for snapshot in &pool.items {
        // Only Queued items and Assigned items with a queued delivery are dispatchable, and no
        // transition leads back to either state, so other snapshots need no fresh reload.
        if snapshot.status != WorkpoolItemStatus::Queued && !(snapshot.status == WorkpoolItemStatus::Assigned
            && snapshot.delivery.as_ref().is_some_and(|delivery| delivery.phase == WorkpoolDeliveryPhase::Queued)) { continue; }
        // Items already awaiting admission need no fresh reload; the post-load check below still
        // covers requests inserted while the load runs.
        if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.contains_key(&snapshot.item_id) { continue; }
        let fresh = store.load(pool_id)?;
        if fresh.hold.is_some() { return Ok(()); }
        let Some(item) = fresh.items.iter().find(|candidate| candidate.item_id == snapshot.item_id).cloned() else { continue };
        if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.contains_key(&item.item_id) { continue; }
        let recovery = if item.status == WorkpoolItemStatus::Assigned && item.delivery.as_ref().is_some_and(|delivery| delivery.phase == WorkpoolDeliveryPhase::Queued) { item.binding.clone() } else { None };
        if item.status != WorkpoolItemStatus::Queued && recovery.is_none() { continue; }
        if recovery.as_ref().is_some_and(|binding| admission.get(&binding.task_id).is_some_and(|task| task.status == crate::state::TaskStatus::Paused)) { continue; }
        let idle = if let Some(binding) = recovery.as_ref().filter(|binding| binding.run_epoch > 0) {
            Some(WorkpoolWorker { task_id: binding.task_id.clone(), run_epoch: admission.task_run_epoch(&binding.task_id).unwrap_or(binding.run_epoch - 1),
                status: WorkpoolWorkerStatus::Idle, completed_turns: 0, idle_since: 0 })
        } else if pool.mode == WorkpoolMode::KeepAlive {
            super::dispatcher::order_idle_workers(&fresh.workers).into_iter().find(|worker| !reserved_workers.contains(&worker.task_id))
        } else { None };
        if pool.mode == WorkpoolMode::KeepAlive && idle.is_none() && recovery.is_none() &&
            (!fresh.workers.is_empty() || state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.values().any(|p| p.pool_id == pool_id)) &&
            !admission.has_free_slot(&pool.worker_spec.plan.model) { break; }
        if let Some(worker) = idle.as_ref() { reserved_workers.insert(worker.task_id.clone()); }
        let bound_task = Arc::new(Mutex::new(recovery.as_ref().map(|binding| binding.task_id.clone())));
        let schedule: Schedule = {
            let store = store.clone();
            let admission = Arc::clone(admission);
            let emit = Arc::clone(emit);
            let authorize = Arc::clone(authorize);
            let state = Arc::clone(state);
            Arc::new(move |next_pool| enqueue_schedule(store.clone(), Arc::clone(&admission), Arc::clone(&emit), Arc::clone(&authorize), Arc::clone(&state), next_pool))
        };
        let request = DispatcherRequest { pool: Arc::clone(&pool), item: item.clone(), worker: idle.clone(), store: store.clone(),
            admission: Arc::clone(admission), emit: Arc::clone(emit), authorize: Arc::clone(authorize), state: Arc::clone(state),
            credential: state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).credentials.get(pool_id).cloned(),
            bound_task: Arc::clone(&bound_task), generation: pool.generation, schedule };
        state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.insert(item.item_id.clone(), PendingRequest {
            pool_id: pool_id.to_owned(), worker_id: idle.map(|worker| worker.task_id), cancel: None,
        });
        let handle = admission.request(Box::new(request));
        let crate::workpool::ports::WorkpoolRequestHandle { held, cancel } = handle;
        if held.is_some() {
            // The run is paused: the turn stays held until Resume, so nothing waits and nothing is
            // acknowledged (`workpool/dispatcher.ts` `if (request.kind === "held") return`).
            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.remove(&item.item_id);
            return Ok(());
        }
        let mut state_guard = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = state_guard.pending.get_mut(&item.item_id) {
            pending.cancel = Some(cancel);
        } else {
            drop(state_guard);
            cancel();
            continue;
        }
        drop(state_guard);
        emit(WorkpoolEvent { pool_id: pool_id.to_owned(), kind: WorkpoolEventKind::Waiting, item_id: Some(item.item_id), task_id: None, run_epoch: None, error: None });
        admission.drain();
    }
    Ok(())
}

fn spawn_pump(store: WorkpoolStore, admission: Arc<dyn WorkpoolAdmission + Send + Sync>, emit: Emit, authorize: Authorize,
    state: Arc<Mutex<DispatcherState>>, pool_id: PoolId) {
    tokio::spawn(async move {
        {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.scheduled.remove(&pool_id);
            if state.disposed || !state.pumping.insert(pool_id.clone()) { return; }
        }
        if let Err(error) = pump(&store, &admission, &emit, &authorize, &state, &pool_id) {
            emit(WorkpoolEvent { pool_id: pool_id.clone(), kind: WorkpoolEventKind::AdmissionFailed, item_id: None, task_id: None, run_epoch: None,
                error: Some(WorkpoolEventError { code: error.code, message: error.message }) });
        }
        let repeat = {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pumping.remove(&pool_id);
            !state.disposed && state.scheduled.contains(&pool_id)
        };
        if repeat { spawn_pump(store, admission, emit, authorize, state, pool_id); }
    });
}

fn enqueue_schedule(store: WorkpoolStore, admission: Arc<dyn WorkpoolAdmission + Send + Sync>, emit: Emit,
    authorize: Authorize, state: Arc<Mutex<DispatcherState>>, pool_id: PoolId) {
    let should_wake = {
        let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed { false }
        else { state.scheduled.insert(pool_id.clone()) && !state.pumping.contains(&pool_id) }
    };
    if should_wake { spawn_pump(store, admission, emit, authorize, state, pool_id); }
}

struct DispatcherRequest {
    pool: Arc<WorkpoolRecord>, item: WorkpoolItem, worker: Option<WorkpoolWorker>, store: WorkpoolStore,
    admission: Arc<dyn WorkpoolAdmission + Send + Sync>, emit: Emit, authorize: Authorize, state: Arc<Mutex<DispatcherState>>,
    credential: Option<ResumeCredential>,
    bound_task: Arc<Mutex<Option<String>>>, generation: u32, schedule: Schedule,
}
impl WorkpoolRequest for DispatcherRequest {
    fn resume_credential(&self) -> Option<ResumeCredential> { self.credential.clone() }
    fn pool(&self) -> &WorkpoolRecord { &self.pool }
    fn item(&self) -> &WorkpoolItem { &self.item }
    fn worker(&self) -> Option<&WorkpoolWorker> { self.worker.as_ref() }
    fn current(&self) -> bool {
        let Ok(latest) = self.store.load(&self.pool.pool_id) else { return false };
        let bound = self.bound_task.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        !self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed && latest.hold.is_none() && latest.generation == self.generation &&
            matches!(latest.status, WorkpoolStatus::Open | WorkpoolStatus::Closing) && latest.items.iter().any(|candidate|
                candidate.item_id == self.item.item_id && (candidate.status == WorkpoolItemStatus::Queued ||
                    (candidate.binding.as_ref().is_some_and(|binding| Some(&binding.task_id) == bound.as_ref()) &&
                    (candidate.status == WorkpoolItemStatus::Assigned || candidate.yield_sha256.is_some()))))
    }
    fn authorize(&self) -> Result<(), super::ports::WorkpoolAdmissionError> {
        let pool = self.store.load(&self.pool.pool_id).map_err(|error| super::ports::WorkpoolAdmissionError(error.to_string()))?;
        (self.authorize)(&pool)
    }
    fn bind(&mut self, task_id: &str, epoch: u32) -> bool {
        let mut bound = false;
        let result = self.store.mutate_result(&self.pool.pool_id, |latest| {
            if self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed || latest.hold.is_some() || latest.generation != self.generation ||
                !matches!(latest.status, WorkpoolStatus::Open | WorkpoolStatus::Closing) { return Ok(latest.clone()); }
            let Some(anchor) = latest.items.iter().find(|candidate| candidate.item_id == self.item.item_id) else { return Ok(latest.clone()) };
            if anchor.status != WorkpoolItemStatus::Queued && !(anchor.status == WorkpoolItemStatus::Assigned &&
                anchor.delivery.as_ref().is_some_and(|d| d.phase == WorkpoolDeliveryPhase::Queued) &&
                anchor.binding.as_ref().is_some_and(|b| b.task_id == task_id && b.run_epoch == epoch)) { return Ok(latest.clone()); }
            bound = true;
            *self.bound_task.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task_id.to_owned());
            let prior = latest.workers.iter().find(|worker| worker.task_id == task_id);
            let worker = WorkpoolWorker { task_id: task_id.to_owned(), run_epoch: epoch, status: WorkpoolWorkerStatus::Busy,
                completed_turns: prior.map_or(0, |worker| worker.completed_turns), idle_since: prior.map_or(0, |worker| worker.idle_since) };
            let mut next = latest.clone();
            for item in &mut next.items {
                if item.item_id == self.item.item_id || (self.worker.is_some() && item.status == WorkpoolItemStatus::Queued) {
                    item.status = WorkpoolItemStatus::Assigned;
                    item.binding = Some(super::types::WorkpoolBinding { task_id: task_id.to_owned(), run_epoch: epoch, generation: self.generation });
                    item.delivery = Some(WorkpoolDelivery { phase: WorkpoolDeliveryPhase::Queued, message_sha256: None, entry_ids: None });
                }
            }
            next.workers.retain(|candidate| candidate.task_id != task_id);
            next.workers.push(worker);
            Ok(next)
        });
        if result.is_err() { return false; }
        if bound {
        let latest = match self.store.load(&self.pool.pool_id) { Ok(latest) => latest, Err(error) => {
            self.event(WorkpoolEvent { pool_id: self.pool.pool_id.clone(), kind: WorkpoolEventKind::AdmissionFailed, item_id: Some(self.item.item_id.clone()), task_id: Some(task_id.to_owned()), run_epoch: Some(epoch),
                error: Some(WorkpoolEventError { code: error.code, message: error.message }) });
            return false;
        }};
            let ids: Vec<_> = latest.items.iter().filter(|candidate| candidate.item_id != self.item.item_id && candidate.binding.as_ref().is_some_and(|binding| binding.task_id == task_id))
                .map(|candidate| candidate.item_id.clone()).collect();
            let requests: Vec<_> = {
                let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                ids.into_iter().filter_map(|id| state.pending.remove(&id)).collect()
            };
            for mut pending in requests {
                if let Some(cancel) = pending.cancel.take() { cancel(); }
                }
        }
        bound
    }
    fn event(&self, event: WorkpoolEvent) {
        if event.kind == WorkpoolEventKind::AdmissionFailed {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.remove(&self.item.item_id);
            if self.store.load(&self.pool.pool_id).is_ok_and(|pool| pool.hold.is_some()) { return; }
            let worker_is_running = event.task_id.as_deref().and_then(|task_id| self.admission.get(task_id))
                .is_some_and(|task| task.status == crate::state::TaskStatus::Running);
            let mutation = self.store.mutate_result(&self.pool.pool_id, |latest| {
                if latest.hold.is_some() || latest.generation != self.generation { return Ok(latest.clone()); }
                let mut next = latest.clone();
                for candidate in &mut next.items {
                    let affected = candidate.item_id == self.item.item_id || self.bound_task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
                        .is_some_and(|task| candidate.binding.as_ref().is_some_and(|binding| &binding.task_id == task && Some(binding.run_epoch) == event.run_epoch));
                    if !affected || !matches!(candidate.status, WorkpoolItemStatus::Queued | WorkpoolItemStatus::Assigned) { continue; }
                    if event.error.as_ref().is_some_and(|error| error.code == WorkpoolErrorCode::DeliveryUncertain) {
                        *candidate = uncertain_item_with_context(candidate.clone(), &self.pool, event.task_id.as_deref().and_then(|task_id| self.admission.get(task_id)).as_ref());
                    } else { candidate.status = WorkpoolItemStatus::Error; candidate.error = event.error.as_ref().map(|error| super::types::WorkpoolYieldError { code: error.code.as_str().into(), message: error.message.clone() }); }
                }
                next.workers.retain(|worker| Some(&worker.task_id) != event.task_id.as_ref() || worker_is_running);
                Ok(next)
            });
            if let Err(error) = mutation {
                (self.emit)(WorkpoolEvent { pool_id: self.pool.pool_id.clone(), kind: WorkpoolEventKind::AdmissionFailed,
                    item_id: Some(self.item.item_id.clone()), task_id: event.task_id.clone(), run_epoch: event.run_epoch,
                    error: Some(WorkpoolEventError { code: error.code, message: error.message }) });
            }
            if event.error.as_ref().is_some_and(|error| error.code == WorkpoolErrorCode::DeliveryUncertain)
                && let (Some(task_id), Some(epoch)) = (event.task_id.clone(), event.run_epoch) {
                    observe_completion(&self.pool, (task_id, epoch), &self.store, Arc::clone(&self.admission), Arc::clone(&self.emit), &self.state, Arc::clone(&self.schedule));
                }
            (self.schedule)(self.pool.pool_id.clone());
            (self.emit)(event);
        } else if event.kind == WorkpoolEventKind::Dispatched {
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.remove(&self.item.item_id);
            if let (Some(task_id), Some(epoch)) = (event.task_id.clone(), event.run_epoch) { observe_completion(&self.pool, (task_id, epoch), &self.store, Arc::clone(&self.admission), Arc::clone(&self.emit), &self.state, Arc::clone(&self.schedule)); }
            (self.schedule)(self.pool.pool_id.clone());
            (self.emit)(event);
        } else { (self.emit)(event); }
    }
}

fn observe_completion(pool: &WorkpoolRecord, (task_id, epoch): (String, u32), store: &WorkpoolStore, admission: Arc<dyn WorkpoolAdmission + Send + Sync>, emit: Emit, state: &Arc<Mutex<DispatcherState>>, schedule: Schedule) {
    let identity = format!("{task_id}:{epoch}");
    {
        let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed || state.pending.values().any(|request| request.pool_id == pool.pool_id && request.worker_id.as_deref() == Some(task_id.as_str())) { return; }
    }
    let (cancel, mut cancel_rx) = watch::channel(false);
    {
        let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.completions.contains_key(&identity) { return; }
        state.completions.insert(identity.clone(), CompletionObserver { cancel });
    }
    // Only the id and generation outlive this call; cloning the whole record would copy every item.
    let pool_id = pool.pool_id.clone(); let generation = pool.generation; let store = store.clone(); let state = Arc::clone(state);
    tokio::spawn(async move {
        let result = admission.wait_for(&task_id, &mut cancel_rx).await;
        state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).completions.remove(&identity);
        let record = match result { Ok(record) => record, Err(error) => {
            if !*cancel_rx.borrow() { emit(WorkpoolEvent { pool_id: pool_id.clone(), kind: WorkpoolEventKind::AdmissionFailed, item_id: None,
                task_id: Some(task_id), run_epoch: Some(epoch), error: Some(WorkpoolEventError { code: WorkpoolErrorCode::StoreCorrupt, message: error.0 }) }); }
            return;
        }};
        if *cancel_rx.borrow() || record.notification.run_epoch != i64::from(epoch) { return; }
        if record.status == crate::state::TaskStatus::Paused || store.load(&pool_id).is_ok_and(|pool| pool.hold.is_some()) { return; }
        let turn = (pool_id.clone(), generation, task_id.clone(), epoch);
        if let Err(error) = reconcile_worker_turn(&store, admission.tasks(), &turn) {
            emit(WorkpoolEvent { pool_id: pool_id.clone(), kind: WorkpoolEventKind::AdmissionFailed, item_id: None, task_id: Some(task_id), run_epoch: Some(epoch),
                error: Some(WorkpoolEventError { code: error.code, message: error.message }) }); return;
        }
        if let Err(error) = store.mutate_result(&pool_id, |latest| {
            if latest.generation != generation { return Ok(latest.clone()); }
            let mut next=latest.clone();
            for worker in &mut next.workers { if worker.task_id==task_id&&worker.run_epoch==epoch&&worker.status==WorkpoolWorkerStatus::Busy {
                worker.status=WorkpoolWorkerStatus::Idle;worker.completed_turns+=1;
                worker.idle_since=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
            }}
            Ok(next)
        }) {
            emit(WorkpoolEvent { pool_id: pool_id.clone(), kind: WorkpoolEventKind::AdmissionFailed, item_id: None, task_id: Some(task_id), run_epoch: Some(epoch),
                error: Some(WorkpoolEventError { code: error.code, message: error.message }) });
            return;
        }
        emit(WorkpoolEvent { pool_id: pool_id.clone(), kind: WorkpoolEventKind::WorkerIdle, item_id: None, task_id: Some(task_id), run_epoch: Some(epoch), error: None });
        schedule(pool_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use crate::state::{TaskNotification, TaskRecord, TaskRecordInput, TaskStatus, create_task_record};
    use crate::store::{StateDirConfig, TaskRecordStore};
    use super::super::ports::WorkpoolAdmissionError;
    use super::super::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolCreate, WorkpoolSpec as PoolSpec};

    fn worker(id: &str, turns: u32, idle_since: u64, status: WorkpoolWorkerStatus) -> WorkpoolWorker {
        WorkpoolWorker { task_id: id.into(), run_epoch: 0, status, completed_turns: turns, idle_since }
    }

    #[test]
    fn idle_workers_follow_turn_idle_time_then_id() {
        let ordered = order_idle_workers(&[
            worker("z", 1, 2, WorkpoolWorkerStatus::Idle),
            worker("a", 0, 9, WorkpoolWorkerStatus::Idle),
            worker("b", 0, 9, WorkpoolWorkerStatus::Idle),
            worker("busy", 0, 0, WorkpoolWorkerStatus::Busy),
        ]);
        assert_eq!(ordered.iter().map(|worker| worker.task_id.as_str()).collect::<Vec<_>>(), ["a", "b", "z"]);
    }

    fn dispatcher() -> WorkpoolDispatcher {
        let admission = Arc::new(FakeAdmission::new());
        WorkpoolDispatcher::new(WorkpoolStore::new("/tmp"), admission, Arc::new(|_| {}), Arc::new(|_| Ok(())))
    }

    #[tokio::test]
    async fn scheduling_is_coalesced_and_stops() {
        let dispatcher = dispatcher();
        enqueue_schedule(dispatcher.store.clone(), Arc::clone(&dispatcher.admission), Arc::clone(&dispatcher.emit),
            Arc::clone(&dispatcher.authorize), Arc::clone(&dispatcher.state), "wp_a".into());
        assert!(dispatcher.take_scheduled("wp_a"));
        assert!(!dispatcher.take_scheduled("wp_a"));
        dispatcher.stop_scheduling();
        dispatcher.schedule("wp_b");
        assert!(!dispatcher.take_scheduled("wp_b"));
    }

    struct FakeAdmission { tasks: TaskRecordStore, task: TaskRecord }
    impl FakeAdmission {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("state dir");
            let tasks = TaskRecordStore::new(&StateDirConfig { project_dir: root.keep(), task_state_dir: None });
            let mut task = create_task_record(TaskRecordInput { parent_session_id: "parent".into(), root_session_id: "root".into(), ..Default::default() }, Some(10)).expect("task record");
            task.status = TaskStatus::Completed;
            task.notification = TaskNotification { run_epoch: 1, notified_epoch: 1, notification_failed_epoch: None, liveness_notified_epoch: None };
            tasks.save(&task).expect("persist task");
            Self { tasks, task }
        }
    }
    impl WorkpoolAdmission for FakeAdmission {
        fn tasks(&self) -> &TaskRecordStore { &self.tasks }
        fn bind_pool_kernel_tools(&self, _: &str, _: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError> { Ok(()) }
        fn unbind_pool_kernel_tools(&self, _: &str) {}
        fn resolve(&self, _: &WorkpoolCaller, _: &WorkpoolAgent) -> Result<PoolSpec, WorkpoolAdmissionError> { Ok(PoolSpec { start: Default::default(), plan: Default::default() }) }
        fn authorize_spawn(&self, _: &WorkpoolAgent, _: &str) -> Result<(), WorkpoolAdmissionError> { Ok(()) }
        fn has_free_slot(&self, _: &str) -> bool { true }
        fn request(&self, mut request: Box<dyn WorkpoolRequest>) -> crate::workpool::ports::WorkpoolRequestHandle {
            assert!(request.current());
            assert!(request.authorize().is_ok());
            assert!(request.bind(&self.task.task_id, 1));
            request.event(WorkpoolEvent { pool_id: request.pool().pool_id.clone(), kind: WorkpoolEventKind::Dispatched,
                item_id: Some(request.item().item_id.clone()), task_id: Some(self.task.task_id.clone()), run_epoch: Some(1), error: None });
            crate::workpool::ports::WorkpoolRequestHandle::dispatched(Box::new(|| {}))
        }
        fn drain(&self) {}
        fn get(&self, _: &str) -> Option<TaskRecord> { Some(self.task.clone()) }
        fn task_run_epoch(&self, _: &str) -> Option<u32> { Some(1) }
        fn pending(&self, _: &str) -> bool { false }
        fn cancel<'a>(&'a self, _: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolAdmissionError>> + Send + 'a>> { Box::pin(async { Ok(()) }) }
        fn wait_for<'a>(&'a self, _: &'a str, cancel: &'a mut watch::Receiver<bool>) -> Pin<Box<dyn Future<Output = Result<TaskRecord, WorkpoolAdmissionError>> + Send + 'a>> {
            let task = self.task.clone();
            Box::pin(async move { if *cancel.borrow() { Err(WorkpoolAdmissionError("cancelled".into())) } else { Ok(task) } })
        }
    }

    #[tokio::test]
    async fn admitted_item_binds_and_completion_returns_worker_to_idle() {
        let root = tempfile::tempdir().expect("pool state dir");
        let admission = Arc::new(FakeAdmission::new());
        let store = WorkpoolStore::new(root.path());
        let caller = WorkpoolCaller { session_id: "parent".into(), root_session_id: "root".into(), depth: 0, cwd: "/tmp".into() };
        let create = WorkpoolCreate { name: "dispatcher-test".into(), agent: WorkpoolAgent::Category { prompt: "run".into(), model: None, category: "quick".into() }, mode: Some(WorkpoolMode::Fresh), tools: None };
        let pool = store.create(&caller, &create, WorkpoolMode::Fresh, super::super::types::WorkpoolSpec { start: Default::default(), plan: Default::default() }).expect("pool");
        store.mutate(&pool.pool_id, |record| { let mut next=record.clone(); next.items.push(WorkpoolItem { key: "key".into(), input: serde_json::Value::Null, item_id: "item".into(), status: WorkpoolItemStatus::Queued, binding: None, delivery: None, data: None, error: None, yield_sha256: None }); next }).expect("queued item");
        let (idle_tx, idle_rx) = tokio::sync::oneshot::channel();
        let idle_tx = Arc::new(Mutex::new(Some(idle_tx)));
        let notify = Arc::clone(&idle_tx);
        let expected_task_id = admission.task.task_id.clone();
        let admission: Arc<dyn WorkpoolAdmission + Send + Sync> = admission;
        let dispatcher = WorkpoolDispatcher::new(store.clone(), admission.clone(), Arc::new(move |event| {
            if event.kind == WorkpoolEventKind::WorkerIdle && let Some(tx) = notify.lock().expect("signal").take() { let _ = tx.send(()); }
        }), Arc::new(|_| Ok(())));
        pump(&store, &admission, &dispatcher.emit, &dispatcher.authorize, &dispatcher.state, &pool.pool_id).expect("pump admission");
        tokio::time::timeout(std::time::Duration::from_secs(5), idle_rx).await.expect("worker idle deadline").expect("worker idle event");
        let saved = store.load(&pool.pool_id).expect("saved pool");
        assert_eq!(saved.items[0].binding.as_ref().map(|binding| binding.task_id.as_str()), Some(expected_task_id.as_str()));
        assert_eq!(saved.workers[0].status, WorkpoolWorkerStatus::Idle);
        dispatcher.stop_scheduling();
    }

    #[test]
    fn cancelling_pool_keeps_completion_observer_until_dispatcher_stops() {
        let dispatcher = dispatcher();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let identity = "task:3".to_owned();
        dispatcher.state.lock().expect("dispatcher state").completions.insert(identity.clone(), CompletionObserver {
            cancel: cancel_tx,
        });
        dispatcher.cancel("wp_cancel");
        assert!(!*cancel_rx.borrow());
        assert!(dispatcher.state.lock().expect("dispatcher state").completions.contains_key(&identity));
        dispatcher.stop_scheduling();
        assert!(*cancel_rx.borrow());
    }
}
