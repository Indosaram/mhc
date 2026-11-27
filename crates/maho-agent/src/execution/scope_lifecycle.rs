//! Port of senpi `packages/agent/src/execution/scope-lifecycle.ts`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;

use crate::durable::store::{DurableStore, StoreValue, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::canonical_json::canonical_json;

/// What an endpoint owns for one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeBinding {
    pub issuer: String,
    pub workspace: String,
    pub scope: String,
    pub generation: u64,
    pub allowed_roots: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeState {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeLifecycleEvent {
    pub seq: usize,
    pub kind: String,
    pub effect: StoreValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRecord {
    pub binding: ScopeBinding,
    pub state: ScopeState,
    pub last_active_at: i64,
    pub resources: BTreeMap<String, String>,
    pub events: Vec<ScopeLifecycleEvent>,
    pub outcomes: BTreeMap<String, StoreValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRefusal {
    UnknownScope,
    NotIssuer,
    Closed,
    LiveResources,
    ConflictingScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeUnavailableReason {
    UnknownScope,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("scope {scope} is not open: {reason:?}")]
pub struct ScopeUnavailableError {
    pub scope: String,
    pub reason: ScopeUnavailableReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenScopeResult {
    Opened,
    Refused(ScopeRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleCommitResult {
    Committed { seq: usize },
    Refused(ScopeRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseScopeResult {
    Closed,
    Refused(ScopeRefusal),
}

pub type LifecycleHandler =
    Box<dyn FnOnce() -> BoxFuture<'static, Result<StoreValue, String>> + Send>;
pub type TerminateResourceFn =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<StoreValue, String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct CloseScopeOptions {
    pub force: bool,
    pub terminate: Option<TerminateResourceFn>,
}

pub struct ScopeLifecycleOptions {
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub idle_ttl_ms: i64,
}

#[derive(Debug, Clone)]
pub enum ScopeError {
    Unavailable(ScopeUnavailableError),
    Other(String),
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => e.fmt(f),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for ScopeError {}

pub trait ScopeLifecycle: Send + Sync {
    fn open(&self, binding: ScopeBinding) -> BoxFuture<'_, Result<OpenScopeResult, String>>;
    fn lifecycle(
        &self,
        actor: &str,
        scope: &str,
        kind: &str,
        handler: LifecycleHandler,
    ) -> BoxFuture<'_, Result<LifecycleCommitResult, String>>;
    fn call_scope(
        &self,
        scope: &str,
        run: Box<dyn FnOnce() -> BoxFuture<'static, Result<StoreValue, ScopeError>> + Send>,
    ) -> BoxFuture<'_, Result<StoreValue, ScopeError>>;
    fn hold(
        &self,
        scope: &str,
        resource: &str,
        kind: &str,
    ) -> BoxFuture<'_, Result<(), ScopeError>>;
    fn release(&self, scope: &str, resource: &str) -> BoxFuture<'_, Result<(), String>>;
    fn close(
        &self,
        actor: &str,
        scope: &str,
        options: CloseScopeOptions,
    ) -> BoxFuture<'_, Result<CloseScopeResult, String>>;
    fn sweep_idle(&self) -> BoxFuture<'_, Result<Vec<String>, String>>;
    fn read(&self, scope: &str) -> BoxFuture<'_, Result<Option<ScopeRecord>, String>>;
}

const PREFIX: &str = "execution.scope:";

fn decode_scope(value: &StoreValue) -> Result<ScopeRecord, String> {
    let Some(obj) = value.as_object() else {
        return Err("corrupt scope record".to_owned());
    };
    if obj.get("binding").and_then(StoreValue::as_object).is_some()
        && matches!(
            obj.get("state").and_then(StoreValue::as_str),
            Some("open" | "closed")
        )
        && obj
            .get("lastActiveAt")
            .and_then(StoreValue::as_i64)
            .is_some()
        && obj
            .get("resources")
            .and_then(StoreValue::as_object)
            .is_some()
        && obj.get("events").and_then(StoreValue::as_array).is_some()
        && obj
            .get("outcomes")
            .and_then(StoreValue::as_object)
            .is_some()
    {
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())
    } else {
        Err("corrupt scope record".to_owned())
    }
}

struct CompletionFlag {
    done: AtomicBool,
    notify: Notify,
}

impl CompletionFlag {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.done.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct FlagGuard {
    scope: String,
    flag: Arc<CompletionFlag>,
    tails: Arc<Mutex<HashMap<String, Arc<CompletionFlag>>>>,
}

impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.flag.finish();
        let mut map = self.tails.lock().unwrap_or_else(|p| p.into_inner());
        if map
            .get(&self.scope)
            .is_some_and(|f| Arc::ptr_eq(f, &self.flag))
        {
            map.remove(&self.scope);
        }
    }
}

struct StoreScopeLifecycle {
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    idle_ttl_ms: i64,
    tails: Arc<Mutex<HashMap<String, Arc<CompletionFlag>>>>,
}

impl StoreScopeLifecycle {
    fn key_of(scope: &str) -> String {
        format!(
            "{PREFIX}{}",
            serde_json::to_string(&[scope]).unwrap_or_default()
        )
    }

    async fn read_inner(&self, scope: &str) -> Result<Option<ScopeRecord>, String> {
        let val = self
            .store
            .get_document(&Self::key_of(scope))
            .await
            .map_err(|e| e.to_string())?;
        match val {
            None => Ok(None),
            Some(v) => decode_scope(&v).map(Some),
        }
    }

    async fn write_inner(
        &self,
        record: &ScopeRecord,
        step: &str,
        execution_id: Option<&str>,
    ) -> Result<(), String> {
        let doc = serde_json::to_value(record).map_err(|e| e.to_string())?;
        self.store
            .commit(
                self.epoch,
                vec![
                    StoreWrite::Append {
                        id: self.store.mint_id(),
                        kind: "execution.scope".to_owned(),
                        data: json!({
                            "scope": record.binding.scope,
                            "step": step,
                            "executionId": execution_id,
                        }),
                    },
                    StoreWrite::Put {
                        key: Self::key_of(&record.binding.scope),
                        data: doc,
                    },
                ],
                Box::new(|_| {}),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn after<T, F, Fut>(&self, scope: &str, step: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let next_flag = Arc::new(CompletionFlag::new());
        let prev = {
            let mut map = self.tails.lock().unwrap_or_else(|p| p.into_inner());
            map.insert(scope.to_owned(), Arc::clone(&next_flag))
        };
        let _guard = FlagGuard {
            scope: scope.to_owned(),
            flag: next_flag,
            tails: Arc::clone(&self.tails),
        };
        if let Some(prev_flag) = prev {
            prev_flag.wait().await;
        }
        step().await
    }

    async fn wait_tail(&self, scope: &str) {
        let current = self
            .tails
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(scope)
            .cloned();
        if let Some(flag) = current {
            flag.wait().await;
        }
    }

    async fn touch(&self, scope: &str) -> Result<Option<ScopeState>, String> {
        let scope_owned = scope.to_owned();
        self.admission
            .admit(|| async move {
                let Some(mut rec) = self.read_inner(&scope_owned).await? else {
                    return Ok(None);
                };
                if rec.state != ScopeState::Open {
                    return Ok(Some(rec.state));
                }
                rec.last_active_at = (self.clock)();
                self.write_inner(&rec, "active", None).await?;
                Ok(Some(ScopeState::Open))
            })
            .await
    }

    async fn finish_close(
        &self,
        scope: &str,
        force: bool,
        terminate: Option<&TerminateResourceFn>,
    ) -> Result<CloseScopeResult, String> {
        enum CloseStep {
            Closed,
            Refused(ScopeRefusal),
            Terminate(Vec<String>),
        }
        loop {
            let scope_owned = scope.to_owned();
            let step = self
                .admission
                .admit(|| async move {
                    let Some(mut latest) = self.read_inner(&scope_owned).await? else {
                        return Ok::<CloseStep, String>(CloseStep::Refused(
                            ScopeRefusal::UnknownScope,
                        ));
                    };
                    if latest.state == ScopeState::Closed {
                        return Ok(CloseStep::Closed);
                    }
                    let pending: Vec<String> = latest
                        .resources
                        .keys()
                        .filter(|res| !latest.outcomes.contains_key(*res))
                        .cloned()
                        .collect();
                    if !pending.is_empty() && !force {
                        return Ok(CloseStep::Refused(ScopeRefusal::LiveResources));
                    }
                    if !pending.is_empty() {
                        return Ok(CloseStep::Terminate(pending));
                    }
                    latest.state = ScopeState::Closed;
                    latest.resources.clear();
                    self.write_inner(&latest, "close", None).await?;
                    Ok(CloseStep::Closed)
                })
                .await?;
            match step {
                CloseStep::Closed => return Ok(CloseScopeResult::Closed),
                CloseStep::Refused(r) => return Ok(CloseScopeResult::Refused(r)),
                CloseStep::Terminate(pending) => {
                    let Some(term_fn) = terminate else {
                        return Err(format!("forced close of {scope} needs a terminate port"));
                    };
                    for resource in pending {
                        let outcome = term_fn(resource.clone()).await?;
                        let scope_owned = scope.to_owned();
                        self.admission
                            .admit(|| async move {
                                let Some(mut latest) = self.read_inner(&scope_owned).await? else {
                                    return Ok(());
                                };
                                if latest.state == ScopeState::Closed
                                    || latest.outcomes.contains_key(&resource)
                                {
                                    return Ok(());
                                }
                                latest.outcomes.insert(resource.clone(), outcome);
                                self.write_inner(&latest, "outcome", Some(&resource)).await
                            })
                            .await?;
                    }
                }
            }
        }
    }
}

impl ScopeLifecycle for StoreScopeLifecycle {
    fn read(&self, scope: &str) -> BoxFuture<'_, Result<Option<ScopeRecord>, String>> {
        let scope = scope.to_owned();
        Box::pin(async move { self.read_inner(&scope).await })
    }

    fn open(&self, binding: ScopeBinding) -> BoxFuture<'_, Result<OpenScopeResult, String>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    if let Some(rec) = self.read_inner(&binding.scope).await? {
                        let rec_json =
                            serde_json::to_value(&rec.binding).map_err(|e| e.to_string())?;
                        let in_json = serde_json::to_value(&binding).map_err(|e| e.to_string())?;
                        return Ok(
                            if canonical_json(&rec_json) == canonical_json(&in_json)
                                && rec.state == ScopeState::Open
                            {
                                OpenScopeResult::Opened
                            } else {
                                OpenScopeResult::Refused(ScopeRefusal::ConflictingScope)
                            },
                        );
                    }
                    let record = ScopeRecord {
                        binding,
                        state: ScopeState::Open,
                        last_active_at: (self.clock)(),
                        resources: BTreeMap::new(),
                        events: Vec::new(),
                        outcomes: BTreeMap::new(),
                    };
                    self.write_inner(&record, "open", None).await?;
                    Ok(OpenScopeResult::Opened)
                })
                .await
        })
    }

    fn lifecycle(
        &self,
        actor: &str,
        scope: &str,
        kind: &str,
        handler: LifecycleHandler,
    ) -> BoxFuture<'_, Result<LifecycleCommitResult, String>> {
        let actor = actor.to_owned();
        let scope_key = scope.to_owned();
        let scope = scope.to_owned();
        let kind = kind.to_owned();
        Box::pin(async move {
            self.after(&scope_key, || async move {
                let Some(record) = self.read_inner(&scope).await? else {
                    return Ok(LifecycleCommitResult::Refused(ScopeRefusal::UnknownScope));
                };
                if record.binding.issuer != actor {
                    return Ok(LifecycleCommitResult::Refused(ScopeRefusal::NotIssuer));
                }
                if record.state == ScopeState::Closed {
                    return Ok(LifecycleCommitResult::Refused(ScopeRefusal::Closed));
                }
                let effect = handler().await?;
                self.admission
                    .admit(|| async move {
                        let Some(mut latest) = self.read_inner(&scope).await? else {
                            return Ok(LifecycleCommitResult::Refused(ScopeRefusal::Closed));
                        };
                        if latest.state == ScopeState::Closed {
                            return Ok(LifecycleCommitResult::Refused(ScopeRefusal::Closed));
                        }
                        let seq = latest.events.len() + 1;
                        latest.last_active_at = (self.clock)();
                        latest.events.push(ScopeLifecycleEvent {
                            seq,
                            kind: kind.clone(),
                            effect,
                        });
                        let exec_id = format!("{kind}#{seq}");
                        self.write_inner(&latest, "lifecycle", Some(&exec_id))
                            .await?;
                        Ok(LifecycleCommitResult::Committed { seq })
                    })
                    .await
            })
            .await
        })
    }

    fn call_scope(
        &self,
        scope: &str,
        run: Box<dyn FnOnce() -> BoxFuture<'static, Result<StoreValue, ScopeError>> + Send>,
    ) -> BoxFuture<'_, Result<StoreValue, ScopeError>> {
        let scope = scope.to_owned();
        Box::pin(async move {
            self.wait_tail(&scope).await;
            let state = self.touch(&scope).await.map_err(ScopeError::Other)?;
            match state {
                Some(ScopeState::Open) => run().await,
                None => Err(ScopeError::Unavailable(ScopeUnavailableError {
                    scope,
                    reason: ScopeUnavailableReason::UnknownScope,
                })),
                Some(ScopeState::Closed) => Err(ScopeError::Unavailable(ScopeUnavailableError {
                    scope,
                    reason: ScopeUnavailableReason::Closed,
                })),
            }
        })
    }

    fn hold(
        &self,
        scope: &str,
        resource: &str,
        kind: &str,
    ) -> BoxFuture<'_, Result<(), ScopeError>> {
        let scope = scope.to_owned();
        let resource = resource.to_owned();
        let kind = kind.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let record = self.read_inner(&scope).await.map_err(ScopeError::Other)?;
                    let Some(mut record) = record else {
                        return Err(ScopeError::Unavailable(ScopeUnavailableError {
                            scope,
                            reason: ScopeUnavailableReason::UnknownScope,
                        }));
                    };
                    if record.state != ScopeState::Open {
                        return Err(ScopeError::Unavailable(ScopeUnavailableError {
                            scope,
                            reason: ScopeUnavailableReason::Closed,
                        }));
                    }
                    record.last_active_at = (self.clock)();
                    record.resources.insert(resource.clone(), kind);
                    self.write_inner(&record, "hold", Some(&resource))
                        .await
                        .map_err(ScopeError::Other)
                })
                .await
        })
    }

    fn release(&self, scope: &str, resource: &str) -> BoxFuture<'_, Result<(), String>> {
        let scope = scope.to_owned();
        let resource = resource.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut record) = self.read_inner(&scope).await? else {
                        return Ok(());
                    };
                    if !record.resources.contains_key(&resource) {
                        return Ok(());
                    }
                    record.resources.remove(&resource);
                    record.last_active_at = (self.clock)();
                    self.write_inner(&record, "release", Some(&resource)).await
                })
                .await
        })
    }

    fn close(
        &self,
        actor: &str,
        scope: &str,
        options: CloseScopeOptions,
    ) -> BoxFuture<'_, Result<CloseScopeResult, String>> {
        let actor = actor.to_owned();
        let scope_key = scope.to_owned();
        let scope = scope.to_owned();
        Box::pin(async move {
            self.after(&scope_key, || async move {
                let Some(record) = self.read_inner(&scope).await? else {
                    return Ok(CloseScopeResult::Refused(ScopeRefusal::UnknownScope));
                };
                if record.binding.issuer != actor {
                    return Ok(CloseScopeResult::Refused(ScopeRefusal::NotIssuer));
                }
                self.finish_close(&scope, options.force, options.terminate.as_ref())
                    .await
            })
            .await
        })
    }

    fn sweep_idle(&self) -> BoxFuture<'_, Result<Vec<String>, String>> {
        Box::pin(async move {
            let mut closed = Vec::new();
            let docs = self
                .store
                .scan_documents(PREFIX)
                .await
                .map_err(|e| e.to_string())?;
            for doc in docs {
                let candidate = decode_scope(&doc.data)?;
                let scope = candidate.binding.scope;
                if candidate.state != ScopeState::Open
                    || !candidate.resources.is_empty()
                    || (self.clock)() - candidate.last_active_at <= self.idle_ttl_ms
                {
                    continue;
                }
                let scope_for_close = scope.clone();
                let scope_owned = scope_for_close.clone();
                let done = self
                    .after(&scope_for_close, || async move {
                        self.admission
                            .admit(|| async move {
                                let Some(mut latest) = self.read_inner(&scope_owned).await? else {
                                    return Ok::<bool, String>(false);
                                };
                                if latest.state != ScopeState::Open
                                    || !latest.resources.is_empty()
                                    || (self.clock)() - latest.last_active_at <= self.idle_ttl_ms
                                {
                                    return Ok::<bool, String>(false);
                                }
                                latest.state = ScopeState::Closed;
                                self.write_inner(&latest, "close", None).await?;
                                Ok(true)
                            })
                            .await
                    })
                    .await?;
                if done {
                    closed.push(scope);
                }
            }
            Ok(closed)
        })
    }
}

pub fn create_scope_lifecycle(options: ScopeLifecycleOptions) -> Arc<dyn ScopeLifecycle> {
    Arc::new(StoreScopeLifecycle {
        store: options.store,
        epoch: options.epoch,
        admission: options.admission,
        clock: options.clock,
        idle_ttl_ms: options.idle_ttl_ms,
        tails: Arc::new(Mutex::new(HashMap::new())),
    })
}
