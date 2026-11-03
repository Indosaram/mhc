//! Orphaned-resident reclamation and claimed revival (`lifecycle/reconcile-reclamation.ts`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde_json::json;

use super::context::{LifecycleContext, is_terminal, reconciliation_active};
use super::destroy::{destroy_resident_task, term_then_kill_claimed};
use super::errors::LifecycleError;
use super::host_endpoint_reach::{RecordedHostVerdict, parked_reason, reach_recorded_host};
use super::host_session::host_session_identity;
use super::host_session::host_session_resume_path;
use super::pending_cancel::finish_pending_cancel;
use super::port::{
    DestroyCause, ReattachFailureKind, ReattachResult, ResidentHandle, ResidentKind,
    RespawnDisposition, RespawnFailureCode, RespawnResult, get_lifecycle_reattach_ports,
};
use super::reconcile_crashed_resident::{lost_reason, mark_crashed_resident, reconcile_lost_event};
use super::residency::{ResidencyClaimResult, is_revivable_status, reclaim_orphaned_resident};
use super::types::{ReconcileOutcome, ReconcileOutcomeKind};
use crate::host::HostError;
use crate::manager::child_handle::ManagedChildHandle;
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
    if !reconciliation_active(context) {
        return Ok(deferred(&observed.task_id, "session_unavailable"));
    }
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
    // A daemon-hosted child names its transcript on the record; the disk scan only knows the child's
    // own session dir, so preferring the record keeps a parked session from reading as transcript-less.
    let session_path = host_session_resume_path(&claimed)
        .map(PathBuf::from)
        .or_else(|| session_path_for(&claimed.task_id));
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
    let interrupted = |claim: &TaskRecord| -> Result<bool, LifecycleError> {
        Ok(!reconciliation_active(context) || !is_same_claim(context, claim)?)
    };
    if interrupted(claimed)? {
        return Ok(rollback_or_deferred(
            context,
            &claimed.task_id,
            rollback,
            "foreign_live_owner",
            Some(claimed),
        ));
    }
    if claimed.provisional_exit.is_some() && !is_terminal(claimed.status) {
        return super::provisional_exit::settle_provisional_exit_loss(context, claimed);
    }
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
            Some(claimed),
        ));
    };
    // An isolated record is NEVER revived: respawning the child against the parent checkout would
    // defeat the sandbox the caller relies on, so the record is marked lost - which makes its delta
    // salvageable by the crash-salvage pass - and the clone is left for that pass.
    if fresh.isolation.is_some() {
        mark_lost(context, &fresh, "isolated_not_revivable")?;
        return Ok(outcome(
            &fresh.task_id,
            ReconcileOutcomeKind::Lost,
            "isolated_not_revivable",
        ));
    }
    // A host session has no pid of its own; only the child-process runner ever leaves one behind.
    if fresh.execution_mode == "process"
        && fresh.pid.is_some()
        && host_session_identity(&fresh).is_none()
    {
        let terminated = term_then_kill_claimed(context, &fresh)?;
        if interrupted(&fresh)? {
            return Ok(rollback_or_deferred(
                context,
                &fresh.task_id,
                rollback,
                "foreign_live_owner",
                Some(&fresh),
            ));
        }
        if !terminated {
            return Ok(rollback_or_deferred(
                context,
                &fresh.task_id,
                rollback,
                "session_unavailable",
                Some(&fresh),
            ));
        }
    }
    // An accepted cancel is final: this claim finishes it rather than running the child again. A host
    // that does not confirm the session closed leaves the cancel pending for the next revival.
    if fresh.cancel_requested.is_some() && !is_terminal(fresh.status) {
        let finished = finish_pending_cancel(context, &fresh)?;
        return Ok(finished.unwrap_or_else(|| {
            rollback_or_deferred(
                context,
                &fresh.task_id,
                rollback,
                "host_unreachable",
                Some(&fresh),
            )
        }));
    }
    if session_path.is_none() {
        if is_terminal(fresh.status) {
            dispose(context, &fresh.task_id)?;
            return Ok(outcome(
                &fresh.task_id,
                ReconcileOutcomeKind::Resumed,
                TERMINAL_DISPOSED,
            ));
        }
        if !fresh.spawn_spec.as_ref().is_some_and(is_spawn_spec_v1) {
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
    }
    // Everything above can wait on the old child's termination: a stop or another owner that landed
    // meanwhile ends this revival here, so two owners never both respawn the same record.
    if interrupted(&fresh)? {
        return Ok(rollback_or_deferred(
            context,
            &fresh.task_id,
            rollback,
            "foreign_live_owner",
            Some(&fresh),
        ));
    }
    let reached = host_session_identity(&fresh).map_or(RecordedHostVerdict::Alive, |host| {
        reach_recorded_host(context, host)
    });
    if interrupted(&fresh)? {
        return Ok(rollback_or_deferred(
            context,
            &fresh.task_id,
            rollback,
            "foreign_live_owner",
            Some(&fresh),
        ));
    }
    if let Some(reason) = parked_reason(reached) {
        let outcome = rollback_or_deferred(
            context,
            &fresh.task_id,
            rollback,
            reached.as_str(),
            Some(&fresh),
        );
        super::host_session_record::mark_suspension_reason(context, &fresh.task_id, reason)?;
        return Ok(outcome);
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
    let respawned = respawn_through_drain(context, &ports.respawn, &fresh, session_path)?;
    if interrupted(&fresh)? {
        if let RespawnResult::Ok(handle) = respawned {
            let owns = || is_same_claim(context, &fresh).unwrap_or(false);
            suspend_revived(context, &fresh, handle, &owns);
        }
        return Ok(rollback_or_deferred(
            context,
            &fresh.task_id,
            rollback,
            "foreign_live_owner",
            Some(&fresh),
        ));
    }
    let handle = match respawned {
        RespawnResult::Ok(handle) => handle,
        RespawnResult::Failed {
            disposition: RespawnDisposition::Retryable,
            code,
            ..
        } => {
            if matches!(code, RespawnFailureCode::HostDraining { .. }) {
                context.store.mutate(&fresh.task_id, &mut |current| {
                    let mut next = current.clone();
                    if holds_claim(context, current, Some(&fresh)) {
                        next.suspension_reason = Some(crate::state::SuspensionReason::HostDraining);
                    }
                    next
                })?;
            }
            return Ok(rollback_or_deferred(
                context,
                &fresh.task_id,
                rollback,
                deferred_code(code),
                Some(&fresh),
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
    let reattached = (ports.reattach)(&fresh, Arc::clone(&handle));
    let owns_attached = || {
        context
            .store
            .load(&fresh.task_id)
            .ok()
            .flatten()
            .is_some_and(|current| {
                current.host_pid == Some(context.host_pid)
                    && current.residency_claim == fresh.residency_claim
            })
    };
    if !reconciliation_active(context) || !owns_attached() {
        if reattached == ReattachResult::Ok {
            suspend_revived(context, &fresh, handle, &owns_attached);
        }
        return Ok(rollback_or_deferred(
            context,
            &fresh.task_id,
            rollback,
            "session_unavailable",
            Some(&fresh),
        ));
    }
    match reattached {
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
                Some(&fresh),
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
    super::host_session_record::clear_suspension_reason(context, &fresh.task_id)?;
    Ok(outcome(
        &fresh.task_id,
        ReconcileOutcomeKind::Resumed,
        "respawned and reattached",
    ))
}

/// A revival or reattach that finished after the pass went stale, or after ownership moved, is
/// suspended without being attached: the registry, the record and the teardown bookkeeping are
/// touched only while `owns` still holds (`suspendHandle(..., "revived_after_shutdown", owns)`).
pub(crate) fn suspend_revived(
    context: &LifecycleContext,
    record: &TaskRecord,
    handle: Arc<dyn ManagedChildHandle>,
    owns: &dyn Fn() -> bool,
) {
    let kind = if record.execution_mode == "process" {
        ResidentKind::Rpc
    } else {
        ResidentKind::InProcess
    };
    let resident: Arc<dyn ResidentHandle> = Arc::new(RevivedResident { handle, kind });
    if let Err(error) =
        super::shutdown::suspend_handle_owned(context, &resident, "revived_after_shutdown", owns)
    {
        utils::logger::log(
            "senpi-task late revival suspension failed",
            Some(&json!({ "taskId": record.task_id, "error": error.to_string() })),
        );
    }
}

/// The freshly respawned handle seen through the residency port.
struct RevivedResident {
    handle: Arc<dyn ManagedChildHandle>,
    kind: ResidentKind,
}

impl ResidentHandle for RevivedResident {
    fn task_id(&self) -> &str {
        self.handle.task_id()
    }
    fn kind(&self) -> ResidentKind {
        self.kind
    }
    fn pid(&self) -> Option<i64> {
        self.handle.pid()
    }
    fn abort(&self) -> Result<(), HostError> {
        self.handle.abort()
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.handle.dispose()
    }
    fn terminate(&self) -> Result<(), HostError> {
        if self.kind == ResidentKind::InProcess || !self.handle.has_terminate() {
            return Ok(());
        }
        self.handle.terminate()
    }
    fn mark_stopping(&self) {
        self.handle.mark_stopping();
    }
    fn is_host_session(&self) -> bool {
        self.handle.is_host_session()
    }
    fn publish_task_status(&self, status: TaskStatus) -> Result<(), HostError> {
        self.handle.publish_task_status(status)
    }
}

pub(crate) fn rollback_or_deferred(
    context: &LifecycleContext,
    task_id: &str,
    residency: ResidencyState,
    success_reason: &str,
    claim: Option<&TaskRecord>,
) -> ReconcileOutcome {
    if rollback_claim(context, task_id, residency, claim) {
        deferred(task_id, success_reason)
    } else {
        deferred(task_id, "rollback_failed")
    }
}

/// Whether `fresh` is still held by the very claim `claim` took. Another revival in this process
/// (same host pid) that claimed the task since holds a different claim and is never undone here.
pub fn holds_claim(
    context: &LifecycleContext,
    fresh: &TaskRecord,
    claim: Option<&TaskRecord>,
) -> bool {
    if fresh.host_pid != Some(context.host_pid) || fresh.residency_state != ResidencyState::Resident
    {
        return false;
    }
    claim.is_none_or(|claim| {
        fresh.notification.run_epoch == claim.notification.run_epoch
            && fresh.residency_claim == claim.residency_claim
    })
}

fn rollback_claim(
    context: &LifecycleContext,
    task_id: &str,
    residency: ResidencyState,
    claim: Option<&TaskRecord>,
) -> bool {
    let timestamp = context.now_iso();
    let result = context.store.mutate(task_id, &mut |fresh| {
        if !holds_claim(context, fresh, claim) {
            return fresh.clone();
        }
        let mut next = fresh.clone();
        next.host_pid = None;
        next.residency_claim = None;
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
        return !has_live_handle(context, &record.task_id)
            && !context.registry.is_launching(&record.task_id);
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

fn is_same_claim(context: &LifecycleContext, claimed: &TaskRecord) -> Result<bool, LifecycleError> {
    let Some(now) = context.store.load(&claimed.task_id)? else {
        return Ok(false);
    };
    Ok(is_claim_held(context, &now, &claimed.parent_session_id)
        && now.killed != Some(true)
        && now.status == claimed.status
        && now.notification.run_epoch == claimed.notification.run_epoch
        && now.residency_claim == claimed.residency_claim)
}

fn respawn_through_drain(
    context: &LifecycleContext,
    respawn: &crate::lifecycle::port::RespawnPort,
    record: &TaskRecord,
    session_path: Option<&Path>,
) -> Result<RespawnResult, LifecycleError> {
    let max_drain_attempts = context.host_retry.max_drain_attempts;
    let default_retry_after_ms = context.host_retry.default_retry_after_ms;
    let mut result = respawn(record, session_path);
    for _attempt in 1..max_drain_attempts {
        if !reconciliation_active(context) || !is_same_claim(context, record)? {
            return Ok(result);
        }
        let retry_after_ms = match &result {
            RespawnResult::Failed {
                disposition: RespawnDisposition::Retryable,
                code: RespawnFailureCode::HostDraining { retry_after_ms },
                ..
            } => retry_after_ms.unwrap_or(default_retry_after_ms),
            _ => return Ok(result),
        };
        (context.host_retry_wait)(retry_after_ms);
        if !reconciliation_active(context) || !is_same_claim(context, record)? {
            return Ok(result);
        }
        result = respawn(record, session_path);
    }
    Ok(result)
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
