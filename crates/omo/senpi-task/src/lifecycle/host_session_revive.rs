//! Reviving a child that lives on the shared daemon (`lifecycle/host-session-revive.ts`).
//!
//! A host session has no pid, so nothing here can (or may) signal: the only two answers are
//! "attach/reopen it" and "leave it parked". A parked record is NEVER lost - losing it would throw
//! away a transcript the daemon still owns.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::context::{LifecycleContext, reconciliation_active};
use super::errors::LifecycleError;
use super::host_endpoint_reach::{RecordedHostVerdict, parked_reason, reach_recorded_host};
use super::host_session::{
    host_session_identity, host_session_resume_path, is_host_session_record,
};
use super::host_session_record::{mark_suspension_reason, park_host_session_record};
use super::reconcile_reclamation::{deferred, holds_claim, revive_claimed, rollback_or_deferred};
use super::types::ReconcileOutcome;
use crate::state::{ResidencyState, SuspensionReason, TaskRecord, TaskStatus};
use crate::store::PersistedTaskEvent;

/// The reconciliation branch for a daemon-hosted orphan: reach the RECORDED endpoint (re-ensuring
/// it unless it is this session's own), then a live session is re-attached and a parked one
/// reopened from its JSONL there. An endpoint that stays out of reach leaves the record suspended
/// with the reason. A reach that returns after the pass went stale, or after another claim
/// replaced this one, hands the claim back instead of parking or reviving.
pub fn reconcile_host_session_orphan(
    context: &LifecycleContext,
    record: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    let Some(host_session) = host_session_identity(record) else {
        return Ok(deferred(&record.task_id, "session_unavailable"));
    };
    let verdict = reach_recorded_host(context, host_session);
    let fresh = context.store.load(&record.task_id)?;
    if !reconciliation_active(context)
        || !fresh
            .as_ref()
            .is_some_and(|fresh| holds_claim(context, fresh, Some(record)))
    {
        return Ok(rollback_or_deferred(
            context,
            &record.task_id,
            ResidencyState::RpcDetached,
            "foreign_live_owner",
            Some(record),
        ));
    }
    if let Some(reason) = parked_reason(verdict) {
        park_host_session_record(context, &record.task_id)?;
        mark_suspension_reason(context, &record.task_id, reason)?;
        context.store.append_event(
            &record.task_id,
            &PersistedTaskEvent {
                event_type: "suspended".to_string(),
                payload: json!({ "reason": serde_json::to_value(reason).unwrap_or_default() }),
            },
        )?;
        return Ok(deferred(&record.task_id, verdict.as_str()));
    }
    let session_path = std::path::PathBuf::from(&host_session.session_path);
    revive_claimed(
        context,
        record,
        ResidencyState::RpcDetached,
        Some(session_path.as_path()),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostSessionParkOutcome {
    Revived,
    Suspended { reason: SuspensionReason },
}

pub type BeforeAttemptFn = Arc<dyn Fn(usize) + Send + Sync>;

#[derive(Default, Clone)]
pub struct HostSessionParkOptions {
    pub before_attempt: Option<BeforeAttemptFn>,
}

impl HostSessionParkOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_before_attempt(before_attempt: impl Fn(usize) + Send + Sync + 'static) -> Self {
        Self {
            before_attempt: Some(Arc::new(before_attempt)),
        }
    }
}

pub fn park_host_session_on_daemon_loss(
    context: &LifecycleContext,
    task_id: &str,
    options: Option<HostSessionParkOptions>,
) -> Result<HostSessionParkOutcome, LifecycleError> {
    let parked = context.store.load(task_id)?;
    let Some(parked) = parked.filter(is_host_session_record) else {
        return Ok(HostSessionParkOutcome::Suspended {
            reason: SuspensionReason::DaemonUnavailable,
        });
    };
    let host_session = host_session_identity(&parked)
        .expect("is_host_session_record guarantees host_session is present")
        .clone();

    context.registry.forget(task_id);
    park_host_session_record(context, task_id)?;
    context.store.append_event(
        task_id,
        &PersistedTaskEvent {
            event_type: "suspended".to_string(),
            payload: json!({ "reason": "daemon_unavailable" }),
        },
    )?;

    let mut attempt = 0;
    let mut verdict = RecordedHostVerdict::HostUnreachable;
    for backoff_ms in &context.host_retry.daemon_loss_backoff_ms {
        attempt += 1;
        (context.host_retry_wait)(*backoff_ms);
        if !reconciliation_active(context)
            || super::deferred_revival::retries_stopped(context, &parked.parent_session_id)
        {
            return Ok(HostSessionParkOutcome::Suspended {
                reason: SuspensionReason::DaemonUnavailable,
            });
        }
        if let Some(before_attempt) = options
            .as_ref()
            .and_then(|options| options.before_attempt.as_ref())
        {
            before_attempt(attempt);
        }
        context
            .host_session_probe
            .refresh(Some(&host_session.socket));
        verdict = reach_recorded_host(context, &host_session);
        if verdict == RecordedHostVerdict::HostIncompatible {
            break;
        }
        if verdict != RecordedHostVerdict::Alive {
            continue;
        }
        if revive_parked_host_session(context, task_id) {
            return Ok(HostSessionParkOutcome::Revived);
        }
    }
    let reason = if verdict == RecordedHostVerdict::Alive {
        SuspensionReason::DaemonUnavailable
    } else {
        parked_reason(verdict).unwrap_or(SuspensionReason::DaemonUnavailable)
    };
    mark_suspension_reason(context, task_id, reason)?;
    Ok(HostSessionParkOutcome::Suspended { reason })
}

fn revive_parked_host_session(context: &LifecycleContext, task_id: &str) -> bool {
    let claimed = super::residency::claim_residency_slot(context, task_id, |fresh| {
        fresh.residency_state == ResidencyState::RpcDetached
            && fresh.killed != Some(true)
            && !super::deferred_revival::retries_stopped(context, &fresh.parent_session_id)
    });
    if !matches!(claimed, Ok(super::residency::ResidencyClaimResult::Claimed)) {
        return false;
    }
    let fresh = match context.store.load(task_id) {
        Ok(Some(fresh)) => fresh,
        _ => return false,
    };
    let session_path = host_session_resume_path(&fresh).map(std::path::PathBuf::from);
    match revive_claimed(
        context,
        &fresh,
        ResidencyState::RpcDetached,
        session_path.as_deref(),
    ) {
        Ok(outcome) => outcome.kind == super::types::ReconcileOutcomeKind::Resumed,
        Err(error) => {
            utils::logger::log(
                "senpi-task host session revive after daemon loss failed",
                Some(&serde_json::json!({
                    "taskId": task_id,
                    "error": error.to_string(),
                })),
            );
            false
        }
    }
}

pub const DEFERRED_HOST_REASONS: &[&str] = &["host_unreachable", "host_draining"];

pub fn retry_deferred_host_sessions(
    context: &LifecycleContext,
    outcomes: &[ReconcileOutcome],
    parent_session_id: Option<&str>,
) {
    for outcome in outcomes {
        if outcome.kind != super::types::ReconcileOutcomeKind::Deferred {
            continue;
        }
        let Some(reason) = outcome.reason.as_deref() else {
            continue;
        };
        let is_host_reason = DEFERRED_HOST_REASONS.contains(&reason);
        let is_scoped_reason =
            super::deferred_revival_reasons::SCOPED_RETRY_REASONS.contains(&reason);
        if !is_host_reason && !is_scoped_reason {
            continue;
        }
        let record = match context.store.load(&outcome.task_id) {
            Ok(Some(record)) => record,
            _ => continue,
        };
        let is_host = is_host_reason && is_host_session_record(&record);
        let is_scoped = is_scoped_reason
            && parent_session_id.is_some_and(|parent| record.parent_session_id == parent);
        if !is_host && !is_scoped {
            continue;
        }
        if !context.scoped_retries.try_start_retry(&outcome.task_id) {
            continue;
        }
        let retry_context = context.clone();
        let retry_task = outcome.task_id.clone();
        let reason_str = reason.to_string();
        let parent = parent_session_id.map(str::to_string);
        let spawned = std::thread::Builder::new()
            .name("senpi-task-deferred-host-retry".into())
            .spawn(move || {
                if is_scoped {
                    if let Some(parent) = parent
                        && let Err(error) = super::deferred_revival::retry_deferred_scoped_child(
                            &retry_context,
                            &retry_task,
                            &parent,
                            &reason_str,
                        )
                    {
                        utils::logger::log(
                            "senpi-task scoped revival retry failed",
                            Some(&serde_json::json!({
                                "taskId": retry_task,
                                "error": error.to_string(),
                            })),
                        );
                    }
                } else {
                    retry_deferred_host_session(&retry_context, &retry_task);
                }
                retry_context.scoped_retries.finish_retry(&retry_task);
            });
        if let Err(error) = spawned {
            context.scoped_retries.finish_retry(&outcome.task_id);
            utils::logger::log(
                "senpi-task deferred host session retry spawn failed",
                Some(&serde_json::json!({
                    "taskId": outcome.task_id,
                    "error": error.to_string(),
                })),
            );
        }
    }
}

pub fn retry_deferred_host_session(context: &LifecycleContext, task_id: &str) {
    for backoff_ms in &context.host_retry.deferred_retry_backoff_ms {
        (context.host_retry_wait)(*backoff_ms);
        if context.scoped_retries.is_disposed() || !reconciliation_active(context) {
            return;
        }
        let fresh = match context.store.load(task_id) {
            Ok(Some(record)) => record,
            _ => return,
        };
        if !is_host_session_record(&fresh)
            || fresh.status != TaskStatus::Running
            || fresh.residency_state != ResidencyState::RpcDetached
            || fresh.killed == Some(true)
            || super::deferred_revival::retries_stopped(context, &fresh.parent_session_id)
        {
            return;
        }
        if let Some(host) = host_session_identity(&fresh) {
            context.host_session_probe.refresh(Some(&host.socket));
        }
        if revive_parked_host_session(context, task_id) {
            return;
        }
    }
}
