use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, atomic::{AtomicBool, Ordering}};

use tokio::sync::watch;

use crate::manager::manager::{AbortSignal, TaskManager};
use crate::manager::types::ManagedStartSpec;
use crate::state::TaskRecord;
use crate::steering::{CancelOptions, SendOutcome};
use crate::store::TaskRecordStore;
use crate::workpool::ports::{WorkpoolAdmission, WorkpoolAdmissionError, WorkpoolRequest};
use crate::workpool::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolErrorCode, WorkpoolSpec};

pub struct ManagerWorkpoolAdmission {
    manager: Arc<TaskManager>,
    worker_tools: Arc<Mutex<BTreeMap<String, Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>>>>,
}

impl ManagerWorkpoolAdmission {
    pub fn new(manager: Arc<TaskManager>) -> Self { Self { manager, worker_tools: Arc::new(Mutex::new(BTreeMap::new())) } }
    pub fn bind_worker_tools(&self, pool_id: &str, tools: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError> {
        let pool = crate::workpool::store::WorkpoolStore::new(self.manager.state_dir()).load(pool_id).map_err(|error| WorkpoolAdmissionError(error.to_string()))?;
        let mut expected = pool.kernel_tool_names.unwrap_or_default();
        let mut actual = tools.iter().map(|tool| tool.name().to_owned()).collect::<Vec<_>>();
        expected.sort(); expected.dedup(); actual.sort(); actual.dedup();
        if expected != actual { return Err(WorkpoolAdmissionError("Live worker-tool grant does not match persisted pool tool names.".into())); }
        self.worker_tools.lock().unwrap_or_else(PoisonError::into_inner).insert(pool_id.to_owned(), tools);
        Ok(())
    }
    pub fn release_worker_tools(&self, pool_id: &str) {
        self.worker_tools.lock().unwrap_or_else(PoisonError::into_inner).remove(pool_id);
    }
}

struct AdmissionLeaseGuard(Arc<dyn crate::lifecycle::admission_lease::SessionAdmissionLease>);

impl Drop for AdmissionLeaseGuard {
    fn drop(&mut self) { self.0.release(); }
}

impl WorkpoolAdmission for ManagerWorkpoolAdmission {
    fn tasks(&self) -> &TaskRecordStore { self.manager.store() }
    fn bind_pool_kernel_tools(&self, pool_id: &str, tools: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError> { self.bind_worker_tools(pool_id, tools) }
    fn unbind_pool_kernel_tools(&self, pool_id: &str) { self.release_worker_tools(pool_id); }
    fn resolve(&self, caller: &WorkpoolCaller, agent: &WorkpoolAgent) -> Result<WorkpoolSpec, WorkpoolAdmissionError> { self.manager.resolve_workpool_spec(caller, agent) }
    fn authorize_spawn(&self, _agent: &WorkpoolAgent, session: &str) -> Result<(), WorkpoolAdmissionError> { self.manager.admission_authorize(session) }
    fn has_free_slot(&self, model: &str) -> bool { self.manager.has_free_slot(model) }

    fn request(&self, mut request: Box<dyn WorkpoolRequest>) -> Box<dyn FnOnce() + Send> {
        let task_id = match request.worker().map(|worker| worker.task_id.clone()).or_else(|| request.item().binding.as_ref().map(|binding| binding.task_id.clone())) {
            Some(task_id) => task_id,
            None => match crate::state::create_task_id(None) {
                Ok(task_id) => task_id.to_string(),
                Err(error) => {
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, "", 0, Some((WorkpoolErrorCode::SpawnFailed, error.to_string()))));
                    return Box::new(|| {});
                }
            },
        };
        let epoch = match request.worker() {
            Some(worker) => match self.manager.workpool_worker_record(&task_id) {
                Some(record)
                    if i64::from(worker.run_epoch).checked_add(1)
                        == record.notification.run_epoch.checked_add(1)
                        && request.item().binding.as_ref().is_some_and(|binding| {
                            binding.task_id == task_id
                                && i64::from(binding.run_epoch)
                                    == record.notification.run_epoch.saturating_add(1)
                        }) =>
                {
                    record.notification.run_epoch.saturating_add(1)
                }
                Some(_) => {
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, i64::from(worker.run_epoch), Some((WorkpoolErrorCode::WorkerNotContinuable, "Worker epoch is no longer continuable.".into()))));
                    return Box::new(|| {});
                }
                None => {
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, 0, Some((WorkpoolErrorCode::WorkerNotContinuable, "Worker task record is unavailable.".into()))));
                    return Box::new(|| {});
                }
            },
            None => 0,
        };
        let model = request.pool().worker_spec.plan.model.clone();
        let manager = self.manager.clone();
        let worker_tools = Arc::clone(&self.worker_tools);
        let cancelled = Arc::new(AtomicBool::new(false));
        let grant_cancelled = cancelled.clone();
        let grant_task_id = task_id.clone();
        let grant_model = model.clone();
        let grant: crate::manager::concurrency::Grant = Box::new(move || {
            let task_id = grant_task_id;
            let model = grant_model;
            if task_id.is_empty() || grant_cancelled.load(Ordering::Acquire) || !request.current() {
                if !task_id.is_empty() { manager.release_reserved_slot(&task_id, &model, epoch); }
                return;
            }
            request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::Granted, &task_id, epoch, None));
            if let Err(error) = request.authorize() {
                manager.release_reserved_slot(&task_id, &model, epoch);
                request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::PolicyDenied, error.to_string()))));
                return;
            }
            if request.worker().is_some() && !manager.workpool_worker_eligible(&task_id, &request.pool().parent_session_id, epoch) {
                    manager.release_reserved_slot(&task_id, &model, epoch);
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::WorkerNotContinuable, "Worker changed before revival.".into()))));
                    return;
            }
            if grant_cancelled.load(Ordering::Acquire) || !request.current() {
                manager.release_reserved_slot(&task_id, &model, epoch);
                return;
            }
            if !request.bind(&task_id, u32::try_from(epoch).unwrap_or_default()) { manager.release_reserved_slot(&task_id, &model, epoch); return; }
            let pool = request.pool().clone();
            let mut spec = pool.worker_spec.clone();
            spec.start.prompt = format!("{}\n\n{}", request.item().input, crate::workpool::worker_turn::WORKPOOL_TURN_MESSAGE);
            if pool.kernel_tool_names.as_ref().is_some_and(|names| !names.is_empty()) {
                let grants = worker_tools.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(tools) = grants.get(&pool.pool_id) else {
                    manager.release_reserved_slot(&task_id, &model, epoch);
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::KernelToolStale, "The pool worker-tool grant is not present in this live runtime.".into()))));
                    return;
                };
                let mut expected = pool.kernel_tool_names.clone().unwrap_or_default();
                let mut actual = tools.iter().map(|tool| tool.name().to_owned()).collect::<Vec<_>>();
                expected.sort(); expected.dedup(); actual.sort(); actual.dedup();
                if expected != actual {
                    manager.release_reserved_slot(&task_id, &model, epoch);
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::KernelToolStale, "The pool worker-tool grant no longer matches persisted names.".into()))));
                    return;
                }
                spec.start.member_scoped_tools = Some(tools.clone());
            }
            let admission_lease = if request.worker().is_none() {
                match crate::lifecycle::admission_lease::acquire_session_admission_lease(manager.state_dir(), &pool.parent_session_id, Default::default()) {
                    Ok(crate::lifecycle::admission_lease::AcquireAdmissionLeaseResult::Acquired(lease)) => Some(lease),
                    Ok(crate::lifecycle::admission_lease::AcquireAdmissionLeaseResult::Contended) => { manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::AdmissionRefused, "Residency admission is contended.".into())))); return; },
                    Err(error) => { manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::StoreCorrupt, error.to_string())))); return; },
                }
            } else { None };
            let _lease_guard = admission_lease.as_ref().map(|lease| AdmissionLeaseGuard(Arc::clone(lease)));
            let prepared = if request.worker().is_some() {
                manager.get(&task_id).map(|record| {
                    let managed = crate::manager::helpers::build_managed_spec(&record, &spec.start, &spec.plan, spec.start.cwd.as_deref().unwrap_or_default(), manager.state_dir());
                    (record, managed)
                }).ok_or_else(|| "Worker task record disappeared before revival.".to_owned())
            } else {
                manager.allocate_workpool_record(&spec, &task_id, i64::from(std::process::id())).map_err(|error| error.to_string())
            };
            let (record, managed) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => { if let Some(lease) = &admission_lease { lease.release(); } manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::SpawnFailed, error)))); return; }
            };
            let managed = ManagedStartSpec { workpool_state_dir: Some(manager.state_dir().to_string_lossy().into_owned()), workpool_run_epoch: Some(u32::try_from(epoch).unwrap_or_default()), ..managed };
            if request.worker().is_none() && manager.get(&task_id).is_some() {
                if let Some(lease) = &admission_lease { lease.release(); }
                manager.release_reserved_slot(&task_id, &model, epoch);
                request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::DeliveryUncertain, "An existing worker launch cannot be replayed.".into()))));
                return;
            }
            if let Some(lease) = &admission_lease && (!lease.is_owner() || manager.admission_authorize(&pool.parent_session_id).is_err()) { manager.release_reserved_slot(&task_id, &model, epoch); return; }
            if request.worker().is_none() && manager.store().save(&record).is_err() { manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::StoreCorrupt, "Could not persist prelaunch task record.".into())))); return; }
            if let Some(lease) = &admission_lease { lease.release(); }
            let store = crate::workpool::store::WorkpoolStore::new(manager.store().state_dir());
            let turn = (pool.pool_id.clone(), pool.generation, task_id.clone(), u32::try_from(epoch).unwrap_or_default());
            if let Err(error) = crate::workpool::worker_turn::append_assigned_messages(&store, manager.store(), &turn) {
                if request.worker().is_none() { let _ = manager.cancel_task(&task_id, None, CancelOptions::default()); }
                manager.release_reserved_slot(&task_id, &model, epoch);
                request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((error.code, error.message))));
                return;
            }
            let turn_prompt = match crate::workpool::worker_turn::begin_worker_turn(&store, manager.store(), &turn, Some(&managed.prompt)) {
                Ok(Some(prompt)) => prompt,
                Ok(None) => { if request.worker().is_none() { let _ = manager.cancel_task(&task_id, None, CancelOptions::default()); } manager.release_reserved_slot(&task_id, &model, epoch); return; }
                Err(error) => { if request.worker().is_none() { let _ = manager.cancel_task(&task_id, None, CancelOptions::default()); } manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((error.code, error.message)))); return; }
            };
            if grant_cancelled.load(Ordering::Acquire) || !request.current() {
                if request.worker().is_none() { let _ = manager.cancel_task(&task_id, None, CancelOptions::default()); }
                manager.release_reserved_slot(&task_id, &model, epoch);
                return;
            }
            if request.worker().is_some() {
                if manager.get_resident_handle(&task_id).is_none() { manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::WorkerNotContinuable, "Worker has no resident session for revival.".into())))); return; }
                if let Err(error) = crate::workpool::worker_turn::acknowledge_worker_turn(&store, manager.store(), &turn, &turn_prompt.entries) {
                    manager.release_reserved_slot(&task_id, &model, epoch);
                    request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::DeliveryUncertain, error.to_string()))));
                    return;
                }
                match manager.revive_workpool_task(&task_id, epoch, &turn_prompt.message) {
                    Ok(SendOutcome::Revived { .. }) => {},
                    Ok(SendOutcome::NotContinuable { reason, .. }) => {
                        let uncertain = manager.get(&task_id).is_some_and(|record| record.notification.run_epoch == epoch);
                        manager.release_reserved_slot(&task_id, &model, epoch);
                        let code = if uncertain { WorkpoolErrorCode::DeliveryUncertain } else { WorkpoolErrorCode::WorkerNotContinuable };
                        request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((code, reason))));
                        return;
                    }
                    Ok(_) | Err(_) => { manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::WorkerNotContinuable, "Worker could not be revived.".into())))); return; }
                }
            } else if grant_cancelled.load(Ordering::Acquire) || !request.current() {
                let _ = manager.cancel_task(&task_id, None, CancelOptions::default()); manager.release_reserved_slot(&task_id, &model, epoch); return;
            } else if let Err(error) = manager.launch_prepared(record, ManagedStartSpec { prompt: turn_prompt.message.clone(), workpool_state_dir: Some(manager.state_dir().to_string_lossy().into_owned()), workpool_run_epoch: Some(u32::try_from(epoch).unwrap_or_default()), ..managed }) {
                if let Some(lease) = &admission_lease { lease.release(); } manager.release_reserved_slot(&task_id, &model, epoch); request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::SpawnFailed, error)))); return;
            }
            if request.worker().is_none() && let Err(error) = crate::workpool::worker_turn::acknowledge_worker_turn(&store, manager.store(), &turn, &turn_prompt.entries) {
                request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::AdmissionFailed, &task_id, epoch, Some((WorkpoolErrorCode::DeliveryUncertain, error.to_string()))));
                return;
            }
            request.event(event(request.as_ref(), crate::workpool::types::WorkpoolEventKind::Dispatched, &task_id, epoch, None));
        });
        self.manager.enqueue_workpool(&model, &task_id, epoch, grant);
        let manager = self.manager.clone();
        Box::new(move || {
            cancelled.store(true, Ordering::Release);
            manager.remove_workpool_waiter(&model, &task_id);
        })
    }

    fn drain(&self) {}
    fn get(&self, task_id: &str) -> Option<TaskRecord> { self.manager.get(task_id) }
    fn task_run_epoch(&self, task_id: &str) -> Option<u32> { self.manager.run_epoch(task_id).and_then(|epoch| u32::try_from(epoch).ok()) }
    fn pending(&self, task_id: &str) -> bool { self.manager.is_launching(task_id) }
    fn cancel<'a>(&'a self, task_id: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WorkpoolAdmissionError>> + Send + 'a>> {
        Box::pin(async move { self.manager.cancel_task(task_id, None, CancelOptions::default()).map(|_| ()).map_err(|error| WorkpoolAdmissionError(error.to_string())) })
    }
    fn wait_for<'a>(&'a self, task_id: &'a str, cancel: &'a mut watch::Receiver<bool>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TaskRecord, WorkpoolAdmissionError>> + Send + 'a>> {
        Box::pin(async move {
            let signal = AbortSignal::default();
            let wait = tokio::task::spawn_blocking({ let manager = self.manager.clone(); let id = task_id.to_string(); let signal = signal.clone(); move || manager.wait_for(&id, Some(&signal), None) });
            tokio::pin!(wait);
            tokio::select! {
                result = &mut wait => result.map_err(|error| WorkpoolAdmissionError(error.to_string()))?.map_err(|error| WorkpoolAdmissionError(error.to_string())),
                changed = cancel.changed() => {
                    let _ = changed;
                    signal.abort();
                    let _settled = wait.await.map_err(|error| WorkpoolAdmissionError(error.to_string()))?;
                    Err(WorkpoolAdmissionError("waitFor aborted".into()))
                }
            }
        })
    }
}

fn event(request: &dyn WorkpoolRequest, kind: crate::workpool::types::WorkpoolEventKind, task_id: &str, epoch: i64, error: Option<(WorkpoolErrorCode, String)>) -> crate::workpool::types::WorkpoolEvent {
    crate::workpool::types::WorkpoolEvent { pool_id: request.pool().pool_id.clone(), kind, item_id: Some(request.item().item_id.clone()), task_id: Some(task_id.to_string()), run_epoch: Some(u32::try_from(epoch).unwrap_or_default()), error: error.map(|(code,message)| crate::workpool::types::WorkpoolEventError { code, message }) }
}

