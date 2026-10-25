//! Durable suspension markers shared by shutdown and reconciliation.
use super::{LifecycleContext, LifecycleError};
use crate::state::{ResidencyState, SuspensionReason};

pub fn park_host_session_record(
    context: &LifecycleContext,
    task_id: &str,
) -> Result<(), LifecycleError> {
    context.store.mutate(task_id, &mut |fresh| {
        let mut next = fresh.clone();
        if next.residency_state != ResidencyState::RpcDetached || next.host_pid.is_some() {
            next.residency_state = ResidencyState::RpcDetached;
            next.host_pid = None;
            next.updated_at = context.now_iso();
        }
        next
    })?;
    Ok(())
}

pub fn mark_suspension_reason(
    context: &LifecycleContext,
    task_id: &str,
    reason: SuspensionReason,
) -> Result<(), LifecycleError> {
    context.store.mutate(task_id, &mut |fresh| {
        let mut next = fresh.clone();
        next.suspension_reason = Some(reason);
        next
    })?;
    Ok(())
}

pub fn clear_suspension_reason(
    context: &LifecycleContext,
    task_id: &str,
) -> Result<(), LifecycleError> {
    context.store.mutate(task_id, &mut |fresh| {
        let mut next = fresh.clone();
        next.suspension_reason = None;
        next.revival_deferred_reason = None;
        next.recovery_deadline_at = None;
        next
    })?;
    Ok(())
}

pub fn mark_revival_deferred(
    context: &LifecycleContext,
    task_id: &str,
    reason: &str,
) -> Result<(), LifecycleError> {
    context.store.mutate(task_id, &mut |fresh| {
        if !matches!(
            fresh.residency_state,
            ResidencyState::PersistedOnly | ResidencyState::RpcDetached
        ) || !matches!(
            fresh.suspension_reason,
            None | Some(SuspensionReason::RevivalDeferred | SuspensionReason::SessionQuit)
        ) || (fresh.suspension_reason == Some(SuspensionReason::RevivalDeferred)
            && fresh.revival_deferred_reason.as_deref() == Some(reason))
        {
            return fresh.clone();
        }
        let mut next = fresh.clone();
        next.suspension_reason = Some(SuspensionReason::RevivalDeferred);
        next.revival_deferred_reason = Some(reason.to_string());
        next
    })?;
    Ok(())
}
