//! Port of senpi `packages/agent/src/execution/testing/batch-executor.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use serde_json::json;
use tokio::sync::Notify;

use crate::durable::store::{DurableStore, StoreValue};
use crate::durable::store_admission::create_admission_line;
use crate::durable::testing::store_harness::StoreConformanceAdapter;
use crate::execution::coordinator_types::{
    BatchExecutor, ConsentStepReply, ExecutionRef, PrepareReply, PrepareRequest, SettleReply,
};
use crate::execution::ledger::{
    ExecutionLedger, StoreExecutionLedgerOptions, create_execution_ledger,
};
use crate::execution::ledger_types::{
    Actor, ExecutionAdmission, ExecutionCommand, ExecutionEntry, ExecutionEvent, ExecutionKey,
    ExecutionOutcome, ExecutionProgress, FrozenCall, LedgerReply,
};

/// Where a test can hold an execution: inside the `tool_call` chain, between release and dispatch, inside the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PausePoint {
    Hook,
    Dispatch,
    Body,
}

impl PausePoint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Dispatch => "dispatch",
            Self::Body => "body",
        }
    }
}

pub type PauseFn = Arc<dyn Fn(&str, PausePoint) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct ExecutorOpenOptions {
    pub id: String,
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub trace: Arc<Mutex<Vec<String>>>,
    pub pause: PauseFn,
    pub killed: AbortSignal,
}

pub trait BatchExecutorConformanceAdapter: Send + Sync {
    fn open(
        &self,
        options: ExecutorOpenOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn BatchExecutor>, String>>;
}

#[derive(Clone)]
pub struct BatchCaseAdapters {
    pub store: Arc<dyn StoreConformanceAdapter>,
    pub executor: Arc<dyn BatchExecutorConformanceAdapter>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("executor {0} was killed")]
pub struct ExecutorKilledError(pub String);

fn frozen_digest_of(name: &str, request_digest: &str) -> String {
    if name == "ask" {
        request_digest.to_string()
    } else {
        format!("{request_digest}:frozen")
    }
}

fn needs_consent(name: &str) -> bool {
    name == "gated" || name == "ask"
}

fn batch_tool_decision(name: &str) -> Option<&'static str> {
    match name {
        "deny" => Some("blocked"),
        "stop" => Some("batch_abort"),
        _ => None,
    }
}

fn stopped_reply(kind: &str, name: &str) -> Option<PrepareReply> {
    match kind {
        "blocked" => Some(PrepareReply::Blocked {
            result: json!(format!("blocked: {name}")),
        }),
        "batch_abort" => Some(PrepareReply::BatchAbort),
        _ => None,
    }
}

fn from_stopped(value: &StoreValue) -> Option<PrepareReply> {
    let obj = value.as_object()?;
    let kind = obj.get("kind")?.as_str()?;
    let name = obj.get("name")?.as_str()?;
    stopped_reply(kind, name)
}

fn expect_entry(reply: LedgerReply, what: &str) -> Result<ExecutionEntry, String> {
    match reply {
        LedgerReply::Applied { entry } | LedgerReply::Replayed { entry } => Ok(entry),
        other => Err(format!(
            "{what}: unexpected ledger reply {}",
            serde_json::to_string(&other).unwrap_or_default()
        )),
    }
}

#[derive(Clone)]
struct SharedInFlight<R> {
    done: Arc<Mutex<Option<Result<R, String>>>>,
    notify: Arc<Notify>,
}

struct ReferenceBatchExecutor {
    id: String,
    ledger: Arc<dyn ExecutionLedger>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    trace: Arc<Mutex<Vec<String>>>,
    pause: PauseFn,
    killed: AbortSignal,
    prepare_inflight: Mutex<HashMap<String, SharedInFlight<PrepareReply>>>,
    release_inflight: Mutex<HashMap<String, SharedInFlight<SettleReply>>>,
    abort_inflight: Mutex<HashMap<String, SharedInFlight<SettleReply>>>,
}

impl ReferenceBatchExecutor {
    fn alive(&self) -> Result<(), String> {
        if self.killed.aborted() {
            Err(ExecutorKilledError(self.id.clone()).to_string())
        } else {
            Ok(())
        }
    }

    async fn hold(&self, execution_id: &str, point: PausePoint) -> Result<(), String> {
        let fut = (self.pause)(execution_id, point);
        tokio::select! {
            _ = fut => {}
            _ = self.killed.cancelled() => {
                return Err(ExecutorKilledError(self.id.clone()).to_string());
            }
        }
        self.alive()
    }

    fn key_of(exec_ref: &ExecutionRef) -> ExecutionKey {
        ExecutionKey {
            issuer: exec_ref.issuer.clone(),
            command_id: exec_ref.execution_id.clone(),
        }
    }

    async fn apply(
        &self,
        exec_ref: &ExecutionRef,
        event: ExecutionEvent,
        actor: &str,
    ) -> Result<LedgerReply, String> {
        self.ledger
            .apply(
                ExecutionCommand {
                    key: Self::key_of(exec_ref),
                    generation: exec_ref.generation,
                    actor: Actor {
                        id: actor.to_string(),
                        approved_workspace: None,
                    },
                    now: (self.now)(),
                    event,
                },
                None,
            )
            .await
            .map_err(|e| e.to_string())
    }

    fn stored_prepare(entry: &ExecutionEntry) -> Result<PrepareReply, String> {
        match &entry.progress {
            ExecutionProgress::Settled { outcome } => {
                if let ExecutionOutcome::Blocked { value, .. } = outcome
                    && let Some(stopped) = from_stopped(value)
                {
                    return Ok(stopped);
                }
                Ok(PrepareReply::Settled {
                    outcome: outcome.clone(),
                })
            }
            ExecutionProgress::PreparedAwaitingConsent { call } => {
                Ok(PrepareReply::PreparedAwaitingConsent {
                    frozen_digest: call.digest.clone(),
                })
            }
            ExecutionProgress::Prepared { call }
            | ExecutionProgress::Released { call }
            | ExecutionProgress::Dispatched { call }
            | ExecutionProgress::Reconciling { call, .. } => Ok(PrepareReply::Prepared {
                frozen_digest: call.digest.clone(),
            }),
            other => Err(format!(
                "no stored prepare reply in state {}",
                other.state()
            )),
        }
    }

    async fn settled_reply(&self, exec_ref: &ExecutionRef) -> Result<SettleReply, String> {
        let Some(entry) = self
            .ledger
            .read(Self::key_of(exec_ref))
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(SettleReply::UnknownExecution);
        };
        match entry.progress {
            ExecutionProgress::Settled { outcome } => Ok(SettleReply::Settled { outcome }),
            other => Err(format!(
                "execution {} is {}",
                exec_ref.execution_id,
                other.state()
            )),
        }
    }

    fn push_trace(&self, item: String) {
        let mut guard = self.trace.lock().unwrap_or_else(|p| p.into_inner());
        guard.push(item);
    }

    async fn prepare_inner(&self, request: PrepareRequest) -> Result<PrepareReply, String> {
        let exec_ref = ExecutionRef {
            issuer: request.issuer.clone(),
            execution_id: request.execution_id.clone(),
            generation: request.generation,
        };
        let admission = ExecutionAdmission {
            issuer: request.issuer.clone(),
            command_id: request.execution_id.clone(),
            executor: self.id.clone(),
            workspace: request.workspace.clone(),
            scope: request.scope.clone(),
            generation: request.generation,
            request_digest: request.request_digest.clone(),
            admitted_at: request.admitted_at,
            lease_until: request.lease_until,
            reconcilable: false,
        };
        let entry = expect_entry(
            self.ledger
                .admit(admission, None)
                .await
                .map_err(|e| e.to_string())?,
            "admit",
        )?;
        if entry.progress.state() != "admitted" {
            return Self::stored_prepare(&entry);
        }
        expect_entry(
            self.apply(&exec_ref, ExecutionEvent::Prepare, &self.id)
                .await?,
            "prepare",
        )?;
        expect_entry(
            self.apply(&exec_ref, ExecutionEvent::Effect, &self.id)
                .await?,
            "effect",
        )?;
        self.push_trace(format!("shim:{}", request.execution_id));
        self.push_trace(format!("hook:{}", request.execution_id));
        self.hold(&request.execution_id, PausePoint::Hook).await?;
        if let Some(decision) = batch_tool_decision(&request.call.name) {
            let block = ExecutionEvent::Block {
                value: json!({
                    "kind": decision,
                    "name": request.call.name,
                }),
            };
            let entry = expect_entry(self.apply(&exec_ref, block, &self.id).await?, "block")?;
            return Self::stored_prepare(&entry);
        }
        let digest = frozen_digest_of(&request.call.name, &request.request_digest);
        let frozen = FrozenCall {
            name: request.call.name.clone(),
            args: request.call.args.clone(),
            digest: digest.clone(),
        };
        let awaiting_consent = needs_consent(&request.call.name)
            && request.authorized_digest.as_deref() != Some(digest.as_str());
        let entry = expect_entry(
            self.apply(
                &exec_ref,
                ExecutionEvent::Freeze {
                    call: frozen,
                    awaiting_consent,
                },
                &self.id,
            )
            .await?,
            "freeze",
        )?;
        Self::stored_prepare(&entry)
    }

    async fn release_inner(&self, exec_ref: ExecutionRef) -> Result<SettleReply, String> {
        let Some(entry) = self
            .ledger
            .read(Self::key_of(&exec_ref))
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(SettleReply::UnknownExecution);
        };
        if entry.progress.state() == "settled" {
            return self.settled_reply(&exec_ref).await;
        }
        expect_entry(
            self.apply(&exec_ref, ExecutionEvent::Release, &exec_ref.issuer)
                .await?,
            "release",
        )?;
        self.hold(&exec_ref.execution_id, PausePoint::Dispatch)
            .await?;
        let dispatched = expect_entry(
            self.apply(&exec_ref, ExecutionEvent::Dispatch, &self.id)
                .await?,
            "dispatch",
        )?;
        if let ExecutionProgress::Dispatched { call } = dispatched.progress {
            self.push_trace(format!("body:{}", exec_ref.execution_id));
            self.hold(&exec_ref.execution_id, PausePoint::Body).await?;
            let value = json!({
                "executionId": exec_ref.execution_id,
                "name": call.name,
            });
            self.push_trace(format!("done:{}", exec_ref.execution_id));
            expect_entry(
                self.apply(&exec_ref, ExecutionEvent::Result { value }, &self.id)
                    .await?,
                "result",
            )?;
        }
        self.settled_reply(&exec_ref).await
    }

    async fn abort_inner(&self, exec_ref: ExecutionRef) -> Result<SettleReply, String> {
        if self
            .ledger
            .read(Self::key_of(&exec_ref))
            .await
            .map_err(|e| e.to_string())?
            .is_none()
        {
            return Ok(SettleReply::UnknownExecution);
        }
        let reply = self
            .apply(&exec_ref, ExecutionEvent::Abort, &exec_ref.issuer)
            .await?;
        if let LedgerReply::Refused { reason } = &reply
            && reason.as_str() != "terminal"
        {
            return Err(format!("abort refused: {}", reason.as_str()));
        }
        self.settled_reply(&exec_ref).await
    }

    async fn consent_inner(
        &self,
        exec_ref: ExecutionRef,
        frozen_digest: &str,
    ) -> Result<ConsentStepReply, String> {
        let Some(entry) = self
            .ledger
            .read(Self::key_of(&exec_ref))
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(ConsentStepReply::UnknownExecution);
        };
        if let Some(call) = entry.progress.call()
            && call.digest != frozen_digest
        {
            return Err(format!(
                "consent for {} names another frozen call",
                exec_ref.execution_id
            ));
        }
        let reply = self
            .apply(&exec_ref, ExecutionEvent::Consent, &exec_ref.issuer)
            .await?;
        if let LedgerReply::Refused { reason } = &reply
            && reason.as_str() != "terminal"
            && reason.as_str() != "invalid_transition"
        {
            return Err(format!("consent refused: {}", reason.as_str()));
        }
        let after = self
            .ledger
            .read(Self::key_of(&exec_ref))
            .await
            .map_err(|e| e.to_string())?;
        if after
            .as_ref()
            .is_some_and(|e| e.progress.state() == "prepared")
        {
            Ok(ConsentStepReply::Prepared)
        } else {
            Ok(self.settled_reply(&exec_ref).await?.into())
        }
    }

    async fn dedup_call<R, F, Fut>(
        &self,
        inflight: &Mutex<HashMap<String, SharedInFlight<R>>>,
        key: String,
        op: F,
    ) -> Result<R, String>
    where
        R: Clone,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<R, String>>,
    {
        self.alive()?;
        enum Role<R> {
            Wait(SharedInFlight<R>),
            Run(SharedInFlight<R>),
        }
        let role = {
            let mut guard = inflight.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(existing) = guard.get(&key) {
                Role::Wait(existing.clone())
            } else {
                let entry = SharedInFlight {
                    done: Arc::new(Mutex::new(None)),
                    notify: Arc::new(Notify::new()),
                };
                guard.insert(key.clone(), entry.clone());
                Role::Run(entry)
            }
        };
        match role {
            Role::Wait(shared) => loop {
                let notified = shared.notify.notified();
                if let Some(res) = shared
                    .done
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
                {
                    return res;
                }
                notified.await;
            },
            Role::Run(shared) => {
                let result = op().await;
                {
                    let mut guard = shared.done.lock().unwrap_or_else(|p| p.into_inner());
                    *guard = Some(result.clone());
                }
                {
                    let mut map = inflight.lock().unwrap_or_else(|p| p.into_inner());
                    map.remove(&key);
                }
                shared.notify.notify_waiters();
                result
            }
        }
    }
}

impl BatchExecutor for ReferenceBatchExecutor {
    fn prepare(&self, request: PrepareRequest) -> BoxFuture<'_, Result<PrepareReply, String>> {
        Box::pin(async move {
            let key = serde_json::to_string(&[&request.issuer, &request.execution_id])
                .unwrap_or_default();
            self.dedup_call(&self.prepare_inflight, key, || self.prepare_inner(request))
                .await
        })
    }

    fn release(&self, exec_ref: ExecutionRef) -> BoxFuture<'_, Result<SettleReply, String>> {
        Box::pin(async move {
            let key = serde_json::to_string(&[&exec_ref.issuer, &exec_ref.execution_id])
                .unwrap_or_default();
            self.dedup_call(&self.release_inflight, key, || self.release_inner(exec_ref))
                .await
        })
    }

    fn abort(&self, exec_ref: ExecutionRef) -> BoxFuture<'_, Result<SettleReply, String>> {
        Box::pin(async move {
            let key = serde_json::to_string(&[&exec_ref.issuer, &exec_ref.execution_id])
                .unwrap_or_default();
            self.dedup_call(&self.abort_inflight, key, || self.abort_inner(exec_ref))
                .await
        })
    }

    fn consent(
        &self,
        exec_ref: ExecutionRef,
        frozen_digest: &str,
    ) -> Option<BoxFuture<'_, Result<ConsentStepReply, String>>> {
        let digest = frozen_digest.to_string();
        Some(Box::pin(async move {
            self.alive()?;
            self.consent_inner(exec_ref, &digest).await
        }))
    }
}

struct ReferenceAdapter;

impl BatchExecutorConformanceAdapter for ReferenceAdapter {
    fn open(
        &self,
        options: ExecutorOpenOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn BatchExecutor>, String>> {
        Box::pin(async move {
            let ledger = create_execution_ledger(StoreExecutionLedgerOptions {
                store: options.store,
                epoch: options.epoch,
                admission: create_admission_line(),
            });
            ledger
                .recover((options.now)())
                .await
                .map_err(|e| e.to_string())?;
            Ok(Arc::new(ReferenceBatchExecutor {
                id: options.id,
                ledger,
                now: options.now,
                trace: options.trace,
                pause: options.pause,
                killed: options.killed,
                prepare_inflight: Mutex::new(HashMap::new()),
                release_inflight: Mutex::new(HashMap::new()),
                abort_inflight: Mutex::new(HashMap::new()),
            }) as Arc<dyn BatchExecutor>)
        })
    }
}

pub fn reference_batch_executor() -> Arc<dyn BatchExecutorConformanceAdapter> {
    Arc::new(ReferenceAdapter)
}
