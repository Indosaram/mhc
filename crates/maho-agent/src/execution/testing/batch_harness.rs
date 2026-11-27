//! Port of senpi `packages/agent/src/execution/testing/batch-harness.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::durable::store::{
    AdoptCallback, CommitResult, DurableStore, DurableStoreError, DurableStoreOptions, RecordScan,
    StoreDocument, StoreRecord, StoreSnapshot, StoreValue, StoreWrite,
};
use crate::durable::store_admission::create_admission_line;
use crate::execution::coordinator::{
    BatchCoordinator, BatchCoordinatorOptions, create_batch_coordinator,
};
use crate::execution::coordinator_types::{
    BatchCallPlan, BatchConsent, BatchExecutor, BatchPlan, CallOutcome,
};
use crate::execution::nested::{
    NestedAdmissionControl, NestedAdmissionOptions, create_nested_admission,
};
use crate::execution::scope_lifecycle::{
    ScopeLifecycle, ScopeLifecycleOptions, create_scope_lifecycle,
};
use crate::execution::wake_inbox::{OwnerLookup, WakeInbox, WakeInboxOptions, create_wake_inbox};
use crate::execution::wake_outbox::{WakeOutbox, WakeOutboxOptions, create_wake_outbox};

use super::batch_executor::{BatchCaseAdapters, ExecutorOpenOptions, PausePoint};

pub const ADMITTED_AT: i64 = 100;
pub const NOW: i64 = ADMITTED_AT + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainKill {
    pub label: String,
    pub at: BrainKillAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainKillAt {
    Before,
    After,
}

impl BrainKillAt {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("brain killed around {0}")]
pub struct BrainKilledError(pub String);

fn label_of(writes: &[StoreWrite]) -> Option<String> {
    for write in writes {
        if let StoreWrite::Append { kind, data, .. } = write
            && kind.starts_with("execution.")
            && let Some(obj) = data.as_object()
        {
            let step = obj
                .get("step")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    obj.get("step")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "undefined".to_string())
                });
            if let Some(exec_id) = obj.get("executionId").and_then(Value::as_str) {
                return Some(format!("{step}:{exec_id}"));
            }
            return Some(step);
        }
    }
    None
}

struct BrainKillStore {
    inner: Arc<dyn DurableStore>,
    on_durable: Arc<dyn Fn(&str) + Send + Sync>,
    kill: Option<BrainKill>,
    dead: AtomicBool,
}

impl DurableStore for BrainKillStore {
    fn read_epoch(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        self.inner.read_epoch()
    }

    fn read_head(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        self.inner.read_head()
    }

    fn claim(&self, expected: u64, next: u64) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        self.inner.claim(expected, next)
    }

    fn commit<'a>(
        &'a self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        adopt: AdoptCallback<'a>,
    ) -> BoxFuture<'a, Result<CommitResult, DurableStoreError>> {
        Box::pin(async move {
            let label = label_of(&writes).unwrap_or_default();
            if self.dead.load(Ordering::SeqCst) {
                return Err(DurableStoreError::Other(
                    BrainKilledError(label).to_string(),
                ));
            }
            if let Some(kill) = &self.kill
                && label == kill.label
                && kill.at == BrainKillAt::Before
            {
                self.dead.store(true, Ordering::SeqCst);
                return Err(DurableStoreError::Other(
                    BrainKilledError(label).to_string(),
                ));
            }
            let result = self.inner.commit(epoch, writes, adopt).await?;
            (self.on_durable)(&label);
            if let Some(kill) = &self.kill
                && label == kill.label
            {
                self.dead.store(true, Ordering::SeqCst);
                return Err(DurableStoreError::Other(
                    BrainKilledError(label).to_string(),
                ));
            }
            Ok(result)
        })
    }

    fn mint_id(&self) -> String {
        self.inner.mint_id()
    }

    fn scan_records(
        &self,
        query: RecordScan,
    ) -> BoxFuture<'_, Result<Vec<StoreRecord>, DurableStoreError>> {
        self.inner.scan_records(query)
    }

    fn get_document(
        &self,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<StoreValue>, DurableStoreError>> {
        self.inner.get_document(key)
    }

    fn scan_documents(
        &self,
        prefix: &str,
    ) -> BoxFuture<'_, Result<Vec<StoreDocument>, DurableStoreError>> {
        self.inner.scan_documents(prefix)
    }

    fn export_snapshot(&self) -> BoxFuture<'_, Result<StoreSnapshot, DurableStoreError>> {
        self.inner.export_snapshot()
    }

    fn import_snapshot(
        &self,
        epoch: u64,
        snapshot: StoreSnapshot,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        self.inner.import_snapshot(epoch, snapshot)
    }
}

struct DeadAfterStore {
    inner: Arc<dyn DurableStore>,
    killed: AbortSignal,
}

impl DurableStore for DeadAfterStore {
    fn read_epoch(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        self.inner.read_epoch()
    }

    fn read_head(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        self.inner.read_head()
    }

    fn claim(&self, expected: u64, next: u64) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        self.inner.claim(expected, next)
    }

    fn commit<'a>(
        &'a self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        adopt: AdoptCallback<'a>,
    ) -> BoxFuture<'a, Result<CommitResult, DurableStoreError>> {
        if self.killed.aborted() {
            return Box::pin(async {
                Err(DurableStoreError::Other(
                    "a killed executor cannot write".to_string(),
                ))
            });
        }
        self.inner.commit(epoch, writes, adopt)
    }

    fn mint_id(&self) -> String {
        self.inner.mint_id()
    }

    fn scan_records(
        &self,
        query: RecordScan,
    ) -> BoxFuture<'_, Result<Vec<StoreRecord>, DurableStoreError>> {
        self.inner.scan_records(query)
    }

    fn get_document(
        &self,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<StoreValue>, DurableStoreError>> {
        self.inner.get_document(key)
    }

    fn scan_documents(
        &self,
        prefix: &str,
    ) -> BoxFuture<'_, Result<Vec<StoreDocument>, DurableStoreError>> {
        self.inner.scan_documents(prefix)
    }

    fn export_snapshot(&self) -> BoxFuture<'_, Result<StoreSnapshot, DurableStoreError>> {
        self.inner.export_snapshot()
    }

    fn import_snapshot(
        &self,
        epoch: u64,
        snapshot: StoreSnapshot,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        self.inner.import_snapshot(epoch, snapshot)
    }
}

pub type CallSpec = (&'static str, &'static str, &'static str, bool);

pub fn call_spec(execution_id: &'static str, executor: &'static str) -> CallSpec {
    (execution_id, executor, "read", false)
}

pub fn batch_plan(batch_id: &str, calls: &[CallSpec]) -> BatchPlan {
    BatchPlan {
        batch_id: batch_id.to_string(),
        scope: "scope".to_string(),
        workspace: "workspace".to_string(),
        generation: 1,
        admitted_at: ADMITTED_AT,
        lease_until: ADMITTED_AT + 60_000,
        calls: calls
            .iter()
            .map(
                |&(execution_id, executor, name, sequential)| BatchCallPlan {
                    execution_id: execution_id.to_string(),
                    executor: executor.to_string(),
                    name: name.to_string(),
                    args: json!({ "executionId": execution_id }),
                    sequential,
                },
            )
            .collect(),
    }
}

pub fn result_of(execution_id: &str, name: &str) -> CallOutcome {
    CallOutcome::Result {
        value: json!({ "executionId": execution_id, "name": name }),
    }
}

#[derive(Clone, Default)]
struct Gate {
    reached: Arc<AtomicBool>,
    reached_notify: Arc<Notify>,
    open_configured: Arc<AtomicBool>,
    open: Arc<AtomicBool>,
    open_notify: Arc<Notify>,
}

impl Gate {
    async fn wait_reached(&self) {
        loop {
            let notified = self.reached_notify.notified();
            if self.reached.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    async fn wait_open(&self) {
        if !self.open_configured.load(Ordering::SeqCst) {
            return;
        }
        loop {
            let notified = self.open_notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn mark_reached(&self) {
        self.reached.store(true, Ordering::SeqCst);
        self.reached_notify.notify_waiters();
    }

    fn set_open_gate(&self) {
        self.open_configured.store(true, Ordering::SeqCst);
    }

    fn trigger_open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.open_notify.notify_waiters();
    }
}

pub struct HoldHandle {
    gate: Gate,
}

impl HoldHandle {
    pub async fn reached(&self) {
        self.gate.wait_reached().await;
    }

    pub fn open(&self) {
        self.gate.trigger_open();
    }
}

#[derive(Clone)]
struct ExecutorState {
    medium: String,
    killed: AbortController,
    endpoint: Arc<dyn BatchExecutor>,
}

struct ExecutorProxy {
    id: String,
    executors: Arc<Mutex<HashMap<String, ExecutorState>>>,
}

impl BatchExecutor for ExecutorProxy {
    fn prepare(
        &self,
        request: crate::execution::coordinator_types::PrepareRequest,
    ) -> BoxFuture<'_, Result<crate::execution::coordinator_types::PrepareReply, String>> {
        let ep = {
            let guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&self.id).map(|e| e.endpoint.clone())
        };
        Box::pin(async move {
            let Some(ep) = ep else {
                return Err(format!("no executor {}", self.id));
            };
            ep.prepare(request).await
        })
    }

    fn release(
        &self,
        exec_ref: crate::execution::coordinator_types::ExecutionRef,
    ) -> BoxFuture<'_, Result<crate::execution::coordinator_types::SettleReply, String>> {
        let ep = {
            let guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&self.id).map(|e| e.endpoint.clone())
        };
        Box::pin(async move {
            let Some(ep) = ep else {
                return Err(format!("no executor {}", self.id));
            };
            ep.release(exec_ref).await
        })
    }

    fn abort(
        &self,
        exec_ref: crate::execution::coordinator_types::ExecutionRef,
    ) -> BoxFuture<'_, Result<crate::execution::coordinator_types::SettleReply, String>> {
        let ep = {
            let guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&self.id).map(|e| e.endpoint.clone())
        };
        Box::pin(async move {
            let Some(ep) = ep else {
                return Err(format!("no executor {}", self.id));
            };
            ep.abort(exec_ref).await
        })
    }

    fn consent(
        &self,
        exec_ref: crate::execution::coordinator_types::ExecutionRef,
        frozen_digest: &str,
    ) -> Option<BoxFuture<'_, Result<crate::execution::coordinator_types::ConsentStepReply, String>>>
    {
        let ep = {
            let guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(&self.id).map(|e| e.endpoint.clone())
        };
        let digest = frozen_digest.to_string();
        Some(Box::pin(async move {
            let Some(ep) = ep else {
                return Err(format!("no executor {}", self.id));
            };
            let Some(fut) = ep.consent(exec_ref, &digest) else {
                return Err(format!("executor {} takes no consent", self.id));
            };
            fut.await
        }))
    }
}

pub struct WrittenWaiter {
    state: Arc<Mutex<Option<Vec<String>>>>,
    notify: Arc<Notify>,
}

impl WrittenWaiter {
    pub async fn wait(self) -> Vec<String> {
        loop {
            let notified = self.notify.notified();
            if let Some(writes) = self.state.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                return writes;
            }
            notified.await;
        }
    }
}

type WriteWaiterMap = Arc<Mutex<HashMap<String, (Arc<Mutex<Option<Vec<String>>>>, Arc<Notify>)>>>;

pub struct BatchHarness {
    adapters: BatchCaseAdapters,
    trace: Arc<Mutex<Vec<String>>>,
    gates: Arc<Mutex<HashMap<String, Gate>>>,
    clock: Arc<AtomicI64>,
    executors: Arc<Mutex<HashMap<String, ExecutorState>>>,
    brain_medium: String,
    wake_media: Mutex<HashMap<String, String>>,
    scope_medium: Mutex<Option<String>>,
    brain_writes: Arc<Mutex<Vec<String>>>,
    write_waiters: WriteWaiterMap,
}

impl BatchHarness {
    async fn claimed(&self) -> Result<String, String> {
        claimed_medium(&self.adapters).await
    }

    async fn start_executor(&self, id: &str, medium: String) -> Result<(), String> {
        let killed = AbortController::new();
        let inner = self
            .adapters
            .store
            .open(&medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(DeadAfterStore {
            inner,
            killed: killed.signal(),
        });
        let gates = Arc::clone(&self.gates);
        let pause: super::batch_executor::PauseFn = Arc::new(move |execution_id, point| {
            let key = format!("{execution_id}/{}", point.as_str());
            let gate = {
                let mut guard = gates.lock().unwrap_or_else(|p| p.into_inner());
                guard.entry(key).or_default().clone()
            };
            Box::pin(async move {
                gate.mark_reached();
                gate.wait_open().await;
            })
        });
        let clock = Arc::clone(&self.clock);
        let endpoint = self
            .adapters
            .executor
            .open(ExecutorOpenOptions {
                id: id.to_string(),
                store,
                epoch: 1,
                now: Arc::new(move || clock.load(Ordering::SeqCst)),
                trace: Arc::clone(&self.trace),
                pause,
                killed: killed.signal(),
            })
            .await?;
        let mut guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
        guard.insert(
            id.to_string(),
            ExecutorState {
                medium,
                killed,
                endpoint,
            },
        );
        Ok(())
    }

    fn kill_executor(&self, id: &str) -> Result<String, String> {
        let guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.get(id).ok_or_else(|| format!("no executor {id}"))?;
        state.killed.abort(None);
        Ok(state.medium.clone())
    }

    fn on_durable_callback(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        let brain_writes = Arc::clone(&self.brain_writes);
        let write_waiters = Arc::clone(&self.write_waiters);
        Arc::new(move |label: &str| {
            let snapshot = {
                let mut writes = brain_writes.lock().unwrap_or_else(|p| p.into_inner());
                writes.push(label.to_string());
                writes.clone()
            };
            let waiter = {
                let mut waiters = write_waiters.lock().unwrap_or_else(|p| p.into_inner());
                waiters.remove(label)
            };
            if let Some((slot, notify)) = waiter {
                *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(snapshot);
                notify.notify_waiters();
            }
        })
    }

    pub fn trace(&self) -> Vec<String> {
        self.trace.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn count(&self, event: &str) -> usize {
        self.trace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|entry| entry.as_str() == event)
            .count()
    }

    pub fn written(&self, label: &str) -> WrittenWaiter {
        let slot = Arc::new(Mutex::new(None));
        let notify = Arc::new(Notify::new());
        let mut waiters = self.write_waiters.lock().unwrap_or_else(|p| p.into_inner());
        waiters.insert(label.to_string(), (Arc::clone(&slot), Arc::clone(&notify)));
        WrittenWaiter {
            state: slot,
            notify,
        }
    }

    pub fn hold(&self, execution_id: &str, point: PausePoint) -> HoldHandle {
        let key = format!("{execution_id}/{}", point.as_str());
        let mut guard = self.gates.lock().unwrap_or_else(|p| p.into_inner());
        let gate = guard.entry(key).or_default().clone();
        gate.set_open_gate();
        HoldHandle { gate }
    }

    pub fn executor_proxy(&self, id: &str) -> Arc<dyn BatchExecutor> {
        Arc::new(ExecutorProxy {
            id: id.to_string(),
            executors: Arc::clone(&self.executors),
        })
    }

    pub async fn brain(
        &self,
        die_at: Option<BrainKill>,
        issuer: Option<&str>,
        consent: Option<Arc<dyn BatchConsent>>,
    ) -> Result<Arc<dyn BatchCoordinator>, String> {
        let opened = self
            .adapters
            .store
            .open(&self.brain_medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(BrainKillStore {
            inner: opened,
            on_durable: self.on_durable_callback(),
            kill: die_at,
            dead: AtomicBool::new(false),
        });
        let executors = Arc::clone(&self.executors);
        let executor_factory: crate::execution::coordinator_types::BatchExecutorFactory =
            Arc::new(move |id: &str| {
                Arc::new(ExecutorProxy {
                    id: id.to_string(),
                    executors: Arc::clone(&executors),
                })
            });
        Ok(create_batch_coordinator(BatchCoordinatorOptions {
            issuer: issuer.unwrap_or("brain").to_string(),
            store,
            epoch: 1,
            admission: create_admission_line(),
            executor: executor_factory,
            consent,
        }))
    }

    pub async fn nested(
        &self,
        die_at: Option<BrainKill>,
        issuer: Option<&str>,
    ) -> Result<Arc<dyn NestedAdmissionControl>, String> {
        let opened = self
            .adapters
            .store
            .open(&self.brain_medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(BrainKillStore {
            inner: opened,
            on_durable: self.on_durable_callback(),
            kill: die_at,
            dead: AtomicBool::new(false),
        });
        let clock = Arc::clone(&self.clock);
        Ok(create_nested_admission(NestedAdmissionOptions {
            issuer: issuer.unwrap_or("brain").to_string(),
            store,
            epoch: 1,
            admission: create_admission_line(),
            clock: Arc::new(move || clock.load(Ordering::SeqCst)),
        }))
    }

    pub async fn wake_outbox(
        &self,
        hand: &str,
        die_at: Option<BrainKill>,
    ) -> Result<Arc<dyn WakeOutbox>, String> {
        let medium = {
            let guard = self.wake_media.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(hand).cloned()
        };
        let medium = match medium {
            Some(m) => m,
            None => {
                let m = self.claimed().await?;
                self.wake_media
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(hand.to_string(), m.clone());
                m
            }
        };
        let opened = self
            .adapters
            .store
            .open(&medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(BrainKillStore {
            inner: opened,
            on_durable: Arc::new(|_| {}),
            kill: die_at,
            dead: AtomicBool::new(false),
        });
        let clock = Arc::clone(&self.clock);
        Ok(create_wake_outbox(WakeOutboxOptions {
            hand: hand.to_string(),
            store,
            epoch: 1,
            admission: create_admission_line(),
            clock: Arc::new(move || clock.load(Ordering::SeqCst)),
            limits: None,
            on_fault: None,
        }))
    }

    pub async fn scopes(
        &self,
        idle_ttl_ms: i64,
        die_at: Option<BrainKill>,
    ) -> Result<Arc<dyn ScopeLifecycle>, String> {
        let medium = {
            let guard = self.scope_medium.lock().unwrap_or_else(|p| p.into_inner());
            guard.clone()
        };
        let medium = match medium {
            Some(m) => m,
            None => {
                let m = self.claimed().await?;
                *self.scope_medium.lock().unwrap_or_else(|p| p.into_inner()) = Some(m.clone());
                m
            }
        };
        let opened = self
            .adapters
            .store
            .open(&medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(BrainKillStore {
            inner: opened,
            on_durable: Arc::new(|_| {}),
            kill: die_at,
            dead: AtomicBool::new(false),
        });
        let clock = Arc::clone(&self.clock);
        Ok(create_scope_lifecycle(ScopeLifecycleOptions {
            store,
            epoch: 1,
            admission: create_admission_line(),
            clock: Arc::new(move || clock.load(Ordering::SeqCst)),
            idle_ttl_ms,
        }))
    }

    pub async fn wake_log(&self, hand: &str) -> Result<Vec<StoreValue>, String> {
        let medium = {
            let guard = self.wake_media.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(hand).cloned()
        };
        let Some(medium) = medium else {
            return Ok(Vec::new());
        };
        let store = self
            .adapters
            .store
            .open(&medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let records = store
            .scan_records(RecordScan {
                kind: Some("execution.wake-expired".to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(records.into_iter().map(|r| r.data).collect())
    }

    pub async fn wake_inbox(
        &self,
        owner: OwnerLookup,
        die_at: Option<BrainKill>,
    ) -> Result<Arc<dyn WakeInbox>, String> {
        let opened = self
            .adapters
            .store
            .open(&self.brain_medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(BrainKillStore {
            inner: opened,
            on_durable: self.on_durable_callback(),
            kill: die_at,
            dead: AtomicBool::new(false),
        });
        let clock = Arc::clone(&self.clock);
        Ok(create_wake_inbox(WakeInboxOptions {
            issuer: "brain".to_string(),
            store,
            epoch: 1,
            admission: create_admission_line(),
            clock: Arc::new(move || clock.load(Ordering::SeqCst)),
            owner,
            on_undeliverable: None,
            max_attempts: None,
        }))
    }

    pub fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    pub async fn restart_executor(&self, id: &str) -> Result<(), String> {
        let medium = self.kill_executor(id)?;
        self.start_executor(id, medium).await
    }

    pub async fn lose_executor(&self, id: &str) -> Result<(), String> {
        self.kill_executor(id)?;
        let fresh = self.claimed().await?;
        self.start_executor(id, fresh).await
    }

    pub async fn ledger_state(
        &self,
        id: &str,
        execution_id: &str,
    ) -> Result<Option<String>, String> {
        let medium = {
            let guard = self.executors.lock().unwrap_or_else(|p| p.into_inner());
            guard.get(id).map(|e| e.medium.clone()).unwrap_or_default()
        };
        let store = self
            .adapters
            .store
            .open(&medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let key = format!(
            "execution.ledger:{}",
            serde_json::to_string(&["brain", execution_id]).unwrap_or_default()
        );
        let row = store.get_document(&key).await.map_err(|e| e.to_string())?;
        Ok(row
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("progress"))
            .and_then(|p| p.as_object())
            .and_then(|p| p.get("state"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }
}

async fn claimed_medium(adapters: &BatchCaseAdapters) -> Result<String, String> {
    let medium = adapters
        .store
        .create_medium()
        .await
        .map_err(|e| e.to_string())?;
    adapters
        .store
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?
        .claim(0, 1)
        .await
        .map_err(|e| e.to_string())?;
    Ok(medium)
}

pub async fn batch_harness(adapters: BatchCaseAdapters) -> Result<BatchHarness, String> {
    let brain_medium = claimed_medium(&adapters).await?;
    let harness = BatchHarness {
        adapters,
        trace: Arc::new(Mutex::new(Vec::new())),
        gates: Arc::new(Mutex::new(HashMap::new())),
        clock: Arc::new(AtomicI64::new(NOW)),
        executors: Arc::new(Mutex::new(HashMap::new())),
        brain_medium,
        wake_media: Mutex::new(HashMap::new()),
        scope_medium: Mutex::new(None),
        brain_writes: Arc::new(Mutex::new(Vec::new())),
        write_waiters: Arc::new(Mutex::new(HashMap::new())),
    };
    for id in ["e1", "e2"] {
        let medium = harness.claimed().await?;
        harness.start_executor(id, medium).await?;
    }
    Ok(harness)
}

pub async fn expect_rejects_msg<T: std::fmt::Debug>(
    operation: impl std::future::Future<Output = Result<T, String>>,
    expected_substr: &str,
    what: &str,
) -> Result<(), String> {
    match operation.await {
        Err(err) if err.contains(expected_substr) => Ok(()),
        Err(err) => Err(format!(
            "{what}: expected error containing {:?}, got {err}",
            expected_substr
        )),
        Ok(val) => Err(format!(
            "{what}: expected error containing {:?}, but it resolved with {val:?}",
            expected_substr
        )),
    }
}
