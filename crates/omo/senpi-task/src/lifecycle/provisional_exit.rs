//! `lifecycle/provisional-exit.ts`: an unconfirmed child exit whose owner is gone ends LOST.

use super::context::LifecycleContext;
use super::destroy::destroy_resident_task;
use super::errors::LifecycleError;
use super::port::DestroyCause;
use super::reconcile_crashed_resident::reconcile_lost_event;
use super::reconcile_reclamation::holds_claim;
use super::residency::{ResidencyClaimResult, claim_residency_slot};
use super::types::{ReconcileOutcome, ReconcileOutcomeKind};
use crate::state::{LostReason, TaskRecord, TaskStatus, mark_record_lost_for_reconciliation};
use crate::tools::control::cancel::with_dropped_steering_notice;

const OWNER_EXITED: &str = "owner exited before confirming the child's unexpected exit";

/// An unconfirmed exit is interrupted work, never a transcript to replay after losing its owner.
pub fn reconcile_provisional_exit(
    context: &LifecycleContext,
    observed: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    let observed_at = observed
        .provisional_exit
        .as_ref()
        .map(|exit| exit.observed_at.as_str());
    let claim = claim_residency_slot(context, &observed.task_id, |fresh| {
        matches!(fresh.status, TaskStatus::Running | TaskStatus::Pending)
            && fresh.host_pid == observed.host_pid
            && fresh.updated_at == observed.updated_at
            && fresh.residency_state == observed.residency_state
            && fresh.residency_claim == observed.residency_claim
            && fresh
                .provisional_exit
                .as_ref()
                .is_some_and(|exit| exit.run_epoch == fresh.notification.run_epoch)
            && fresh
                .provisional_exit
                .as_ref()
                .map(|exit| exit.observed_at.as_str())
                == observed_at
            && context.registry.get(&fresh.task_id).is_none()
            && fresh
                .host_pid
                .is_none_or(|pid| pid == context.host_pid || !context.signaller.is_alive(pid))
    });
    match claim {
        Ok(ResidencyClaimResult::Claimed) => {}
        Ok(ResidencyClaimResult::NotClaimable) => {
            return Ok(foreign(&observed.task_id, "provisional exit claim changed"));
        }
        Err(_) => {
            return Ok(foreign(
                &observed.task_id,
                "provisional exit lock contended",
            ));
        }
    }
    let Some(claimed) = context.store.load(&observed.task_id)? else {
        return Ok(ReconcileOutcome::new(
            &observed.task_id,
            ReconcileOutcomeKind::Resumed,
            Some("record removed"),
        ));
    };
    settle_provisional_exit_loss(context, &claimed)
}

/// Shared by startup and deferred revival after either path has claimed the record.
pub fn settle_provisional_exit_loss(
    context: &LifecycleContext,
    claimed: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    // Accepted conservative edge: an independent crash followed by parent death during confirmation
    // is LOST too. Only a manager that committed ERROR while still alive may establish failure.
    mark_lost(context, claimed, OWNER_EXITED)?;
    let lost = context
        .store
        .load(&claimed.task_id)?
        .is_some_and(|fresh| fresh.status == TaskStatus::Lost);
    Ok(if lost {
        ReconcileOutcome::new(
            &claimed.task_id,
            ReconcileOutcomeKind::Lost,
            Some("unconfirmed child exit after owner loss"),
        )
    } else {
        foreign(&claimed.task_id, "provisional exit claim changed")
    })
}

/// TS `markLost` (`revive-rollback.ts`): only while this pass still holds the claim it made.
fn mark_lost(
    context: &LifecycleContext,
    claimed: &TaskRecord,
    message: &str,
) -> Result<(), LifecycleError> {
    let claim = claimed.residency_claim.is_some().then_some(claimed);
    let timestamp = context.now_iso();
    let mut applied = false;
    context.store.mutate(&claimed.task_id, &mut |fresh| {
        if !holds_claim(context, fresh, claim) {
            return fresh.clone();
        }
        let dropped = fresh.pending_steering.as_ref().map_or(0, Vec::len);
        let reason = if fresh.status == TaskStatus::Lost {
            LostReason::Update
        } else {
            LostReason::KeepExisting
        };
        let result = mark_record_lost_for_reconciliation(
            fresh,
            &timestamp,
            &with_dropped_steering_notice(message, dropped),
            reason,
        );
        if !result.applied {
            return fresh.clone();
        }
        applied = true;
        result.record
    })?;
    if !applied {
        return Ok(());
    }
    context
        .store
        .append_event(&claimed.task_id, &reconcile_lost_event(message))?;
    // Rust has no `target_gone` destroy cause; `ReconcileLost` takes the same orphan teardown.
    destroy_resident_task(context, &claimed.task_id, DestroyCause::ReconcileLost, None)
}

fn foreign(task_id: &str, reason: &str) -> ReconcileOutcome {
    ReconcileOutcome::new(
        task_id,
        ReconcileOutcomeKind::ForeignLiveOwner,
        Some(reason),
    )
}
