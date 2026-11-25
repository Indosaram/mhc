//! Port of senpi `packages/agent/src/execution/nested.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::durable::store::{DurableStore, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::canonical_json::canonical_json;
use super::coordinator_types::ExecutionRef;
use super::nested_codec::decode_nested_record;
use super::nested_types::{
    ActivityRecord, ActivityRefusal, CancelDeliveryState, NESTED_LIFETIME_MS, NestedActivity,
    NestedAdmission, NestedCancelTarget, NestedChild, NestedParent, NestedPhase, NestedRecord,
    NestedRefusal, NestedRequest,
};

const PREFIX: &str = "execution.nested:";

pub struct NestedAdmissionOptions {
    pub issuer: String,
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenNestedResult {
    Opened,
    RefusedConflictingParent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterActivityResult {
    Registered,
    Refused(ActivityRefusal),
}

pub type DeliverCancelFn =
    Arc<dyn Fn(NestedCancelTarget) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub trait NestedAdmissionControl: Send + Sync {
    fn open(&self, parent: NestedParent) -> BoxFuture<'_, Result<OpenNestedResult, String>>;
    fn release(&self, parent: &str) -> BoxFuture<'_, Result<(), String>>;
    fn settle(&self, parent: &str) -> BoxFuture<'_, Result<(), String>>;
    fn renew(&self, parent: &str, lease_until: i64) -> BoxFuture<'_, Result<i64, String>>;
    fn register_activity(
        &self,
        activity: NestedActivity,
    ) -> BoxFuture<'_, Result<RegisterActivityResult, String>>;
    fn end_activity(&self, parent: &str, activity_id: &str) -> BoxFuture<'_, Result<(), String>>;
    fn admit(&self, request: NestedRequest) -> BoxFuture<'_, Result<NestedAdmission, String>>;
    fn cancel(&self, parent: &str) -> BoxFuture<'_, Result<Vec<NestedCancelTarget>, String>>;
    fn deliver_cancels(
        &self,
        parent: &str,
        send: DeliverCancelFn,
    ) -> BoxFuture<'_, Result<usize, String>>;
    fn read(&self, parent: &str) -> BoxFuture<'_, Result<Option<NestedRecord>, String>>;
}

pub fn nested_child_id(parent: &str, child_request_id: &str) -> String {
    serde_json::to_string(&[parent, child_request_id]).unwrap_or_default()
}

struct StoreNestedAdmission {
    issuer: String,
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl StoreNestedAdmission {
    fn key_of(&self, parent: &str) -> String {
        format!(
            "{PREFIX}{}",
            serde_json::to_string(&[&self.issuer, parent]).unwrap_or_default()
        )
    }

    async fn read_inner(&self, parent: &str) -> Result<Option<NestedRecord>, String> {
        let val = self
            .store
            .get_document(&self.key_of(parent))
            .await
            .map_err(|e| e.to_string())?;
        match val {
            None => Ok(None),
            Some(v) => decode_nested_record(&v)
                .map(Some)
                .map_err(|e| e.to_string()),
        }
    }

    async fn write_inner(
        &self,
        record: &NestedRecord,
        step: &str,
        execution_id: Option<&str>,
    ) -> Result<(), String> {
        let data = json!({
            "parent": record.parent.execution_id,
            "step": step,
            "executionId": execution_id,
        });
        let doc = serde_json::to_value(record).map_err(|e| e.to_string())?;
        self.store
            .commit(
                self.epoch,
                vec![
                    StoreWrite::Append {
                        id: self.store.mint_id(),
                        kind: "execution.nested".to_owned(),
                        data,
                    },
                    StoreWrite::Put {
                        key: self.key_of(&record.parent.execution_id),
                        data: doc,
                    },
                ],
                Box::new(|_| {}),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn is_live(record: &NestedRecord) -> bool {
        !record.cancelled && record.phase != NestedPhase::Settled
    }

    fn allowed_targets(record: &NestedRecord) -> Vec<String> {
        let mut out = record.parent.capabilities.preparation.clone();
        if record.phase != NestedPhase::Preparation {
            out.extend(record.parent.capabilities.body.iter().cloned());
        }
        out
    }
}

impl NestedAdmissionControl for StoreNestedAdmission {
    fn read(&self, parent: &str) -> BoxFuture<'_, Result<Option<NestedRecord>, String>> {
        let parent = parent.to_owned();
        Box::pin(async move { self.read_inner(&parent).await })
    }

    fn open(&self, parent: NestedParent) -> BoxFuture<'_, Result<OpenNestedResult, String>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let existing = self.read_inner(&parent.execution_id).await?;
                    if let Some(rec) = existing {
                        let existing_json =
                            serde_json::to_value(&rec.parent).map_err(|e| e.to_string())?;
                        let incoming_json =
                            serde_json::to_value(&parent).map_err(|e| e.to_string())?;
                        return Ok(
                            if canonical_json(&existing_json) == canonical_json(&incoming_json) {
                                OpenNestedResult::Opened
                            } else {
                                OpenNestedResult::RefusedConflictingParent
                            },
                        );
                    }
                    let expires_at = parent
                        .lease_until
                        .min(parent.admitted_at + NESTED_LIFETIME_MS);
                    let opened = NestedRecord {
                        parent,
                        phase: NestedPhase::Preparation,
                        cancelled: false,
                        expires_at,
                        debited: 0,
                        children: BTreeMap::new(),
                        activities: BTreeMap::new(),
                        cancels: BTreeMap::new(),
                    };
                    self.write_inner(&opened, "open", None).await?;
                    Ok(OpenNestedResult::Opened)
                })
                .await
        })
    }

    fn release(&self, parent: &str) -> BoxFuture<'_, Result<(), String>> {
        let parent = parent.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut rec) = self.read_inner(&parent).await? else {
                        return Ok(());
                    };
                    if rec.phase != NestedPhase::Preparation || rec.cancelled {
                        return Ok(());
                    }
                    rec.phase = NestedPhase::Body;
                    self.write_inner(&rec, "release", None).await
                })
                .await
        })
    }

    fn settle(&self, parent: &str) -> BoxFuture<'_, Result<(), String>> {
        let parent = parent.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut rec) = self.read_inner(&parent).await? else {
                        return Ok(());
                    };
                    if !Self::is_live(&rec) {
                        return Ok(());
                    }
                    rec.phase = NestedPhase::Settled;
                    self.write_inner(&rec, "settle", None).await
                })
                .await
        })
    }

    fn renew(&self, parent: &str, lease_until: i64) -> BoxFuture<'_, Result<i64, String>> {
        let parent = parent.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let mut rec = self
                        .read_inner(&parent)
                        .await?
                        .ok_or_else(|| format!("no nested set for {parent}"))?;
                    let cap = rec.parent.admitted_at + NESTED_LIFETIME_MS;
                    let expires_at = rec.expires_at.max(lease_until.min(cap));
                    if expires_at != rec.expires_at {
                        rec.expires_at = expires_at;
                        self.write_inner(&rec, "renew", None).await?;
                    }
                    Ok(expires_at)
                })
                .await
        })
    }

    fn register_activity(
        &self,
        activity: NestedActivity,
    ) -> BoxFuture<'_, Result<RegisterActivityResult, String>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut rec) = self.read_inner(&activity.parent).await? else {
                        return Ok(RegisterActivityResult::Refused(
                            ActivityRefusal::UnknownParent,
                        ));
                    };
                    if let Some(existing) = rec.activities.get(&activity.activity_id) {
                        let existing_norm = json!({
                            "activityId": existing.activity_id,
                            "parent": existing.parent,
                            "executor": existing.executor,
                            "scope": existing.scope,
                            "targets": existing.targets,
                            "budget": existing.budget,
                            "expiresAt": existing.expires_at,
                        });
                        let incoming_norm =
                            serde_json::to_value(&activity).map_err(|e| e.to_string())?;
                        return Ok(
                            if canonical_json(&existing_norm) == canonical_json(&incoming_norm) {
                                RegisterActivityResult::Registered
                            } else {
                                RegisterActivityResult::Refused(
                                    ActivityRefusal::ConflictingActivity,
                                )
                            },
                        );
                    }
                    if !Self::is_live(&rec) || (self.clock)() > rec.expires_at {
                        return Ok(RegisterActivityResult::Refused(
                            ActivityRefusal::ParentTerminal,
                        ));
                    }
                    let held = Self::allowed_targets(&rec);
                    let subset = activity.executor == rec.parent.executor
                        && activity.scope == rec.parent.scope
                        && activity.targets.iter().all(|t| held.contains(t))
                        && activity.expires_at <= rec.expires_at
                        && activity.budget <= rec.parent.budget.saturating_sub(rec.debited);
                    if !subset {
                        return Ok(RegisterActivityResult::Refused(
                            ActivityRefusal::ExceedsParent,
                        ));
                    }
                    let act_id = activity.activity_id.clone();
                    rec.activities.insert(
                        act_id.clone(),
                        ActivityRecord {
                            activity_id: activity.activity_id,
                            parent: activity.parent,
                            executor: activity.executor,
                            scope: activity.scope,
                            targets: activity.targets,
                            budget: activity.budget,
                            expires_at: activity.expires_at,
                            debited: 0,
                            ended: false,
                        },
                    );
                    self.write_inner(&rec, "activity", Some(&act_id)).await?;
                    Ok(RegisterActivityResult::Registered)
                })
                .await
        })
    }

    fn end_activity(&self, parent: &str, activity_id: &str) -> BoxFuture<'_, Result<(), String>> {
        let parent = parent.to_owned();
        let activity_id = activity_id.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut rec) = self.read_inner(&parent).await? else {
                        return Ok(());
                    };
                    let Some(activity) = rec.activities.get_mut(&activity_id) else {
                        return Ok(());
                    };
                    if activity.ended {
                        return Ok(());
                    }
                    activity.ended = true;
                    self.write_inner(&rec, "activity_end", Some(&activity_id))
                        .await
                })
                .await
        })
    }

    fn admit(&self, request: NestedRequest) -> BoxFuture<'_, Result<NestedAdmission, String>> {
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut rec) = self.read_inner(&request.parent).await? else {
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::UnknownParent,
                        });
                    };
                    let activity = match &request.activity_id {
                        None => None,
                        Some(act_id) => match rec.activities.get(act_id) {
                            Some(act) => Some(act.clone()),
                            None => {
                                return Ok(NestedAdmission::Refused {
                                    reason: NestedRefusal::UnknownActivity,
                                });
                            }
                        },
                    };
                    let (owner_executor, owner_scope) = match &activity {
                        Some(act) => (act.executor.as_str(), act.scope.as_str()),
                        None => (rec.parent.executor.as_str(), rec.parent.scope.as_str()),
                    };
                    if request.sender != owner_executor {
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::NotParentExecutor,
                        });
                    }
                    if request.parent_generation != rec.parent.generation {
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::StaleGeneration,
                        });
                    }
                    if request.scope != owner_scope {
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::ScopeMismatch,
                        });
                    }

                    let id = nested_child_id(&request.parent, &request.child_request_id);
                    let child_ref = ExecutionRef {
                        issuer: self.issuer.clone(),
                        execution_id: id.clone(),
                        generation: 1,
                    };
                    if let Some(known) = rec.children.get(&request.child_request_id) {
                        let cost_ok = request.cost >= 0 && known.cost == request.cost as u64;
                        if known.target == request.target
                            && cost_ok
                            && known.activity_id == request.activity_id
                        {
                            return Ok(NestedAdmission::Admitted {
                                child: child_ref,
                                target: known.target.clone(),
                                replayed: true,
                            });
                        }
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::ConflictingRequest,
                        });
                    }

                    if request.cost < 0 {
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::InvalidCost,
                        });
                    }
                    let cost = request.cost as u64;
                    let now = (self.clock)();
                    if let Some(act) = &activity {
                        if act.ended || rec.cancelled || now > act.expires_at {
                            return Ok(NestedAdmission::Refused {
                                reason: NestedRefusal::ActivityEnded,
                            });
                        }
                        if !act.targets.contains(&request.target) {
                            return Ok(NestedAdmission::Refused {
                                reason: NestedRefusal::NotInNestedSet,
                            });
                        }
                        if act.debited.saturating_add(cost) > act.budget {
                            return Ok(NestedAdmission::Refused {
                                reason: NestedRefusal::BudgetExhausted,
                            });
                        }
                    } else {
                        if !Self::is_live(&rec) {
                            return Ok(NestedAdmission::Refused {
                                reason: NestedRefusal::ParentTerminal,
                            });
                        }
                        if now > rec.expires_at {
                            return Ok(NestedAdmission::Refused {
                                reason: NestedRefusal::Expired,
                            });
                        }
                        if !Self::allowed_targets(&rec).contains(&request.target) {
                            return Ok(NestedAdmission::Refused {
                                reason: NestedRefusal::NotInNestedSet,
                            });
                        }
                    }
                    if rec.debited.saturating_add(cost) > rec.parent.budget {
                        return Ok(NestedAdmission::Refused {
                            reason: NestedRefusal::BudgetExhausted,
                        });
                    }

                    let child = NestedChild {
                        execution_id: id.clone(),
                        target: request.target.clone(),
                        cost,
                        origin: "tool".to_owned(),
                        activity_id: request.activity_id.clone(),
                    };
                    rec.debited = rec.debited.saturating_add(cost);
                    rec.children.insert(request.child_request_id, child);
                    if let Some(act_id) = &request.activity_id
                        && let Some(act_mut) = rec.activities.get_mut(act_id)
                    {
                        act_mut.debited = act_mut.debited.saturating_add(cost);
                    }
                    self.write_inner(&rec, "admit", Some(&id)).await?;
                    Ok(NestedAdmission::Admitted {
                        child: child_ref,
                        target: request.target,
                        replayed: false,
                    })
                })
                .await
        })
    }

    fn cancel(&self, parent: &str) -> BoxFuture<'_, Result<Vec<NestedCancelTarget>, String>> {
        let parent = parent.to_owned();
        Box::pin(async move {
            self.admission
                .admit(|| async move {
                    let Some(mut rec) = self.read_inner(&parent).await? else {
                        return Ok(Vec::new());
                    };
                    let targets: Vec<NestedCancelTarget> = rec
                        .children
                        .values()
                        .map(|child| NestedCancelTarget {
                            exec_ref: ExecutionRef {
                                issuer: self.issuer.clone(),
                                execution_id: child.execution_id.clone(),
                                generation: 1,
                            },
                            executor: child.target.clone(),
                        })
                        .collect();
                    if rec.cancelled {
                        return Ok(targets);
                    }
                    rec.cancelled = true;
                    for act in rec.activities.values_mut() {
                        act.ended = true;
                    }
                    for child in rec.children.values() {
                        rec.cancels
                            .insert(child.execution_id.clone(), CancelDeliveryState::Queued);
                    }
                    self.write_inner(&rec, "cancel", None).await?;
                    Ok(targets)
                })
                .await
        })
    }

    fn deliver_cancels(
        &self,
        parent: &str,
        send: DeliverCancelFn,
    ) -> BoxFuture<'_, Result<usize, String>> {
        let parent = parent.to_owned();
        Box::pin(async move {
            let Some(rec) = self.read_inner(&parent).await? else {
                return Ok(0);
            };
            let mut delivered = 0usize;
            for child in rec.children.values() {
                if rec.cancels.get(&child.execution_id) != Some(&CancelDeliveryState::Queued) {
                    continue;
                }
                send(NestedCancelTarget {
                    exec_ref: ExecutionRef {
                        issuer: self.issuer.clone(),
                        execution_id: child.execution_id.clone(),
                        generation: 1,
                    },
                    executor: child.target.clone(),
                })
                .await?;
                let child_exec_id = child.execution_id.clone();
                let parent_owned = parent.clone();
                self.admission
                    .admit(|| async move {
                        let Some(mut latest) = self.read_inner(&parent_owned).await? else {
                            return Ok(());
                        };
                        if latest.cancels.get(&child_exec_id) != Some(&CancelDeliveryState::Queued)
                        {
                            return Ok(());
                        }
                        latest
                            .cancels
                            .insert(child_exec_id.clone(), CancelDeliveryState::Delivered);
                        self.write_inner(&latest, "cancel_delivered", Some(&child_exec_id))
                            .await
                    })
                    .await?;
                delivered += 1;
            }
            Ok(delivered)
        })
    }
}

pub fn create_nested_admission(options: NestedAdmissionOptions) -> Arc<dyn NestedAdmissionControl> {
    Arc::new(StoreNestedAdmission {
        issuer: options.issuer,
        store: options.store,
        epoch: options.epoch,
        admission: options.admission,
        clock: options.clock,
    })
}
