//! Port of senpi `packages/agent/src/execution/wake-outbox.ts`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::durable::store::{DurableStore, StoreValue, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::wake_outbox_record::{DecodedOutbox, add_drop, decode_outbox, malformed_scope};
use super::wake_types::{
    OutboxRefusalEntry, WAKE_RETENTION_MS, WakeDropReason, WakeEvent, WakeLimits, WakeOffer,
    WakeOutboxRecord, WakeReceipt, WakeRefusal,
};

const KIND: &str = "execution.wake-outbox";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("the hand's wake outbox is full; wake {wake_id} of scope {scope} was dropped (overflow)", wake_id = .wake.event_id, scope = .wake.scope)]
pub struct WakeOverflowError {
    pub wake: WakeEvent,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("a malformed wake was dropped from the outbox: {entry}")]
pub struct MalformedWakeError {
    pub entry: StoreValue,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum WakeOutboxError {
    #[error(transparent)]
    Overflow(#[from] WakeOverflowError),
    #[error("{0}")]
    Other(String),
}

pub type ReofferSendFn =
    Arc<dyn Fn(WakeEvent) -> BoxFuture<'static, Result<WakeReceipt, String>> + Send + Sync>;

pub struct WakeOutboxOptions {
    pub hand: String,
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub limits: Option<WakeLimits>,
    pub on_fault: Option<Arc<dyn Fn(MalformedWakeError) + Send + Sync>>,
}

pub trait WakeOutbox: Send + Sync {
    fn offer(&self, wake: WakeOffer) -> BoxFuture<'_, Result<WakeEvent, WakeOutboxError>>;
    fn reoffer(&self, send: ReofferSendFn) -> BoxFuture<'_, Result<usize, String>>;
    fn due(&self) -> BoxFuture<'_, Result<Vec<WakeEvent>, String>>;
    fn receipt(&self, receipt: WakeReceipt) -> BoxFuture<'_, Result<(), String>>;
    fn close_scope(&self, scope: &str) -> BoxFuture<'_, Result<(), String>>;
    fn sweep(&self) -> BoxFuture<'_, Result<Vec<String>, String>>;
    fn pending(&self) -> BoxFuture<'_, Result<Vec<WakeEvent>, String>>;
}

struct StoreWakeOutbox {
    hand: String,
    key: String,
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    limits: Option<WakeLimits>,
    on_fault: Option<Arc<dyn Fn(MalformedWakeError) + Send + Sync>>,
    fresh: Mutex<HashSet<String>>,
}

impl StoreWakeOutbox {
    fn mint(&self) -> String {
        format!("{}:{}", self.hand, self.store.mint_id())
    }

    async fn decoded(&self) -> Result<DecodedOutbox, String> {
        let val = self
            .store
            .get_document(&self.key)
            .await
            .map_err(|e| e.to_string())?;
        decode_outbox(val.as_ref())
    }

    async fn read_record(&self) -> Result<WakeOutboxRecord, String> {
        Ok(self.decoded().await?.record)
    }

    async fn write_record(
        &self,
        record: &WakeOutboxRecord,
        step: &str,
        event_id: Option<&str>,
        extra: Vec<StoreValue>,
    ) -> Result<(), String> {
        let mut writes = vec![StoreWrite::Append {
            id: self.store.mint_id(),
            kind: KIND.to_owned(),
            data: json!({ "step": step, "eventId": event_id }),
        }];
        for data in extra {
            writes.push(StoreWrite::Append {
                id: self.store.mint_id(),
                kind: "execution.wake-expired".to_owned(),
                data,
            });
        }
        writes.push(StoreWrite::Put {
            key: self.key.clone(),
            data: serde_json::to_value(record).map_err(|e| e.to_string())?,
        });
        self.store
            .commit(self.epoch, writes, Box::new(|_| {}))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn with_drops(
        &self,
        mut record: WakeOutboxRecord,
        scope: &str,
        reason: WakeDropReason,
        count: u64,
    ) -> WakeOutboxRecord {
        let mut fresh = self.fresh.lock().unwrap_or_else(|p| p.into_inner());
        record.drops = add_drop(&record.drops, scope, reason, count, &mut fresh, || {
            self.mint()
        });
        record
    }

    fn evict(
        &self,
        mut record: WakeOutboxRecord,
        wake: &WakeEvent,
        reason: WakeDropReason,
    ) -> WakeOutboxRecord {
        record.refused.remove(&wake.event_id);
        record.pending.retain(|e| e.event_id != wake.event_id);
        self.with_drops(record, &wake.scope, reason, 1)
    }

    fn bounded(&self, mut record: WakeOutboxRecord, scope: &str) -> Option<WakeOutboxRecord> {
        let Some(limits) = self.limits else {
            return Some(record);
        };
        loop {
            let scope_count = record.pending.iter().filter(|w| w.scope == scope).count();
            let over_scope = scope_count > limits.per_scope;
            let over_total = record.pending.len() > limits.total;
            if !over_scope && !over_total {
                return Some(record);
            }
            let evictable = record
                .pending
                .iter()
                .find(|w| {
                    record.refused.contains_key(&w.event_id) && (!over_scope || w.scope == scope)
                })
                .cloned()?;
            record = self.evict(record, &evictable, WakeDropReason::Refused);
        }
    }

    async fn apply_receipt(&self, answer: WakeReceipt) -> Result<(), String> {
        self.admission
            .admit(|| async move {
                let mut record = self.read_record().await?;
                let target_id = answer.event_id();
                let known = record
                    .pending
                    .iter()
                    .chain(record.drops.iter())
                    .any(|w| w.event_id == target_id);
                if !known {
                    return Ok(());
                }
                match answer {
                    WakeReceipt::Refused { event_id, reason } => {
                        if reason == WakeRefusal::UnknownScope
                            || record.refused.contains_key(&event_id)
                        {
                            return Ok(());
                        }
                        record.refused.insert(
                            event_id.clone(),
                            OutboxRefusalEntry {
                                reason,
                                at: (self.clock)(),
                            },
                        );
                        self.write_record(&record, "refused", Some(&event_id), vec![])
                            .await
                    }
                    WakeReceipt::Ack { event_id, .. } => {
                        record.refused.remove(&event_id);
                        record.pending.retain(|w| w.event_id != event_id);
                        record.drops.retain(|w| w.event_id != event_id);
                        self.write_record(&record, "ack", Some(&event_id), vec![])
                            .await
                    }
                }
            })
            .await
    }

    async fn read_due(&self) -> Result<Vec<WakeEvent>, String> {
        let record = self.read_record().await?;
        {
            let mut fresh = self.fresh.lock().unwrap_or_else(|p| p.into_inner());
            for report in &record.drops {
                fresh.remove(&report.event_id);
            }
        }
        Ok(record
            .drops
            .into_iter()
            .chain(record.pending)
            .filter(|w| !record.refused.contains_key(&w.event_id))
            .collect())
    }
}

impl WakeOutbox for StoreWakeOutbox {
    fn offer(&self, wake: WakeOffer) -> BoxFuture<'_, Result<WakeEvent, WakeOutboxError>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let record = self.read_record().await.map_err(WakeOutboxError::Other)?;
                    let event = WakeEvent {
                        event_id: self.mint(),
                        scope: wake.scope,
                        source: wake.source,
                        payload: wake.payload,
                    };
                    let mut candidate = record.clone();
                    candidate.pending.push(event.clone());
                    let Some(next) = self.bounded(candidate, &event.scope) else {
                        let overflow_record =
                            self.with_drops(record, &event.scope, WakeDropReason::Overflow, 1);
                        self.write_record(
                            &overflow_record,
                            "overflow",
                            Some(&event.event_id),
                            vec![],
                        )
                        .await
                        .map_err(WakeOutboxError::Other)?;
                        return Err(WakeOutboxError::Overflow(WakeOverflowError { wake: event }));
                    };
                    self.write_record(&next, "offer", Some(&event.event_id), vec![])
                        .await
                        .map_err(WakeOutboxError::Other)?;
                    Ok(event)
                })
                .await
        })
    }

    fn reoffer(&self, send: ReofferSendFn) -> BoxFuture<'_, Result<usize, String>> {
        Box::pin(async move {
            let wakes = self.read_due().await?;
            let count = wakes.len();
            for wake in wakes {
                let receipt = send(wake).await?;
                self.apply_receipt(receipt).await?;
            }
            Ok(count)
        })
    }

    fn due(&self) -> BoxFuture<'_, Result<Vec<WakeEvent>, String>> {
        Box::pin(async move { self.read_due().await })
    }

    fn receipt(&self, receipt: WakeReceipt) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.apply_receipt(receipt).await })
    }

    fn close_scope(&self, scope: &str) -> BoxFuture<'_, Result<(), String>> {
        let scope = scope.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let mut record = self.read_record().await?;
                    if record.closed.contains_key(&scope) {
                        return Ok(());
                    }
                    record.closed.insert(scope, (self.clock)());
                    self.write_record(&record, "close", None, vec![]).await
                })
                .await
        })
    }

    fn sweep(&self) -> BoxFuture<'_, Result<Vec<String>, String>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let DecodedOutbox {
                        mut record,
                        malformed,
                    } = self.decoded().await?;
                    let now = (self.clock)();
                    let lapsed = |w: &WakeEvent| -> bool {
                        let closed_at = record.closed.get(&w.scope).copied();
                        let refused_at = record.refused.get(&w.event_id).map(|r| r.at);
                        closed_at.is_some_and(|c| now > c + WAKE_RETENTION_MS)
                            || refused_at.is_some_and(|r| now > r + WAKE_RETENTION_MS)
                    };
                    let expired: Vec<WakeEvent> = record
                        .pending
                        .iter()
                        .filter(|w| lapsed(w))
                        .cloned()
                        .collect();
                    let lapsed_drops: Vec<WakeEvent> =
                        record.drops.iter().filter(|w| lapsed(w)).cloned().collect();
                    let held: HashSet<String> = record
                        .pending
                        .iter()
                        .chain(record.drops.iter())
                        .map(|w| w.event_id.clone())
                        .collect();
                    let orphans: Vec<String> = record
                        .refused
                        .keys()
                        .filter(|id| !held.contains(*id))
                        .cloned()
                        .collect();
                    if expired.is_empty()
                        && lapsed_drops.is_empty()
                        && malformed.is_empty()
                        && orphans.is_empty()
                    {
                        return Ok(Vec::new());
                    }

                    let mut gone: HashSet<String> = orphans.into_iter().collect();
                    for w in expired.iter().chain(lapsed_drops.iter()) {
                        gone.insert(w.event_id.clone());
                    }
                    let old_refused = record.refused.clone();
                    record.pending.retain(|w| !gone.contains(&w.event_id));
                    record.drops.retain(|w| !gone.contains(&w.event_id));
                    record.refused.retain(|id, _| !gone.contains(id));

                    for wake in &expired {
                        let reason = if old_refused.contains_key(&wake.event_id) {
                            WakeDropReason::Refused
                        } else {
                            WakeDropReason::Expired
                        };
                        record = self.with_drops(record, &wake.scope, reason, 1);
                    }
                    for entry in malformed {
                        if let Some(on_fault) = &self.on_fault {
                            on_fault(MalformedWakeError {
                                entry: entry.clone(),
                            });
                        }
                        if let Some(scope) = malformed_scope(&entry) {
                            record = self.with_drops(record, &scope, WakeDropReason::Malformed, 1);
                        }
                    }

                    let extra: Vec<StoreValue> = expired
                        .iter()
                        .map(|w| {
                            json!({
                                "reason": "wake_expired",
                                "eventId": w.event_id,
                                "scope": w.scope,
                            })
                        })
                        .collect();
                    self.write_record(&record, "sweep", None, extra).await?;
                    Ok(expired.into_iter().map(|w| w.event_id).collect())
                })
                .await
        })
    }

    fn pending(&self) -> BoxFuture<'_, Result<Vec<WakeEvent>, String>> {
        Box::pin(async move { Ok(self.read_record().await?.pending) })
    }
}

pub fn create_wake_outbox(options: WakeOutboxOptions) -> Arc<dyn WakeOutbox> {
    let key = format!(
        "{KIND}:{}",
        serde_json::to_string(&[&options.hand]).unwrap_or_default()
    );
    Arc::new(StoreWakeOutbox {
        hand: options.hand,
        key,
        store: options.store,
        epoch: options.epoch,
        admission: options.admission,
        clock: options.clock,
        limits: options.limits,
        on_fault: options.on_fault,
        fresh: Mutex::new(HashSet::new()),
    })
}
