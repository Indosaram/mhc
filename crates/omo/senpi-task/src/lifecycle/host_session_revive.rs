//! Reviving a child that lives on the shared daemon (`lifecycle/host-session-revive.ts`).
//!
//! A host session has no pid, so nothing here can (or may) signal: the only two answers are
//! "attach/reopen it" and "leave it parked". A parked record is NEVER lost - losing it would throw
//! away a transcript the daemon still owns.

use serde_json::json;

use super::context::{LifecycleContext, reconciliation_active};
use super::errors::LifecycleError;
use super::host_endpoint_reach::{parked_reason, reach_recorded_host};
use super::host_session::host_session_identity;
use super::host_session_record::{mark_suspension_reason, park_host_session_record};
use super::reconcile_reclamation::{deferred, holds_claim, revive_claimed, rollback_or_deferred};
use super::types::ReconcileOutcome;
use crate::state::{ResidencyState, TaskRecord};
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
