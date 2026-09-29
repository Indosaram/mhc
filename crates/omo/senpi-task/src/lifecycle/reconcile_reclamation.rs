//! Orphaned-resident reclamation and claimed revival (`lifecycle/reconcile-reclamation.ts`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde_json::json;

use super::context::{LifecycleContext, is_terminal};
use super::destroy::{destroy_resident_task, term_then_kill};
use super::errors::LifecycleError;
use super::port::{
    DestroyCause, ReattachFailureKind, ReattachResult, RespawnDisposition, RespawnFailureCode,
    RespawnResult, get_lifecycle_reattach_ports,
};
use super::reconcile_crashed_resident::{lost_reason, mark_crashed_resident, reconcile_lost_event};
use super::residency::{ResidencyClaimResult, is_revivable_status, reclaim_orphaned_resident};
use super::types::{ReconcileOutcome, ReconcileOutcomeKind};
use crate::state::{
    ResidencyState, TaskRecord, TaskStatus, TaskTransition, is_spawn_spec_v1,
    mark_record_lost_for_reconciliation,
};
use crate::store::PersistedTaskEvent;

pub type SessionPathResolver<'a> = dyn Fn(&str) -> Option<PathBuf> + 'a;

const TERMINAL_DISPOSED: &str = "terminal without transcript disposed; persisted result preserved";

fn active_local_reclamations() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Mutex::default)
}

/// Guards a task against concurrent reclamation inside this process; released on drop.
pub struct LocalReclamation {
    key: String,
}

impl Drop for LocalReclamation {
    fn drop(&mut self) {
        active_local_reclamations()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

pub fn begin_local_reclamation(
    context: &LifecycleContext,
    task_id: &str,
) -> Option<LocalReclamation> {
    let key = format!("{}\u{0}{task_id}", context.store.state_dir().display());
    let inserted = active_local_reclamations()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key.clone());
    inserted.then_some(LocalReclamation { key })
}

pub fn deferred(task_id: &str, reason: &str) -> ReconcileOutcome {
    ReconcileOutcome::deferred(task_id, reason)
}

fn outcome(task_id: &str, kind: ReconcileOutcomeKind, reason: &str) -> ReconcileOutcome {
    ReconcileOutcome::new(task_id, kind, Some(reason))
}

pub fn reclaim_resident(
    context: &LifecycleContext,
    observed: &TaskRecord,
    session_path_for: &SessionPathResolver<'_>,
) -> Result<ReconcileOutcome, LifecycleError> {
    let Some(_guard) = begin_local_reclamation(context, &observed.task_id) else {
        return Ok(deferred(&observed.task_id, "foreign_live_owner"));
    };
    reclaim_resident_exclusive(context, observed, session_path_for)
}

fn reclaim_resident_exclusive(
    context: &LifecycleContext,
    observed: &TaskRecord,
    session_path_for: &SessionPathResolver<'_>,
) -> Result<ReconcileOutcome, LifecycleError> {
    match reclaim_orphaned_resident(context, observed) {
        Ok(ResidencyClaimResult::Claimed) => {}
        Ok(ResidencyClaimResult::NotClaimable) => {
            return Ok(deferred(&observed.task_id, "foreign_live_owner"));
        }
        Err(_) => return Ok(deferred(&observed.task_id, "lock_contended")),
    }
    let Some(claimed) = context.store.load(&observed.task_id)? else {
        return Ok(deferred(&observed.task_id, "foreign_live_owner"));
    };
    if claimed.host_pid != Some(context.host_pid)
        || claimed.residency_state != ResidencyState::Resident
    {
        return Ok(deferred(&observed.task_id, "foreign_live_owner"));
    }
    if claimed.killed == Some(true)
        || matches!(claimed.status, TaskStatus::Cancelled | TaskStatus::Lost)
    {
        destroy_resident_task(context, &claimed.task_id, DestroyCause::ReconcileLost, None)?;
        let kind = if claimed.status == TaskStatus::Lost {
            ReconcileOutcomeKind::Lost
        } else {
            ReconcileOutcomeKind::Resumed
        };
        let reason = if claimed.killed == Some(true) {
            "killed orphan disposed".to_string()
        } else {
            format!("{} orphan disposed", claimed.status.as_str())
        };
        return Ok(outcome(&claimed.task_id, kind, &reason));
    }
    if !is_revivable_status(claimed.status) {
        destroy_resident_task(context, &claimed.task_id, DestroyCause::ReconcileLost, None)?;
        return Ok(outcome(
            &claimed.task_id,
            ReconcileOutcomeKind::Resumed,
            "non-revivable orphan disposed",
        ));
    }
    let session_path = session_path_for(&claimed.task_id);
    if is_terminal(claimed.status) && session_path.is_none() {
        dispose(context, &claimed.task_id)?;
        return Ok(outcome(
            &claimed.task_id,
            ReconcileOutcomeKind::Resumed,
            TERMINAL_DISPOSED,
        ));
    }
    let rollback = if claimed.execution_mode == "process" {
        ResidencyState::RpcDetached
    } else {
        ResidencyState::PersistedOnly
    };
    if context.config.reattach_on_reconcile == Some(false) {
        let message = "reattach disabled for crashed resident";
        if mark_crashed_resident(context, &claimed, message)? {
            destroy_resident_task(context, &claimed.task_id, DestroyCause::ReconcileLost, None)?;
        }
        return Ok(outcome(
            &claimed.task_id,
            ReconcileOutcomeKind::Lost,
            message,
        ));
    }
    revive_claimed(context, &claimed, rollback, session_path.as_deref())
}

fn dispose(context: &LifecycleContext, task_id: &str) -> Result<(), LifecycleError> {
    context.store.transition(
        task_id,
        &TaskTransition::Dispose {
            timestamp: context.now_iso(),
        },
    )?;
    Ok(())
}

pub fn revive_claimed(
    context: &LifecycleContext,
    claimed: &TaskRecord,
    rollback: ResidencyState,
    session_path: Option<&Path>,
) -> Result<ReconcileOutcome, LifecycleError> {
    let fresh = context.store.load(&claimed.task_id)?;
    let Some(fresh) = fresh.filter(|fresh| {
        is_claim_held(context, fresh, &claimed.parent_session_id)
            && fresh.killed != Some(true)
            && is_revivable_status(fresh.status)
    }) else {
        return Ok(rollback_or_deferred(
            context,
            &claimed.task_id,
            rollback,
            "foreign_live_owner",
        ));
    };
    if fresh.execution_mode == "process"
        && let Some(pid) = fresh.pid
        && !term_then_kill(context, &fresh.task_id, pid)?
    {
        return Ok(rollback_or_deferred(
            context,
            &fresh.task_id,
            rollback,
            "session_unavailable",
        ));
    }
    if session_path.is_none() && !fresh.spawn_spec.as_ref().is_some_and(is_spawn_spec_v1) {
        if is_terminal(fresh.status) {
            dispose(context, &fresh.task_id)?;
            return Ok(outcome(
                &fresh.task_id,
                ReconcileOutcomeKind::Resumed,
                TERMINAL_DISPOSED,
            ));
        }
        mark_lost(
            context,
            &fresh,
            "record has neither a session transcript nor a persisted v1 spawn spec",
        )?;
        return Ok(outcome(
            &fresh.task_id,
            ReconcileOutcomeKind::Lost,
            "spawn spec unavailable",
        ));
    }
    let ports = context
        .reattach_ports
        .clone()
        .or_else(|| get_lifecycle_reattach_ports(context.store.state_dir()));
    let Some(ports) = ports else {
        mark_lost(context, &fresh, "reattach ports unavailable")?;
        return Ok(outcome(
            &fresh.task_id,
            ReconcileOutcomeKind::Lost,
            "reattach ports unavailable",
        ));
    };
    let handle = match (ports.respawn)(&fresh, session_path) {
        RespawnResult::Ok(handle) => handle,
        RespawnResult::Failed {
            disposition: RespawnDisposition::Retryable,
            code,
            ..
        } => {
            return Ok(rollback_or_deferred(
                context,
                &fresh.task_id,
                rollback,
                deferred_code(code),
            ));
        }
        RespawnResult::Failed { reason, .. } => {
            if is_terminal(fresh.status) {
                dispose(context, &fresh.task_id)?;
                return Ok(outcome(
                    &fresh.task_id,
                    ReconcileOutcomeKind::Resumed,
                    &reason,
                ));
            }
            mark_lost(context, &fresh, &format!("reattach failed: {reason}"))?;
            return Ok(outcome(&fresh.task_id, ReconcileOutcomeKind::Lost, &reason));
        }
    };
    match (ports.reattach)(&fresh, handle) {
        ReattachResult::Ok => {}
        ReattachResult::Failed {
            kind: ReattachFailureKind::AlreadyAttached,
            reason,
        } => {
            return Ok(outcome(
                &fresh.task_id,
                ReconcileOutcomeKind::Resumed,
                &reason,
            ));
        }
        ReattachResult::Failed { .. } => {
            return Ok(rollback_or_deferred(
                context,
                &fresh.task_id,
                rollback,
                "session_unavailable",
            ));
        }
    }
    let payload = match session_path {
        None => json!({ "fresh_launch": true }),
        Some(path) => json!({ "session_path": path.display().to_string() }),
    };
    context.store.append_event(
        &fresh.task_id,
        &PersistedTaskEvent {
            event_type: "reconcile_reattached".to_string(),
            payload,
        },
    )?;
    Ok(outcome(
        &fresh.task_id,
        ReconcileOutcomeKind::Resumed,
        "respawned and reattached",
    ))
}

fn rollback_or_deferred(
    context: &LifecycleContext,
    task_id: &str,
    residency: ResidencyState,
    success_reason: &str,
) -> ReconcileOutcome {
    if rollback_claim(context, task_id, residency) {
        deferred(task_id, success_reason)
    } else {
        deferred(task_id, "rollback_failed")
    }
}

fn rollback_claim(context: &LifecycleContext, task_id: &str, residency: ResidencyState) -> bool {
    let timestamp = context.now_iso();
    let result = context.store.mutate(task_id, &mut |fresh| {
        if fresh.host_pid != Some(context.host_pid)
            || fresh.residency_state != ResidencyState::Resident
        {
            return fresh.clone();
        }
        let mut next = fresh.clone();
        next.host_pid = None;
        if residency != ResidencyState::RpcDetached {
            next.pid = None;
        }
        next.residency_state = residency;
        next.updated_at.clone_from(&timestamp);
        next
    });
    match result {
        Ok(_) => true,
        Err(error) => {
            utils::logger::log(
                "senpi-task reconcile ownership rollback failed",
                Some(
                    &json!({ "taskId": task_id, "residency": residency.as_str(), "error": error.to_string() }),
                ),
            );
            false
        }
    }
}

fn mark_lost(
    context: &LifecycleContext,
    record: &TaskRecord,
    message: &str,
) -> Result<(), LifecycleError> {
    let mut applied = false;
    let timestamp = context.now_iso();
    context.store.mutate(&record.task_id, &mut |fresh| {
        if fresh.host_pid != Some(context.host_pid)
            || fresh.residency_state != ResidencyState::Resident
        {
            return fresh.clone();
        }
        let result =
            mark_record_lost_for_reconciliation(fresh, &timestamp, message, lost_reason(fresh));
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
        .append_event(&record.task_id, &reconcile_lost_event(message))?;
    destroy_resident_task(context, &record.task_id, DestroyCause::ReconcileLost, None)
}

pub fn is_orphan(context: &LifecycleContext, record: &TaskRecord) -> bool {
    if record.host_pid == Some(context.host_pid) {
        return !has_live_handle(context, &record.task_id);
    }
    record
        .host_pid
        .is_none_or(|pid| !context.signaller.is_alive(pid))
}

pub(crate) fn has_live_handle(context: &LifecycleContext, task_id: &str) -> bool {
    context.registry.get(task_id).is_some()
        || context
            .registry
            .entries()
            .iter()
            .any(|handle| handle.task_id() == task_id)
}

pub fn is_claim_held(
    context: &LifecycleContext,
    record: &TaskRecord,
    parent_session_id: &str,
) -> bool {
    record.parent_session_id == parent_session_id
        && record.residency_state == ResidencyState::Resident
        && record.host_pid == Some(context.host_pid)
}

fn deferred_code(code: RespawnFailureCode) -> &'static str {
    match code {
        RespawnFailureCode::RespawnFailed => "session_unavailable",
        other => other.as_str(),
    }
}
