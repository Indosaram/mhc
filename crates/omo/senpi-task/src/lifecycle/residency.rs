//! Residency cap, LRU eviction and capacity-aware batch admission (`lifecycle/residency.ts`).

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;

use super::admission_lease::{
    AcquireAdmissionLeaseResult, AdmissionLeaseTimingOverrides, acquire_session_admission_lease,
};
use super::context::{LifecycleContext, is_terminal};
use super::destroy::destroy_resident_task;
use super::errors::{AgentLimitReached, LifecycleError, ResidentSummary};
use super::port::DestroyCause;
use super::settings::ResidencyCap;
use super::types::AdmissionResult;
use crate::state::{ResidencyState, TaskRecord, TaskStatus};
use crate::store::StoreError;

pub fn admit_resident(
    context: &LifecycleContext,
    parent_session_id: &str,
) -> Result<AdmissionResult, LifecycleError> {
    let residents = residents_for(context, parent_session_id)?;
    let ResidencyCap::Limited(max_children) = context.config.residency_max_children else {
        return Ok(AdmissionResult::Admitted);
    };
    if residents.len() < max_children {
        return Ok(AdmissionResult::Admitted);
    }
    let Some(victim) = lru_evictable(context, &residents) else {
        return Ok(AdmissionResult::Rejected(AgentLimitReached {
            max_children,
            session_id: parent_session_id.to_string(),
            residents: residents
                .iter()
                .map(|record| ResidentSummary {
                    task_id: record.task_id.clone(),
                    name: record
                        .name
                        .clone()
                        .unwrap_or_else(|| record.task_id.clone()),
                    status: record.status.as_str().to_string(),
                })
                .collect(),
        }));
    };
    let evicted_task_id = victim.task_id.clone();
    destroy_resident_task(context, &evicted_task_id, DestroyCause::Evict, None)?;
    Ok(AdmissionResult::Evicted { evicted_task_id })
}

fn residents_for(
    context: &LifecycleContext,
    parent_session_id: &str,
) -> Result<Vec<TaskRecord>, StoreError> {
    Ok(context
        .store
        .list()?
        .records
        .into_iter()
        .filter(|record| {
            record.parent_session_id == parent_session_id
                && record.residency_state == ResidencyState::Resident
        })
        .collect())
}

fn lru_evictable<'a>(
    context: &LifecycleContext,
    residents: &'a [TaskRecord],
) -> Option<&'a TaskRecord> {
    residents
        .iter()
        .filter(|record| {
            is_terminal(record.status) && !context.registry.has_pending_sends(&record.task_id)
        })
        .min_by(|left, right| left.updated_at.cmp(&right.updated_at))
}

pub(crate) fn is_suspended_residency(state: ResidencyState) -> bool {
    matches!(
        state,
        ResidencyState::PersistedOnly | ResidencyState::RpcDetached
    )
}

pub(crate) fn is_revivable_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending
            | TaskStatus::Running
            | TaskStatus::Completed
            | TaskStatus::Error
            | TaskStatus::Interrupted
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchAdmissionDeferral {
    Capacity,
    LockContended,
    ForeignLiveOwner,
    LeaseLost,
}

impl BatchAdmissionDeferral {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::LockContended => "lock_contended",
            Self::ForeignLiveOwner => "foreign_live_owner",
            Self::LeaseLost => "lease_lost",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchAdmissionOutcome {
    Claimed {
        task_id: String,
    },
    Deferred {
        task_id: String,
        reason: BatchAdmissionDeferral,
    },
}

impl BatchAdmissionOutcome {
    pub fn task_id(&self) -> &str {
        match self {
            Self::Claimed { task_id } | Self::Deferred { task_id, .. } => task_id,
        }
    }

    fn deferred(task_id: &str, reason: BatchAdmissionDeferral) -> Self {
        Self::Deferred {
            task_id: task_id.to_string(),
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchLeaseState {
    Acquired,
    LockContended,
    LeaseLost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchAdmissionResult {
    pub lease: BatchLeaseState,
    pub outcomes: Vec<BatchAdmissionOutcome>,
}

pub type AcquireLeaseFn = Arc<
    dyn Fn(
            &std::path::Path,
            &str,
            AdmissionLeaseTimingOverrides,
        ) -> Result<AcquireAdmissionLeaseResult, StoreError>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct BatchAdmissionOptions {
    pub timing: AdmissionLeaseTimingOverrides,
    pub acquire_lease: Option<AcquireLeaseFn>,
    pub exclude_task_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyClaimResult {
    Claimed,
    NotClaimable,
}

/// CAS-claims a residency slot for this process when `expect` still holds on the fresh record.
pub fn claim_residency_slot(
    context: &LifecycleContext,
    task_id: &str,
    expect: impl Fn(&TaskRecord) -> bool,
) -> Result<ResidencyClaimResult, StoreError> {
    let mut applied = false;
    let timestamp = context.now_iso();
    let claimed = context.store.mutate(task_id, &mut |fresh| {
        if !expect(fresh) {
            return fresh.clone();
        }
        applied = true;
        let mut next = fresh.clone();
        next.residency_state = ResidencyState::Resident;
        next.host_pid = Some(context.host_pid);
        next.updated_at.clone_from(&timestamp);
        next
    })?;
    Ok(if claimed.is_some() && applied {
        ResidencyClaimResult::Claimed
    } else {
        ResidencyClaimResult::NotClaimable
    })
}

/// Reclaims an orphaned resident without consuming admission capacity (expected-owner CAS).
pub fn reclaim_orphaned_resident(
    context: &LifecycleContext,
    observed: &TaskRecord,
) -> Result<ResidencyClaimResult, StoreError> {
    claim_residency_slot(context, &observed.task_id, |fresh| {
        fresh.residency_state == ResidencyState::Resident
            && fresh.host_pid == observed.host_pid
            && fresh.updated_at == observed.updated_at
    })
}

pub fn admit_suspended_batch(
    context: &LifecycleContext,
    parent_session_id: &str,
    options: &BatchAdmissionOptions,
) -> Result<BatchAdmissionResult, LifecycleError> {
    let state_dir = context.store.state_dir().to_path_buf();
    let acquired = match &options.acquire_lease {
        Some(acquire) => acquire(&state_dir, parent_session_id, options.timing)?,
        None => acquire_session_admission_lease(&state_dir, parent_session_id, options.timing)?,
    };
    let AcquireAdmissionLeaseResult::Acquired(lease) = acquired else {
        return Ok(BatchAdmissionResult {
            lease: BatchLeaseState::LockContended,
            outcomes: revival_candidates(context, parent_session_id, &options.exclude_task_ids)?
                .iter()
                .map(|record| {
                    BatchAdmissionOutcome::deferred(
                        &record.task_id,
                        BatchAdmissionDeferral::LockContended,
                    )
                })
                .collect(),
        });
    };
    let result = admit_under_lease(context, parent_session_id, options, &*lease);
    lease.release();
    result
}

fn admit_under_lease(
    context: &LifecycleContext,
    parent_session_id: &str,
    options: &BatchAdmissionOptions,
    lease: &dyn super::admission_lease::SessionAdmissionLease,
) -> Result<BatchAdmissionResult, LifecycleError> {
    let mut outcomes = Vec::new();
    let mut lease_state = BatchLeaseState::Acquired;
    let candidates = by_revival_priority(revival_candidates(
        context,
        parent_session_id,
        &options.exclude_task_ids,
    )?);
    let available = match context.config.residency_max_children {
        ResidencyCap::Unlimited => candidates.len(),
        ResidencyCap::Limited(max) => {
            max.saturating_sub(residents_for(context, parent_session_id)?.len())
        }
    }
    .min(candidates.len());
    for record in &candidates[..available] {
        if !lease.is_owner() {
            lease_state = BatchLeaseState::LeaseLost;
            outcomes.push(BatchAdmissionOutcome::deferred(
                &record.task_id,
                BatchAdmissionDeferral::LeaseLost,
            ));
            continue;
        }
        let claim = claim_residency_slot(context, &record.task_id, |fresh| {
            fresh.parent_session_id == parent_session_id
                && is_suspended_residency(fresh.residency_state)
                && is_revivable_status(fresh.status)
                && fresh.killed != Some(true)
        });
        outcomes.push(match claim {
            Ok(ResidencyClaimResult::Claimed) => BatchAdmissionOutcome::Claimed {
                task_id: record.task_id.clone(),
            },
            Ok(ResidencyClaimResult::NotClaimable) => BatchAdmissionOutcome::deferred(
                &record.task_id,
                BatchAdmissionDeferral::ForeignLiveOwner,
            ),
            Err(_) => BatchAdmissionOutcome::deferred(
                &record.task_id,
                BatchAdmissionDeferral::LockContended,
            ),
        });
    }
    for record in &candidates[available..] {
        outcomes.push(BatchAdmissionOutcome::deferred(
            &record.task_id,
            BatchAdmissionDeferral::Capacity,
        ));
    }
    Ok(BatchAdmissionResult {
        lease: lease_state,
        outcomes,
    })
}

fn revival_candidates(
    context: &LifecycleContext,
    parent_session_id: &str,
    exclude: &BTreeSet<String>,
) -> Result<Vec<TaskRecord>, StoreError> {
    Ok(context
        .store
        .list()?
        .records
        .into_iter()
        .filter(|record| {
            !exclude.contains(&record.task_id)
                && record.parent_session_id == parent_session_id
                && is_suspended_residency(record.residency_state)
                && is_revivable_status(record.status)
                && record.killed != Some(true)
        })
        .collect())
}

/// Non-terminal first, then most recently updated, then task id.
fn by_revival_priority(mut records: Vec<TaskRecord>) -> Vec<TaskRecord> {
    records.sort_by(|left, right| {
        let terminality = is_terminal(left.status).cmp(&is_terminal(right.status));
        if terminality != Ordering::Equal {
            return terminality;
        }
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    records
}
