//! Port of senpi `packages/agent/src/execution/wake-inbox.ts`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::durable::store::{DurableStore, StoreValue, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::wake_types::{WAKE_RETENTION_MS, WakeEvent, WakeInboxRecord, WakeReceipt, WakeRefusal};

const KIND: &str = "execution.wake-inbox";

#[derive(Debug, Clone)]
pub struct WakeDeliveryFailure {
    pub wake: WakeEvent,
    pub error: String,
    pub attempts: usize,
    pub poisoned: bool,
}

pub type OwnerLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type OnUndeliverable = Arc<dyn Fn(WakeDeliveryFailure) + Send + Sync>;
pub type WakeCheckFn = Arc<dyn Fn(&WakeEvent) -> Option<WakeRefusal> + Send + Sync>;
pub type WakeConsumeFn =
    Arc<dyn Fn(WakeEvent) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub struct WakeInboxOptions {
    pub issuer: String,
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub owner: OwnerLookup,
    pub on_undeliverable: Option<OnUndeliverable>,
    pub max_attempts: Option<usize>,
}

pub trait WakeInbox: Send + Sync {
    fn receive(
        &self,
        sender: &str,
        wake: WakeEvent,
        check: Option<WakeCheckFn>,
    ) -> BoxFuture<'_, Result<WakeReceipt, String>>;
    fn drain(&self, consume: WakeConsumeFn) -> BoxFuture<'_, Result<usize, String>>;
    fn close_scope(&self, scope: &str) -> BoxFuture<'_, Result<(), String>>;
    fn sweep(&self) -> BoxFuture<'_, Result<Vec<String>, String>>;
    fn read(&self) -> BoxFuture<'_, Result<WakeInboxRecord, String>>;
}

fn decode_inbox(value: Option<&StoreValue>) -> Result<WakeInboxRecord, String> {
    let Some(value) = value else {
        return Ok(WakeInboxRecord::default());
    };
    let Some(obj) = value.as_object() else {
        return Err("corrupt wake inbox record".to_owned());
    };
    if obj.get("seen").and_then(StoreValue::as_object).is_some()
        && obj.get("pending").and_then(StoreValue::as_array).is_some()
        && obj.get("closed").and_then(StoreValue::as_object).is_some()
    {
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())
    } else {
        Err("corrupt wake inbox record".to_owned())
    }
}

fn with_failures(
    mut record: WakeInboxRecord,
    failures: BTreeMap<String, usize>,
) -> WakeInboxRecord {
    record.failures = if failures.is_empty() {
        None
    } else {
        Some(failures)
    };
    record
}

struct StoreWakeInbox {
    key: String,
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    owner: OwnerLookup,
    on_undeliverable: Option<OnUndeliverable>,
    max_attempts: usize,
    drain_lock: tokio::sync::Mutex<()>,
}

impl StoreWakeInbox {
    async fn read_inner(&self) -> Result<WakeInboxRecord, String> {
        let value = self
            .store
            .get_document(&self.key)
            .await
            .map_err(|e| e.to_string())?;
        decode_inbox(value.as_ref())
    }

    async fn write_inner(
        &self,
        record: &WakeInboxRecord,
        step: &str,
        event_id: Option<&str>,
    ) -> Result<(), String> {
        let doc = serde_json::to_value(record).map_err(|e| e.to_string())?;
        self.store
            .commit(
                self.epoch,
                vec![
                    StoreWrite::Append {
                        id: self.store.mint_id(),
                        kind: KIND.to_owned(),
                        data: json!({ "step": step, "executionId": event_id }),
                    },
                    StoreWrite::Put {
                        key: self.key.clone(),
                        data: doc,
                    },
                ],
                Box::new(|_| {}),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn take(&self, wake: &WakeEvent, step: &str) -> Result<(), String> {
        let event_id = wake.event_id.clone();
        let step_owned = step.to_owned();
        self.admission
            .admit(|| async move {
                let mut record = self.read_inner().await?;
                if !record.pending.iter().any(|e| e.event_id == event_id) {
                    return Ok(());
                }
                record.pending.retain(|e| e.event_id != event_id);
                let mut failures = record.failures.take().unwrap_or_default();
                failures.remove(&event_id);
                let next = with_failures(record, failures);
                self.write_inner(&next, &step_owned, Some(&event_id)).await
            })
            .await
    }

    async fn failed(
        &self,
        wake: WakeEvent,
        error: String,
        report: &OnUndeliverable,
    ) -> Result<(), String> {
        let attempts = self
            .read_inner()
            .await?
            .failures
            .and_then(|f| f.get(&wake.event_id).copied())
            .unwrap_or(0)
            + 1;
        let poisoned = attempts >= self.max_attempts;
        if poisoned {
            self.take(&wake, "poison").await?;
        } else {
            let event_id = wake.event_id.clone();
            self.admission
                .admit(|| async move {
                    let mut record = self.read_inner().await?;
                    let mut failures = record.failures.take().unwrap_or_default();
                    failures.insert(event_id.clone(), attempts);
                    let next = with_failures(record, failures);
                    self.write_inner(&next, "failed", Some(&event_id)).await
                })
                .await?;
        }
        report(WakeDeliveryFailure {
            wake,
            error,
            attempts,
            poisoned,
        });
        Ok(())
    }

    async fn drain_once(&self, consume: &WakeConsumeFn) -> Result<usize, String> {
        let mut taken = 0usize;
        let pending = self.read_inner().await?.pending;
        for wake in pending {
            match consume(wake.clone()).await {
                Ok(()) => {
                    self.take(&wake, "take").await?;
                    taken += 1;
                }
                Err(error) => {
                    let Some(report) = &self.on_undeliverable else {
                        return Err(error);
                    };
                    self.failed(wake, error, report).await?;
                }
            }
        }
        Ok(taken)
    }
}

impl WakeInbox for StoreWakeInbox {
    fn read(&self) -> BoxFuture<'_, Result<WakeInboxRecord, String>> {
        Box::pin(async move { self.read_inner().await })
    }

    fn receive(
        &self,
        sender: &str,
        wake: WakeEvent,
        check: Option<WakeCheckFn>,
    ) -> BoxFuture<'_, Result<WakeReceipt, String>> {
        let sender = sender.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let mut record = self.read_inner().await?;
                    let seen = record.seen.get(&wake.scope).cloned().unwrap_or_default();
                    if seen.contains(&wake.event_id) {
                        return Ok(WakeReceipt::Ack {
                            event_id: wake.event_id,
                            duplicate: true,
                        });
                    }
                    if record.closed.contains_key(&wake.scope) {
                        return Ok(WakeReceipt::Refused {
                            event_id: wake.event_id,
                            reason: WakeRefusal::ScopeClosed,
                        });
                    }
                    let Some(bound) = (self.owner)(&wake.scope) else {
                        return Ok(WakeReceipt::Refused {
                            event_id: wake.event_id,
                            reason: WakeRefusal::UnknownScope,
                        });
                    };
                    if bound != sender {
                        return Ok(WakeReceipt::Refused {
                            event_id: wake.event_id,
                            reason: WakeRefusal::NotScopeHand,
                        });
                    }
                    if let Some(check_fn) = check
                        && let Some(reason) = check_fn(&wake)
                    {
                        return Ok(WakeReceipt::Refused {
                            event_id: wake.event_id,
                            reason,
                        });
                    }
                    let event_id = wake.event_id.clone();
                    let mut next_seen = seen;
                    next_seen.push(event_id.clone());
                    record.seen.insert(wake.scope.clone(), next_seen);
                    record.pending.push(wake);
                    self.write_inner(&record, "commit", Some(&event_id)).await?;
                    Ok(WakeReceipt::Ack {
                        event_id,
                        duplicate: false,
                    })
                })
                .await
        })
    }

    fn drain(&self, consume: WakeConsumeFn) -> BoxFuture<'_, Result<usize, String>> {
        Box::pin(async move {
            let _guard = self.drain_lock.lock().await;
            self.drain_once(&consume).await
        })
    }

    fn close_scope(&self, scope: &str) -> BoxFuture<'_, Result<(), String>> {
        let scope = scope.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let mut record = self.read_inner().await?;
                    if record.closed.contains_key(&scope) {
                        return Ok(());
                    }
                    record.closed.insert(scope, (self.clock)());
                    self.write_inner(&record, "close", None).await
                })
                .await
        })
    }

    fn sweep(&self) -> BoxFuture<'_, Result<Vec<String>, String>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let mut record = self.read_inner().await?;
                    let now = (self.clock)();
                    let gone: Vec<String> = record
                        .closed
                        .iter()
                        .filter(|(scope, closed_at)| {
                            now > *closed_at + WAKE_RETENTION_MS && record.seen.contains_key(*scope)
                        })
                        .map(|(scope, _)| scope.clone())
                        .collect();
                    if gone.is_empty() {
                        return Ok(Vec::new());
                    }
                    let drop: HashSet<&String> = gone.iter().collect();
                    record.seen.retain(|scope, _| !drop.contains(scope));
                    self.write_inner(&record, "sweep", None).await?;
                    Ok(gone)
                })
                .await
        })
    }
}

pub fn create_wake_inbox(options: WakeInboxOptions) -> Arc<dyn WakeInbox> {
    let key = format!(
        "{KIND}:{}",
        serde_json::to_string(&[&options.issuer]).unwrap_or_default()
    );
    Arc::new(StoreWakeInbox {
        key,
        store: options.store,
        epoch: options.epoch,
        admission: options.admission,
        clock: options.clock,
        owner: options.owner,
        on_undeliverable: options.on_undeliverable,
        max_attempts: options.max_attempts.unwrap_or(3),
        drain_lock: tokio::sync::Mutex::new(()),
    })
}
