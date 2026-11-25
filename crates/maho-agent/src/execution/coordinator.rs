//! Port of senpi `packages/agent/src/execution/coordinator.ts`.

use std::collections::HashSet;
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::durable::store::{DurableStore, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::coordinator_codec::decode_batch_record;
use super::coordinator_consent::{
    ConsentStep, RawConsentResult, gate_raw_call, resolve_frozen_consent,
};
use super::coordinator_plan::{abort_cause, build_batch_manifest, finished, settled_outcome};
use super::coordinator_types::{
    BatchConsent, BatchDecision, BatchExecutorFactory, BatchPlan, BatchRecord, BatchResult,
    CallOutcome, ExecutionRef, ManifestCall, NotRunCause, PrepareCallPayload, PrepareReply,
    PrepareRequest, SettleReply,
};

const PREFIX: &str = "execution.batch:";

fn key_of(issuer: &str, batch_id: &str) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(&[issuer, batch_id]).unwrap_or_default()
    )
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("batch {0} already has a different durable manifest")]
pub struct BatchManifestConflictError(pub String);

pub struct BatchCoordinatorOptions {
    pub issuer: String,
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
    pub executor: BatchExecutorFactory,
    pub consent: Option<Arc<dyn BatchConsent>>,
}

pub trait BatchCoordinator: Send + Sync {
    fn run(&self, plan: BatchPlan) -> BoxFuture<'_, Result<BatchResult, String>>;
    fn read(&self, batch_id: &str) -> BoxFuture<'_, Result<Option<BatchRecord>, String>>;
}

struct StoreBatchCoordinator {
    issuer: String,
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
    executor: BatchExecutorFactory,
    consent: Option<Arc<dyn BatchConsent>>,
}

impl StoreBatchCoordinator {
    async fn read_inner(&self, batch_id: &str) -> Result<Option<BatchRecord>, String> {
        let value = self
            .store
            .get_document(&key_of(&self.issuer, batch_id))
            .await
            .map_err(|e| e.to_string())?;
        match value {
            None => Ok(None),
            Some(v) => decode_batch_record(&v).map(Some).map_err(|e| e.to_string()),
        }
    }

    async fn persist(
        &self,
        batch_id: &str,
        next: &BatchRecord,
        step: &str,
        call: Option<&ManifestCall>,
    ) -> Result<BatchRecord, String> {
        let key = key_of(&self.issuer, batch_id);
        let data = json!({
            "batchId": batch_id,
            "step": step,
            "executionId": call.map(|c| c.execution_id.as_str()),
        });
        let doc = serde_json::to_value(next).map_err(|e| e.to_string())?;
        let store = Arc::clone(&self.store);
        let epoch = self.epoch;
        self.admission
            .admit(|| async move {
                store
                    .commit(
                        epoch,
                        vec![
                            StoreWrite::Append {
                                id: store.mint_id(),
                                kind: "execution.batch".to_owned(),
                                data,
                            },
                            StoreWrite::Put { key, data: doc },
                        ],
                        Box::new(|_| {}),
                    )
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;
        Ok(next.clone())
    }
}

impl BatchCoordinator for StoreBatchCoordinator {
    fn read(&self, batch_id: &str) -> BoxFuture<'_, Result<Option<BatchRecord>, String>> {
        let batch_id = batch_id.to_owned();
        Box::pin(async move { self.read_inner(&batch_id).await })
    }

    fn run(&self, plan: BatchPlan) -> BoxFuture<'_, Result<BatchResult, String>> {
        Box::pin(async move {
            let manifest = build_batch_manifest(&self.issuer, &plan);
            let mut record = match self.read_inner(&plan.batch_id).await? {
                None => {
                    let size = manifest.calls.len();
                    let initial = BatchRecord {
                        manifest: manifest.clone(),
                        prepares: vec![None; size],
                        decision: None,
                        released: vec![false; size],
                        outcomes: vec![None; size],
                        consents: None,
                    };
                    self.persist(&plan.batch_id, &initial, "manifest", None)
                        .await?
                }
                Some(existing) => {
                    if existing.manifest.manifest_digest != manifest.manifest_digest {
                        return Err(BatchManifestConflictError(plan.batch_id).to_string());
                    }
                    existing
                }
            };

            let make_ref = |call: &ManifestCall| ExecutionRef {
                issuer: self.issuer.clone(),
                execution_id: call.execution_id.clone(),
                generation: manifest.generation,
            };

            if record.decision.is_none() {
                for call in &manifest.calls {
                    let existing_reply = record
                        .prepares
                        .get(call.source_index)
                        .and_then(|r| r.clone());
                    let reply = match existing_reply {
                        Some(r) => r,
                        None => {
                            if let Some(gate) = &self.consent {
                                let size = manifest.calls.len();
                                let recorded_consent = record
                                    .consents
                                    .as_ref()
                                    .and_then(|c| c.get(call.source_index).cloned().flatten());
                                let batch_id = plan.batch_id.clone();
                                let record_slot = Arc::new(tokio::sync::Mutex::new(record.clone()));
                                let record_slot_c = Arc::clone(&record_slot);
                                let step_res = gate_raw_call(ConsentStep {
                                    consent: gate.as_ref(),
                                    executor: (self.executor)(&call.executor),
                                    exec_ref: make_ref(call),
                                    call,
                                    lease_until: manifest.lease_until,
                                    recorded: recorded_consent,
                                    record: Box::new(move |answered| {
                                        Box::pin(async move {
                                            let mut slot = record_slot_c.lock().await;
                                            let mut consents = slot
                                                .consents
                                                .clone()
                                                .unwrap_or_else(|| vec![None; size]);
                                            consents[call.source_index] = Some(answered);
                                            slot.consents = Some(consents);
                                            let updated = self
                                                .persist(&batch_id, &slot, "consent", Some(call))
                                                .await?;
                                            *slot = updated;
                                            Ok(())
                                        })
                                    }),
                                })
                                .await?;
                                record = record_slot.lock().await.clone();
                                match step_res {
                                    RawConsentResult::Blocked(blocked_reply) => {
                                        let mut next = record.clone();
                                        next.prepares[call.source_index] =
                                            Some(blocked_reply.clone());
                                        record = self
                                            .persist(&plan.batch_id, &next, "prepare", Some(call))
                                            .await?;
                                        blocked_reply
                                    }
                                    RawConsentResult::Allowed { authorized_digest } => {
                                        let r = (self.executor)(&call.executor)
                                            .prepare(PrepareRequest {
                                                issuer: self.issuer.clone(),
                                                execution_id: call.execution_id.clone(),
                                                generation: manifest.generation,
                                                batch_id: manifest.batch_id.clone(),
                                                manifest_digest: manifest.manifest_digest.clone(),
                                                scope: manifest.scope.clone(),
                                                workspace: manifest.workspace.clone(),
                                                admitted_at: manifest.admitted_at,
                                                lease_until: manifest.lease_until,
                                                request_digest: call.request_digest.clone(),
                                                call: PrepareCallPayload {
                                                    name: call.name.clone(),
                                                    args: call.args.clone(),
                                                },
                                                authorized_digest,
                                            })
                                            .await?;
                                        let mut next = record.clone();
                                        next.prepares[call.source_index] = Some(r.clone());
                                        record = self
                                            .persist(&plan.batch_id, &next, "prepare", Some(call))
                                            .await?;
                                        r
                                    }
                                }
                            } else {
                                let r = (self.executor)(&call.executor)
                                    .prepare(PrepareRequest {
                                        issuer: self.issuer.clone(),
                                        execution_id: call.execution_id.clone(),
                                        generation: manifest.generation,
                                        batch_id: manifest.batch_id.clone(),
                                        manifest_digest: manifest.manifest_digest.clone(),
                                        scope: manifest.scope.clone(),
                                        workspace: manifest.workspace.clone(),
                                        admitted_at: manifest.admitted_at,
                                        lease_until: manifest.lease_until,
                                        request_digest: call.request_digest.clone(),
                                        call: PrepareCallPayload {
                                            name: call.name.clone(),
                                            args: call.args.clone(),
                                        },
                                        authorized_digest: None,
                                    })
                                    .await?;
                                let mut next = record.clone();
                                next.prepares[call.source_index] = Some(r.clone());
                                record = self
                                    .persist(&plan.batch_id, &next, "prepare", Some(call))
                                    .await?;
                                r
                            }
                        }
                    };
                    if abort_cause(&reply).is_some() {
                        break;
                    }
                }

                for call in &manifest.calls {
                    if record
                        .prepares
                        .iter()
                        .any(|r| r.as_ref().is_some_and(|p| abort_cause(p).is_some()))
                    {
                        break;
                    }
                    let Some(PrepareReply::PreparedAwaitingConsent { frozen_digest }) = record
                        .prepares
                        .get(call.source_index)
                        .and_then(|r| r.as_ref())
                        .cloned()
                    else {
                        continue;
                    };
                    let Some(gate) = &self.consent else {
                        return Err(format!(
                            "batch {}: a call awaits consent but none is configured",
                            plan.batch_id
                        ));
                    };
                    let size = manifest.calls.len();
                    let recorded_consent = record
                        .consents
                        .as_ref()
                        .and_then(|c| c.get(call.source_index).cloned().flatten());
                    let batch_id = plan.batch_id.clone();
                    let record_slot = Arc::new(tokio::sync::Mutex::new(record.clone()));
                    let record_slot_c = Arc::clone(&record_slot);
                    let resolved = resolve_frozen_consent(
                        ConsentStep {
                            consent: gate.as_ref(),
                            executor: (self.executor)(&call.executor),
                            exec_ref: make_ref(call),
                            call,
                            lease_until: manifest.lease_until,
                            recorded: recorded_consent,
                            record: Box::new(move |answered| {
                                Box::pin(async move {
                                    let mut slot = record_slot_c.lock().await;
                                    let mut consents =
                                        slot.consents.clone().unwrap_or_else(|| vec![None; size]);
                                    consents[call.source_index] = Some(answered);
                                    slot.consents = Some(consents);
                                    let updated = self
                                        .persist(&batch_id, &slot, "consent", Some(call))
                                        .await?;
                                    *slot = updated;
                                    Ok(())
                                })
                            }),
                        },
                        &frozen_digest,
                    )
                    .await?;
                    record = record_slot.lock().await.clone();
                    let mut next = record.clone();
                    next.prepares[call.source_index] = Some(resolved);
                    record = self
                        .persist(&plan.batch_id, &next, "prepare", Some(call))
                        .await?;
                }

                let aborted = record
                    .prepares
                    .iter()
                    .any(|r| r.as_ref().is_some_and(|p| abort_cause(p).is_some()));
                let mut next = record.clone();
                next.decision = Some(if aborted {
                    BatchDecision::Abort
                } else {
                    BatchDecision::Release
                });
                record = self
                    .persist(&plan.batch_id, &next, "decision", None)
                    .await?;
            }

            let cause = record
                .prepares
                .iter()
                .filter_map(|r| r.as_ref().and_then(abort_cause))
                .next()
                .unwrap_or(NotRunCause::BatchAbort);

            for call in &manifest.calls {
                if record.outcomes[call.source_index].is_some() {
                    continue;
                }
                let reply = record.prepares[call.source_index].clone();
                let outcome = match reply {
                    Some(PrepareReply::Settled { outcome }) => {
                        Some(settled_outcome(SettleReply::Settled { outcome }))
                    }
                    Some(PrepareReply::Blocked { result }) => {
                        let _ = (self.executor)(&call.executor)
                            .abort(make_ref(call))
                            .await?;
                        Some(CallOutcome::Blocked { result })
                    }
                    other => {
                        if record.decision == Some(BatchDecision::Abort) {
                            if other.is_some() {
                                let _ = (self.executor)(&call.executor)
                                    .abort(make_ref(call))
                                    .await?;
                            }
                            Some(CallOutcome::NotRun { cause })
                        } else {
                            None
                        }
                    }
                };
                if let Some(outcome) = outcome {
                    let mut next = record.clone();
                    next.outcomes[call.source_index] = Some(outcome);
                    record = self
                        .persist(&plan.batch_id, &next, "outcome", Some(call))
                        .await?;
                }
            }

            let mut inflight_indices = HashSet::<usize>::new();
            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<(usize, Result<SettleReply, String>)>();
            loop {
                for call in &manifest.calls {
                    let index = call.source_index;
                    if record.outcomes[index].is_some() || inflight_indices.contains(&index) {
                        continue;
                    }
                    if !record.released[index] {
                        let ready = call
                            .depends_on
                            .iter()
                            .all(|&dep| finished(&manifest.calls, &record.outcomes, dep));
                        if !ready {
                            continue;
                        }
                        let mut next = record.clone();
                        next.released[index] = true;
                        record = self
                            .persist(&plan.batch_id, &next, "release", Some(call))
                            .await?;
                    }
                    inflight_indices.insert(index);
                    let exec = (self.executor)(&call.executor);
                    let exec_ref = make_ref(call);
                    let tx_c = tx.clone();
                    tokio::spawn(async move {
                        let res = exec.release(exec_ref).await;
                        let _ = tx_c.send((index, res));
                    });
                }
                if inflight_indices.is_empty() {
                    break;
                }
                let Some((index, res)) = rx.recv().await else {
                    break;
                };
                inflight_indices.remove(&index);
                let reply = res?;
                if let Some(call) = manifest.calls.get(index) {
                    let mut next = record.clone();
                    next.outcomes[index] = Some(settled_outcome(reply));
                    record = self
                        .persist(&plan.batch_id, &next, "outcome", Some(call))
                        .await?;
                }
            }

            let decision = record.decision.ok_or_else(|| {
                format!("batch {} ended with an unsettled decision", plan.batch_id)
            })?;
            let mut final_outcomes = Vec::with_capacity(record.outcomes.len());
            for o in record.outcomes {
                match o {
                    Some(outcome) => final_outcomes.push(outcome),
                    None => {
                        return Err(format!(
                            "batch {} ended with an unsettled call",
                            plan.batch_id
                        ));
                    }
                }
            }
            Ok(BatchResult {
                decision,
                outcomes: final_outcomes,
            })
        })
    }
}

pub fn create_batch_coordinator(options: BatchCoordinatorOptions) -> Arc<dyn BatchCoordinator> {
    Arc::new(StoreBatchCoordinator {
        issuer: options.issuer,
        store: options.store,
        epoch: options.epoch,
        admission: options.admission,
        executor: options.executor,
        consent: options.consent,
    })
}
