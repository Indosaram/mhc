use super::dispatcher::WorkpoolDispatcher;
use super::ports::WorkpoolAdmission;
use super::store::WorkpoolStore;
use super::types::{PoolId, WorkpoolCaller, WorkpoolCreate, WorkpoolError, WorkpoolRecord};

/// Persistent workpool engine state. Admission remains a host-owned port, as
/// in senpi; this module owns pool records and dispatch scheduling.
pub struct WorkpoolEngine {
    pub store: WorkpoolStore,
    pub admission: std::sync::Arc<dyn WorkpoolAdmission + Send + Sync>,
    pub dispatcher: WorkpoolDispatcher,
    attached_sessions: std::sync::Mutex<std::collections::BTreeSet<String>>,
    events: std::sync::Arc<dyn Fn(super::types::WorkpoolEvent) + Send + Sync>,
    aggregate_port: Option<std::sync::Arc<dyn super::aggregate::WorkpoolAggregatePort>>,
}

impl WorkpoolEngine {
    pub fn new(state_dir: impl Into<std::path::PathBuf>, admission: std::sync::Arc<dyn WorkpoolAdmission + Send + Sync>) -> Self {
        Self::with_events(state_dir, admission, std::sync::Arc::new(|_| {}))
    }

    pub fn with_events(
        state_dir: impl Into<std::path::PathBuf>,
        admission: std::sync::Arc<dyn WorkpoolAdmission + Send + Sync>,
        emit: std::sync::Arc<dyn Fn(super::types::WorkpoolEvent) + Send + Sync>,
    ) -> Self {
        let store = WorkpoolStore::new(state_dir);
        let policy = admission.clone();
        let dispatcher = WorkpoolDispatcher::new(store.clone(), admission.clone(), emit.clone(), std::sync::Arc::new(move |pool| {
            policy.authorize_spawn(&pool.agent, &pool.parent_session_id)
        }));
        Self { store, admission, dispatcher, attached_sessions: Default::default(), events: emit, aggregate_port: None }
    }

    pub fn attach(&self, session_id: &str, credential: Option<crate::pause::admission_barrier::ResumeCredential>) -> Result<(), WorkpoolError> {
        let fresh = self.attached_sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session_id.into());
        for pool in self.store.list_result()?.into_iter().filter(|pool| pool.parent_session_id == session_id) {
            if fresh && pool.aggregate.is_some_and(|aggregate| aggregate.accepted == Some(true) && !aggregate.delivered) {
                self.store.mutate(&pool.pool_id, |record| { let mut next = record.clone(); if let Some(aggregate) = &mut next.aggregate { aggregate.accepted = None; } next })?;
            }
            if pool.hold.is_none() { self.dispatcher.attach_with_credential(&pool.pool_id, credential.clone())?; }
            self.flush_aggregate(&pool.pool_id)?;
        }
        Ok(())
    }
    pub fn recover(&self) -> Result<(), WorkpoolError> {
        self.dispatcher.attach_all()
    }

    pub fn owned_task_ids(&self) -> Result<std::collections::BTreeSet<String>, WorkpoolError> {
        Ok(self.store.list_result()?.into_iter().flat_map(|pool| {
            pool.workers.into_iter().map(|worker| worker.task_id)
                .chain(pool.items.into_iter().filter_map(|item| item.binding.map(|binding| binding.task_id))).collect::<Vec<_>>()
        }).collect())
    }

    pub fn pause_for_run(&self, caller: &WorkpoolCaller, pool_id: &str, pause_seq: u64) -> Result<(), WorkpoolError> {
        super::run_pause::PoolRunPause { store: &self.store, admission: self.admission.as_ref(), dispatcher: &self.dispatcher }.pause_for_run(caller, pool_id, pause_seq)
    }

    pub fn resume_for_run(&self, caller: &WorkpoolCaller, pool_id: &str, pause_seq: u64, credential: Option<crate::pause::admission_barrier::ResumeCredential>) -> Result<super::run_pause::ResumePoolResult, WorkpoolError> {
        super::run_pause::PoolRunPause { store: &self.store, admission: self.admission.as_ref(), dispatcher: &self.dispatcher }.resume_for_run(caller, pool_id, pause_seq, credential)
    }

    pub fn dispose(&self) {
        self.dispatcher.stop_scheduling();
        for pool in self.store.list() {
            self.admission.unbind_pool_kernel_tools(&pool.pool_id);
        }
    }

    pub fn set_aggregate_port(&mut self, port: std::sync::Arc<dyn super::aggregate::WorkpoolAggregatePort>) {
        self.aggregate_port = Some(port);
    }

    pub fn flush_aggregate(&self, pool_id: &str) -> Result<bool, WorkpoolError> {
        let pool = self.store.load(pool_id)?;
        // A paused run holds its aggregate until Resume (`workpool/engine.ts` `flushAggregate`).
        if pool.hold.is_some() || self.admission.delivery_held(&pool.root_session_id) {
            return Ok(false);
        }
        if pool.aggregate.is_some_and(|state| state.generation == pool.generation && (state.delivered || state.accepted == Some(true))) {
            return Ok(false);
        }
        let store = self.store.clone();
        let id = pool.pool_id.clone();
        let generation = pool.generation;
        let events = self.events.clone();
        let persistence_events = events.clone();
        let persist = std::sync::Arc::new(move |state: super::aggregate::WorkpoolAggregateState| {
            if let Err(error) = store.mutate(&id, |prior| {
                let mut next = prior.clone();
                if next.generation == generation {
                    next.aggregate = Some(super::types::WorkpoolAggregate {
                        generation, delivered: state.delivered, accepted: if state.delivered { None } else { Some(state.accepted) },
                    });
                }
                next
            }) {
                persistence_events(super::types::WorkpoolEvent {
                    pool_id: id.clone(), kind: super::types::WorkpoolEventKind::AggregateFailed,
                    item_id: None, task_id: None, run_epoch: None,
                    error: Some(super::types::WorkpoolEventError { code: error.code, message: error.message }),
                });
            }
        });
        let failure_id = pool.pool_id.clone();
        Ok(super::aggregate::deliver_aggregate(&pool, self.aggregate_port.as_deref(), persist, Some(std::sync::Arc::new(move |message| {
            events(super::types::WorkpoolEvent {
                pool_id: failure_id.clone(), kind: super::types::WorkpoolEventKind::AggregateFailed,
                item_id: None, task_id: None, run_epoch: None,
                error: Some(super::types::WorkpoolEventError { code: super::types::WorkpoolErrorCode::DeliveryUncertain, message }),
            });
        }))))
    }

    pub fn inspect(&self, caller: &WorkpoolCaller, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        self.assert_parent(caller)?;
        self.store.owned(caller, pool_id)
    }

    fn assert_parent(&self, caller: &WorkpoolCaller) -> Result<(), WorkpoolError> {
        if caller.session_id.trim().is_empty() {
            return Err(WorkpoolError::new(super::types::WorkpoolErrorCode::ScopeDenied, "A host session identity is required."));
        }
        let assigned: std::collections::BTreeSet<String> = self.store.list_result()?.into_iter().flat_map(|pool| {
            let WorkpoolRecord { workers, items, .. } = pool;
            workers.into_iter().map(|worker| worker.task_id)
                .chain(items.into_iter().filter_map(|item| item.binding.map(|binding| binding.task_id)))
                .collect::<Vec<_>>()
        }).collect();
        for id in &assigned {
            if self.admission.get(id).is_some_and(|task| task.child_session_id.as_deref() == Some(caller.session_id.as_str())) {
                return Err(WorkpoolError::new(super::types::WorkpoolErrorCode::ScopeDenied, "Assigned workers may only yield their current assignment."));
            }
        }
        Ok(())
    }

    pub fn schedule(&mut self, pool_id: &PoolId) { self.dispatcher.schedule(pool_id.clone()); }

    pub fn push(&mut self, caller: &WorkpoolCaller, pool_id: &str, values: &[super::types::WorkpoolInput]) -> Result<serde_json::Value, WorkpoolError> {
        let owned = self.inspect(caller, pool_id)?;
        if values.iter().any(|value| !super::schema::nonempty(&value.key)) {
            return Err(WorkpoolError::new(super::types::WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments."));
        }
        let mut receipts = Vec::new();
        self.store.mutate_result(&owned.pool_id, |pool| {
            if pool.status != super::types::WorkpoolStatus::Open {
                return Err(WorkpoolError::new(super::types::WorkpoolErrorCode::PoolClosed, "This pool no longer accepts items."));
            }
            let mut next = pool.clone();
            for value in values {
                let key = value.key.trim();
                let item = if let Some(prior) = next.items.iter().find(|item| item.key == key) {
                    if super::schema::canonical_json(&prior.input) != super::schema::canonical_json(&value.input) {
                        return Err(WorkpoolError::new(super::types::WorkpoolErrorCode::DuplicateKeyConflict, "The key already identifies different input."));
                    }
                    prior.clone()
                } else {
                    let item = super::types::WorkpoolItem {
                        key: key.to_owned(), input: value.input.clone(), item_id: format!("wi_{}", uuid::Uuid::new_v4().simple()),
                        status: super::types::WorkpoolItemStatus::Queued, binding: None, delivery: None, data: None, error: None, yield_sha256: None,
                    };
                    next.items.push(item.clone());
                    item
                };
                receipts.push(serde_json::json!({"key": item.key, "item_id": item.item_id}));
            }
            Ok(next)
        })?;
        self.schedule(&owned.pool_id);
        (self.events)(super::types::WorkpoolEvent {
            pool_id: owned.pool_id.clone(), kind: super::types::WorkpoolEventKind::Queued,
            item_id: None, task_id: None, run_epoch: None, error: None,
        });
        Ok(serde_json::json!({"pool_id": owned.pool_id, "item_ids": receipts}))
    }

    pub fn close(&self, caller: &WorkpoolCaller, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        let owned = self.inspect(caller, pool_id)?;
        let closed = self.store.mutate(&owned.pool_id, |pool| {
            let mut next = pool.clone();
            if next.status == super::types::WorkpoolStatus::Open { next.status = super::types::WorkpoolStatus::Closing; }
            next
        })?;
        self.flush_aggregate(&owned.pool_id)?;
        Ok(closed)
    }

    pub fn create(&mut self, caller: &WorkpoolCaller, input: &WorkpoolCreate) -> Result<WorkpoolRecord, WorkpoolError> {
        self.assert_parent(caller)?;
        self.admission.authorize_spawn(&input.agent, &caller.session_id)
            .map_err(|error| WorkpoolError::new(super::types::WorkpoolErrorCode::PolicyDenied, error.0))?;
        let spec = self.admission.resolve(caller, &input.agent).map_err(|error| WorkpoolError::new(super::types::WorkpoolErrorCode::AdmissionRefused, error.0))?;
        let mode = input.mode.unwrap_or(super::default_mode::WORKPOOL_DEFAULT_MODE);
        let pool = self.store.create(caller, input, mode, spec)?;
        self.schedule(&pool.pool_id);
        Ok(pool)
    }

    pub async fn cancel(&mut self, caller: &WorkpoolCaller, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        let owned = self.inspect(caller, pool_id)?;
        let cancelled = self.store.mutate(pool_id, |pool| {
            if pool.status == super::types::WorkpoolStatus::Cancelled { return pool.clone(); }
            let mut next = pool.clone();
            next.status = super::types::WorkpoolStatus::Cancelled;
            next.generation += 1;
            for item in &mut next.items {
                if matches!(item.status, super::types::WorkpoolItemStatus::Queued | super::types::WorkpoolItemStatus::Assigned) {
                    item.status = super::types::WorkpoolItemStatus::Cancelled;
                    item.error = Some(super::types::WorkpoolYieldError { code: "cancelled".into(), message: "Pool cancelled.".into() });
                }
            }
            next
        })?;
        self.dispatcher.cancel(&owned.pool_id);
        self.admission.unbind_pool_kernel_tools(&owned.pool_id);
        for worker in &cancelled.workers {
            self.admission.cancel(&worker.task_id).await
                .map_err(|error| WorkpoolError::new(super::types::WorkpoolErrorCode::AdmissionRefused, error.0))?;
        }
        (self.events)(super::types::WorkpoolEvent {
            pool_id: owned.pool_id, kind: super::types::WorkpoolEventKind::Cancelled,
            item_id: None, task_id: None, run_epoch: None, error: None,
        });
        Ok(cancelled)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::state::TaskRecord;
    use crate::store::{StateDirConfig, TaskRecordStore};

    use super::*;
    use crate::workpool::ports::{WorkpoolAdmissionError, WorkpoolRequest};
    use crate::workpool::types::{WorkpoolAgent, WorkpoolSpec, WorkpoolWorker, WorkpoolWorkerStatus};

    struct Admission {
        tasks: TaskRecordStore,
        denied: bool,
        cancelled: AtomicBool,
    }

    impl WorkpoolAdmission for Admission {
        fn tasks(&self) -> &TaskRecordStore { &self.tasks }
        fn bind_pool_kernel_tools(&self, _: &str, _: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError> { Ok(()) }
        fn unbind_pool_kernel_tools(&self, _: &str) {}
        fn resolve(&self, _caller: &WorkpoolCaller, _agent: &WorkpoolAgent) -> Result<WorkpoolSpec, WorkpoolAdmissionError> {
            Ok(WorkpoolSpec { start: Default::default(), plan: Default::default() })
        }
        fn authorize_spawn(&self, _agent: &WorkpoolAgent, _parent_session_id: &str) -> Result<(), WorkpoolAdmissionError> {
            if self.denied { Err(WorkpoolAdmissionError("spawn denied".into())) } else { Ok(()) }
        }
        fn has_free_slot(&self, _model: &str) -> bool { true }
        fn request(&self, _input: Box<dyn WorkpoolRequest>) -> crate::workpool::ports::WorkpoolRequestHandle { crate::workpool::ports::WorkpoolRequestHandle::dispatched(Box::new(|| {})) }
        fn drain(&self) {}
        fn get(&self, _task_id: &str) -> Option<TaskRecord> { None }
        fn task_run_epoch(&self, _task_id: &str) -> Option<u32> { None }
        fn pending(&self, _task_id: &str) -> bool { false }
        fn cancel<'a>(&'a self, _task_id: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolAdmissionError>> + Send + 'a>> {
            Box::pin(async move { self.cancelled.store(true, Ordering::SeqCst); Ok(()) })
        }
        fn wait_for<'a>(&'a self, _task_id: &'a str, _cancel: &'a mut tokio::sync::watch::Receiver<bool>) -> Pin<Box<dyn Future<Output = Result<TaskRecord, WorkpoolAdmissionError>> + Send + 'a>> {
            Box::pin(async { Err(WorkpoolAdmissionError("unused".into())) })
        }
    }

    fn admission(denied: bool) -> Admission {
        Admission { tasks: TaskRecordStore::new(&StateDirConfig::default()), denied, cancelled: AtomicBool::new(false) }
    }

    fn caller() -> WorkpoolCaller { WorkpoolCaller { session_id: "parent".into(), root_session_id: "root".into(), depth: 0, cwd: "/tmp".into() } }

    fn input() -> WorkpoolCreate {
        WorkpoolCreate { name: "engine-test".into(), agent: WorkpoolAgent::Category { prompt: "run".into(), model: None, category: "quick".into() }, mode: None, tools: None }
    }

    #[test]
    fn engine_module_is_registered() {
        let root = tempfile::tempdir().expect("temporary state directory");
        let engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        assert!(!engine.dispatcher.is_disposed());
    }

    #[tokio::test]
    async fn keyed_push_is_idempotent_and_conflict_is_atomic() {
        let root = tempfile::tempdir().expect("temporary state directory");
        let mut engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        let pool = engine.create(&caller(), &input()).expect("pool creation");
        let values = [super::super::types::WorkpoolInput { key: " a ".into(), input: serde_json::json!({"b": 2, "a": 1}) }];
        let first = engine.push(&caller(), &pool.pool_id, &values).expect("first push");
        let repeated = engine.push(&caller(), &pool.pool_id, &values).expect("idempotent push");
        assert_eq!(first, repeated);
        let before = engine.store.load(&pool.pool_id).expect("saved receipt");
        assert_eq!(before.items[0].key, "a");
        let conflict = [
            super::super::types::WorkpoolInput { key: "new".into(), input: serde_json::Value::Null },
            super::super::types::WorkpoolInput { key: "a".into(), input: serde_json::json!(3) },
        ];
        assert_eq!(engine.push(&caller(), &pool.pool_id, &conflict).expect_err("conflict").code, super::super::types::WorkpoolErrorCode::DuplicateKeyConflict);
        let after = engine.store.load(&pool.pool_id).expect("unchanged pool");
        assert_eq!(after.items, before.items);
        assert_eq!(after.revision, before.revision);
        engine.push(&caller(), &pool.pool_id, &[]).expect("empty push is allowed");
        assert_eq!(engine.close(&caller(), &pool.pool_id).expect("close").status, super::super::types::WorkpoolStatus::Closing);
        assert_eq!(engine.push(&caller(), &pool.pool_id, &values).expect_err("closed").code, super::super::types::WorkpoolErrorCode::PoolClosed);
    }

    #[tokio::test]
    async fn closing_aggregate_waits_for_ack_without_duplicate_enqueue() {
        let root = tempfile::tempdir().expect("temporary state directory");
        let mut engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        let receipts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = receipts.clone();
        engine.set_aggregate_port(std::sync::Arc::new(move |_: super::super::aggregate::WorkpoolAggregateMessage, receipt| {
            captured.lock().expect("receipts").push(receipt);
            Ok(())
        }));
        let pool = engine.create(&caller(), &input()).expect("pool creation");
        engine.close(&caller(), &pool.pool_id).expect("close");
        assert!(!engine.flush_aggregate(&pool.pool_id).expect("pending ack"));
        let receipt = receipts.lock().expect("receipts").pop().expect("one receipt");
        assert!(receipts.lock().expect("receipts").is_empty());
        (receipt.ack)();
        let saved = engine.store.load(&pool.pool_id).expect("saved ack");
        assert!(saved.aggregate.expect("aggregate").delivered);
        assert!(!engine.flush_aggregate(&pool.pool_id).expect("already delivered"));
    }

    #[tokio::test]
    async fn run_pause_persists_hold_and_rejects_stale_resume() {
        let root = tempfile::tempdir().expect("state");
        let mut engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        let pool = engine.create(&caller(), &input()).expect("pool");
        engine.pause_for_run(&caller(), &pool.pool_id, 4).expect("pause");
        assert_eq!(engine.store.load(&pool.pool_id).expect("record").hold.expect("hold").pause_seq, 4);
        let barrier = crate::pause::admission_barrier::AdmissionBarrier::new();
        barrier.close("parent", 4);
        let credential = barrier.resume("parent", 4, "resume").expect("credential");
        assert_eq!(engine.resume_for_run(&caller(), &pool.pool_id, 3, Some(credential.clone())).expect("stale"), super::super::run_pause::ResumePoolResult::Stale);
        assert_eq!(engine.resume_for_run(&caller(), &pool.pool_id, 4, Some(credential)).expect("resume"), super::super::run_pause::ResumePoolResult::Released);
        assert!(engine.store.load(&pool.pool_id).expect("record").hold.is_none());
    }
    #[tokio::test]
    async fn fresh_engine_attachment_redelivers_only_accepted_not_delivered_aggregate() {
        let root = tempfile::tempdir().expect("state");
        let mut engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        engine.set_aggregate_port(std::sync::Arc::new(|_, _| Ok(())));
        let pool = engine.create(&caller(), &input()).expect("pool");
        engine.close(&caller(), &pool.pool_id).expect("close");
        let mut resumed = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        let deliveries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)); let count = deliveries.clone();
        resumed.set_aggregate_port(std::sync::Arc::new(move |_, receipts: super::super::aggregate::WorkpoolAggregateReceipts| { count.fetch_add(1, Ordering::SeqCst); (receipts.ack)(); Ok(()) }));
        resumed.attach("parent", None).expect("attach"); resumed.attach("parent", None).expect("attach delivered");
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn create_denies_before_resolving_or_storing_pool() {
        let admission = admission(true);
        let root = tempfile::tempdir().expect("temporary state directory");
        let mut engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission));
        let error = engine.create(&caller(), &input()).err().expect("policy must deny");
        assert_eq!(error.code, super::super::types::WorkpoolErrorCode::PolicyDenied);
        assert!(engine.store.list().is_empty());
        assert!(!engine.dispatcher.take_scheduled("wp_missing"));
    }

    #[test]
    fn create_requires_host_session_identity_before_admission() {
        let root = tempfile::tempdir().expect("temporary state directory");
        let mut engine = WorkpoolEngine::new(root.path(), std::sync::Arc::new(admission(false)));
        let blank = WorkpoolCaller { session_id: " ".into(), ..caller() };
        assert_eq!(engine.create(&blank, &input()).err().expect("scope denied").code, super::super::types::WorkpoolErrorCode::ScopeDenied);
        assert!(engine.store.list_result().expect("pool list").is_empty());
    }

    #[tokio::test]
    async fn cancellation_future_is_executed_for_each_worker() {
        let admission = admission(false);
        let admission = std::sync::Arc::new(admission);
        let root = tempfile::tempdir().expect("temporary state directory");
        let mut engine = WorkpoolEngine::new(root.path(), admission.clone());
        let pool = engine.create(&caller(), &input()).expect("pool creation");
        engine.store.mutate(&pool.pool_id, |pool| {
            let mut next = pool.clone();
            next.workers.push(WorkpoolWorker { task_id: "st_00000001".into(), run_epoch: 1, status: WorkpoolWorkerStatus::Busy, completed_turns: 0, idle_since: 0 });
            next
        }).expect("worker fixture");

        engine.cancel(&caller(), &pool.pool_id).await.expect("pool cancellation");
        assert!(admission.cancelled.load(Ordering::SeqCst));
    }
}
