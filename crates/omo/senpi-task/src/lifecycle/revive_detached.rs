//! Epoch- and ownership-fenced cold revival and its undo operation.
use super::{LifecycleContext, LifecycleError};
use crate::state::{ResidencyState, TaskRecord, TaskStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachedRevivalRollbackResult {
    RolledBack,
    NotOwner,
}

pub fn rollback_detached_revival(
    context: &LifecycleContext,
    prior: &TaskRecord,
) -> Result<DetachedRevivalRollbackResult, LifecycleError> {
    let mut rolled_back = false;
    context.store.mutate(&prior.task_id, &mut |fresh| {
        let next_epoch = prior
            .notification
            .run_epoch
            .max(prior.burnt_epoch.unwrap_or(0.0) as i64)
            + 1;
        let claimed = fresh.status == prior.status
            && fresh.notification.run_epoch == prior.notification.run_epoch;
        let running =
            fresh.status == TaskStatus::Running && fresh.notification.run_epoch == next_epoch;
        if fresh.host_pid != Some(context.host_pid)
            || fresh.residency_state != ResidencyState::Resident
            || fresh.killed == Some(true)
            || (!claimed && !running)
        {
            return fresh.clone();
        }
        rolled_back = true;
        let mut next = fresh.clone();
        next.host_pid = None;
        next.status = prior.status;
        next.final_response.clone_from(&prior.final_response);
        next.error_message.clone_from(&prior.error_message);
        next.failure_kind.clone_from(&prior.failure_kind);
        next.failure_reason.clone_from(&prior.failure_reason);
        next.run_stats.clone_from(&prior.run_stats);
        next.prior_runs_cost_usd = prior.prior_runs_cost_usd;
        next.killed = prior.killed;
        next.run_start_epoch = prior.run_start_epoch;
        next.revive_delivery_uncertain = None;
        next.residency_state = prior.residency_state;
        next.notification.run_epoch = prior.notification.run_epoch;
        if running {
            next.burnt_epoch = Some(
                fresh
                    .burnt_epoch
                    .unwrap_or(0.0)
                    .max(fresh.notification.run_epoch as f64),
            );
        }
        next.updated_at = context.now_iso();
        next
    })?;
    Ok(if rolled_back {
        DetachedRevivalRollbackResult::RolledBack
    } else {
        DetachedRevivalRollbackResult::NotOwner
    })
}

pub fn revive_detached_terminal(
    context: &LifecycleContext,
    task_id: &str,
) -> Result<super::types::ReconcileOutcome, LifecycleError> {
    let Some(prior) = context.store.load(task_id)? else {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "record_disappeared",
        ));
    };
    if !prior.status.is_terminal()
        || prior.status == TaskStatus::Lost
        || prior.killed == Some(true)
        || !super::residency::is_suspended_residency(prior.residency_state)
    {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "task is not a continuable parked child",
        ));
    }
    if prior.isolation.is_some() {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "isolated_not_revivable",
        ));
    }
    if prior.host_pid.is_some_and(|pid| pid != context.host_pid) {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "foreign_owner",
        ));
    }
    let path = super::host_session::host_session_resume_path(&prior)
        .map(std::path::PathBuf::from)
        .or_else(|| super::reconcile::newest_session_path(context, task_id));
    let Some(path) = path else {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "task transcript is unavailable",
        ));
    };
    let Some(spec) = prior.spawn_spec.as_ref() else {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "persisted spawn spec unavailable",
        ));
    };
    let cwd = match spec {
        crate::state::TaskSpawnSpec::V1(spec) => &spec.cwd,
        crate::state::TaskSpawnSpec::LegacyProcess { cwd, .. } => cwd,
    };
    if !std::path::Path::new(cwd).is_dir() {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "cwd_unavailable",
        ));
    }
    let lease = super::admission_lease::acquire_session_admission_lease(
        context.store.state_dir(),
        &prior.parent_session_id,
        super::admission_lease::AdmissionLeaseTimingOverrides {
            acquire_timeout_ms: Some(0),
            ..Default::default()
        },
    )?;
    let super::admission_lease::AcquireAdmissionLeaseResult::Acquired(lease) = lease else {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "lock_contended",
        ));
    };
    if !lease.is_owner() {
        lease.release();
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "lease_lost",
        ));
    }
    let residents = context
        .store
        .list()?
        .records
        .into_iter()
        .filter(|r| {
            r.parent_session_id == prior.parent_session_id
                && r.residency_state == ResidencyState::Resident
        })
        .count();
    if matches!(context.config.residency_max_children, super::settings::ResidencyCap::Limited(limit) if limit > 0 && residents >= limit)
    {
        lease.release();
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "residency_capacity",
        ));
    }
    let mut claimed = false;
    let claim_result = context.store.mutate(task_id, &mut |fresh| {
        if fresh != &prior {
            return fresh.clone();
        }
        claimed = true;
        let mut next = fresh.clone();
        next.host_pid = Some(context.host_pid);
        next.residency_state = ResidencyState::Resident;
        next
    });
    lease.release();
    claim_result?;
    if !claimed {
        return Ok(super::types::ReconcileOutcome::deferred(
            task_id,
            "ownership_changed",
        ));
    }
    let fresh = match context.store.load(task_id)? {
        Some(fresh) => fresh,
        None => {
            return Ok(super::types::ReconcileOutcome::deferred(
                task_id,
                "record_disappeared",
            ));
        }
    };
    let result = super::reconcile_reclamation::revive_claimed(
        context,
        &fresh,
        prior.residency_state,
        Some(&path),
    );
    if result.as_ref().map_or(true, |outcome| {
        outcome.kind != super::types::ReconcileOutcomeKind::Resumed
    }) {
        rollback_detached_revival(context, &prior)?;
    }
    result
}
