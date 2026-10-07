//! Session-start reconciliation (`lifecycle/reconcile.ts`).

use std::path::PathBuf;

use serde_json::json;

use super::context::{LifecycleContext, is_terminal};
use super::destroy::{destroy_resident_task, term_then_kill};
use super::errors::LifecycleError;
use super::port::{
    DestroyCause, ReattachFailureKind, ReattachResult, RespawnResult, get_lifecycle_reattach_ports,
};
use super::reconcile_crashed_resident::{lost_reason, reconcile_lost_event};
use super::reconcile_reclamation::{begin_local_reclamation, has_live_handle};
use super::reconcile_revival::reconcile_scoped_revival;
use super::residency::{ResidencyClaimResult, is_suspended_residency, reclaim_orphaned_resident};
use super::ttl::parse_iso_ms;
use super::types::{ReconcileOutcome, ReconcileOutcomeKind, ReconcileResult};
use crate::isolation::{
    SalvagePorts, needs_crash_salvage, salvage_crashed_isolation, sweep_isolations,
};
use crate::state::{ResidencyState, TaskRecord, TaskStatus, mark_record_lost_for_reconciliation};
use crate::store::PersistedTaskEvent;

const HEARTBEAT_FRESH_MS: i64 = 30_000;

use ReconcileOutcomeKind::{ForeignLiveOwner, Lost, LostAndTerminated, Resumed};

fn outcome(task_id: &str, kind: ReconcileOutcomeKind, reason: &str) -> ReconcileOutcome {
    ReconcileOutcome::new(task_id, kind, Some(reason))
}

pub fn reconcile_on_session_start(
    context: &LifecycleContext,
    parent_session_id: Option<&str>,
) -> Result<ReconcileResult, LifecycleError> {
    let mut outcomes = Vec::new();
    let mut candidates = Vec::new();
    for record in context.store.list()?.records {
        if has_foreign_live_owner(context, &record) {
            outcomes.push(match parent_session_id {
                None => outcome(
                    &record.task_id,
                    ForeignLiveOwner,
                    &format!(
                        "child owned by live process pid={}",
                        record.host_pid.unwrap_or_default()
                    ),
                ),
                Some(_) => ReconcileOutcome::deferred(&record.task_id, "foreign_live_owner"),
            });
            continue;
        }
        if has_live_handle(context, &record.task_id) {
            outcomes.push(outcome(&record.task_id, Resumed, "owned by this process"));
            continue;
        }
        candidates.push(record);
    }
    let Some(parent_session_id) = parent_session_id else {
        for record in &candidates {
            if !is_suspended_residency(record.residency_state) {
                outcomes.push(reconcile_legacy_record(context, record)?);
            }
        }
        reclaim_isolations(context);
        return Ok(ReconcileResult { outcomes });
    };
    for record in &candidates {
        if record.parent_session_id == parent_session_id
            || record.residency_state != ResidencyState::Resident
        {
            continue;
        }
        if record.host_pid == Some(context.host_pid) {
            outcomes.push(ReconcileOutcome::deferred(
                &record.task_id,
                "foreign_live_owner",
            ));
            continue;
        }
        outcomes.push(reconcile_legacy_record(context, record)?);
    }
    let scoped: Vec<TaskRecord> = candidates
        .into_iter()
        .filter(|record| record.parent_session_id == parent_session_id)
        .collect();
    let resolver = |task_id: &str| newest_session_path(context, task_id);
    outcomes.extend(reconcile_scoped_revival(
        context,
        parent_session_id,
        &scoped,
        &resolver,
    )?);
    reclaim_isolations(context);
    Ok(ReconcileResult { outcomes })
}

/// Crash salvage FIRST, then the sweep: a dead host's clone must have its delta captured while the
/// clone still exists, and only then may the sweep reclaim clones whose owner is provably gone.
/// Reversing salvage and sweep would delete unreviewed work.
fn reclaim_isolations(context: &LifecycleContext) {
    let Some(runtime) = context.isolation.as_deref() else {
        return;
    };
    let Ok(listed) = context.store.list() else {
        return;
    };
    let records = listed.records;
    let terminal = |record: &TaskRecord| record.status.is_terminal();
    let mutate = |task_id: &str, mutation: &dyn Fn(&TaskRecord) -> TaskRecord| {
        let mut apply = |record: &TaskRecord| mutation(record);
        let _ = context.store.mutate(task_id, &mut apply);
    };
    for record in &records {
        if !needs_crash_salvage(record, &terminal) {
            continue;
        }
        salvage_crashed_isolation(
            &SalvagePorts {
                runtime,
                state_dir: context.store.state_dir(),
                mutate: &mutate,
            },
            record,
        );
    }
    let _ = sweep_isolations(runtime, &records, std::sync::Arc::clone(&context.isolation_probe));
}

fn reconcile_legacy_record(
    context: &LifecycleContext,
    observed: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    if observed.residency_state != ResidencyState::Resident {
        return reconcile_legacy_exclusive(context, observed);
    }
    let Some(_guard) = begin_local_reclamation(context, &observed.task_id) else {
        return Ok(outcome(
            &observed.task_id,
            ForeignLiveOwner,
            "orphan ownership claim in flight",
        ));
    };
    reconcile_legacy_exclusive(context, observed)
}

fn reload(context: &LifecycleContext, record: &TaskRecord) -> Result<TaskRecord, LifecycleError> {
    Ok(context
        .store
        .load(&record.task_id)?
        .unwrap_or_else(|| record.clone()))
}

fn reconcile_legacy_exclusive(
    context: &LifecycleContext,
    observed: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    let mut record = observed.clone();
    if record.residency_state == ResidencyState::Resident {
        match reclaim_orphaned_resident(context, observed) {
            Ok(ResidencyClaimResult::Claimed) => {}
            Ok(ResidencyClaimResult::NotClaimable) => {
                return Ok(outcome(
                    &observed.task_id,
                    ForeignLiveOwner,
                    "orphan ownership claim lost",
                ));
            }
            Err(_) => {
                return Ok(outcome(
                    &observed.task_id,
                    ForeignLiveOwner,
                    "orphan ownership lock contended",
                ));
            }
        }
        record = reload(context, &record)?;
    }
    if is_terminal(record.status) {
        return reconcile_legacy_terminal(context, &record);
    }
    if record.execution_mode != "process" {
        mark_lost(
            context,
            &record,
            "in-process task from a previous process cannot be reattached",
        )?;
        return Ok(outcome(
            &record.task_id,
            Lost,
            "previous-process in-process",
        ));
    }
    let Some(pid) = record.pid else {
        mark_lost(context, &record, "rpc task had no recorded pid")?;
        return Ok(outcome(&record.task_id, Lost, "no recorded pid"));
    };
    let alive = context.signaller.is_alive(pid);
    let session = record
        .child_session_id
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    if context.config.reattach_on_reconcile == Some(false) {
        if !alive {
            mark_lost(
                context,
                &record,
                &format!("rpc pid={pid} is dead; mapping exit facts only"),
            )?;
            return Ok(outcome(&record.task_id, Lost, &format!("dead pid {pid}")));
        }
        let heartbeat = heartbeat_state(context, &record);
        mark_lost(
            context,
            &record,
            &format!(
                "rpc orphan pid={pid} session={session} heartbeat={heartbeat}; reattach disabled, terminating orphan"
            ),
        )?;
        return Ok(outcome(
            &record.task_id,
            LostAndTerminated,
            &format!("live orphan, heartbeat={heartbeat}"),
        ));
    }
    let session_path = newest_session_path(context, &record.task_id);
    if !alive {
        if let Some(path) = session_path {
            return reattach_legacy_record(context, &record, path);
        }
        mark_lost(
            context,
            &record,
            &format!("rpc pid={pid} is dead; mapping exit facts only"),
        )?;
        return Ok(outcome(&record.task_id, Lost, &format!("dead pid {pid}")));
    }
    let heartbeat = heartbeat_state(context, &record);
    if !term_then_kill(context, &record.task_id, pid)? {
        mark_lost(
            context,
            &record,
            &format!("rpc orphan pid={pid} could not be terminated"),
        )?;
        return Ok(outcome(
            &record.task_id,
            LostAndTerminated,
            &format!("live orphan, heartbeat={heartbeat}"),
        ));
    }
    let Some(path) = session_path else {
        mark_lost(
            context,
            &record,
            &format!(
                "rpc orphan pid={pid} session={session} heartbeat={heartbeat}; terminating before reattach"
            ),
        )?;
        return Ok(outcome(
            &record.task_id,
            LostAndTerminated,
            &format!("live orphan, heartbeat={heartbeat}"),
        ));
    };
    let fresh = reload(context, &record)?;
    reattach_legacy_record(context, &fresh, path)
}

fn reconcile_legacy_terminal(
    context: &LifecycleContext,
    record: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    if matches!(record.status, TaskStatus::Lost | TaskStatus::Cancelled) {
        if record.residency_state == ResidencyState::Resident {
            destroy_resident_task(context, &record.task_id, DestroyCause::ReconcileLost, None)?;
        }
        let kind = if record.status == TaskStatus::Lost {
            Lost
        } else {
            Resumed
        };
        return Ok(outcome(
            &record.task_id,
            kind,
            &format!("already {}", record.status.as_str()),
        ));
    }
    let resumed = ReconcileOutcome::new(&record.task_id, Resumed, None);
    let pid = match record.pid {
        Some(pid)
            if record.execution_mode == "process"
                && record.residency_state == ResidencyState::Resident =>
        {
            pid
        }
        _ => return Ok(resumed),
    };
    let alive = context.signaller.is_alive(pid);
    let session_path = newest_session_path(context, &record.task_id);
    let reattach_enabled = context.config.reattach_on_reconcile != Some(false);
    if alive {
        term_then_kill(context, &record.task_id, pid)?;
        let Some(path) = session_path.filter(|_| reattach_enabled) else {
            destroy_resident_task(context, &record.task_id, DestroyCause::ReconcileLost, None)?;
            return Ok(outcome(
                &record.task_id,
                LostAndTerminated,
                &format!("terminal resident orphan pid {pid}"),
            ));
        };
        let fresh = reload(context, record)?;
        return reattach_legacy_record(context, &fresh, path);
    }
    match session_path {
        Some(path) if reattach_enabled => reattach_legacy_record(context, record, path),
        _ => Ok(resumed),
    }
}

fn reattach_legacy_record(
    context: &LifecycleContext,
    record: &TaskRecord,
    session_path: PathBuf,
) -> Result<ReconcileOutcome, LifecycleError> {
    let ports = context
        .reattach_ports
        .clone()
        .or_else(|| get_lifecycle_reattach_ports(context.store.state_dir()));
    let Some(ports) = ports else {
        mark_lost(context, record, "reattach ports unavailable")?;
        return Ok(outcome(&record.task_id, Lost, "reattach ports unavailable"));
    };
    let handle = match (ports.respawn)(record, Some(&session_path)) {
        RespawnResult::Ok(handle) => handle,
        RespawnResult::Failed { reason, .. } => {
            mark_lost(context, record, &format!("reattach failed: {reason}"))?;
            return Ok(outcome(&record.task_id, Lost, &reason));
        }
    };
    match (ports.reattach)(record, handle) {
        ReattachResult::Ok => {}
        ReattachResult::Failed {
            kind: ReattachFailureKind::AlreadyAttached,
            reason,
        } => return Ok(outcome(&record.task_id, Resumed, &reason)),
        ReattachResult::Failed { reason, .. } => {
            let fresh = reload(context, record)?;
            mark_lost(context, &fresh, &reason)?;
            return Ok(outcome(&record.task_id, Lost, &reason));
        }
    }
    context.store.append_event(
        &record.task_id,
        &PersistedTaskEvent {
            event_type: "reconcile_reattached".to_string(),
            payload: json!({ "session_path": session_path.display().to_string() }),
        },
    )?;
    Ok(outcome(
        &record.task_id,
        Resumed,
        "respawned and reattached",
    ))
}

fn has_foreign_live_owner(context: &LifecycleContext, record: &TaskRecord) -> bool {
    record
        .host_pid
        .is_some_and(|pid| pid != context.host_pid && context.signaller.is_alive(pid))
}

/// `<stateDir>/children/<id>/sessions/<id>/` (`resolveChildSessionDir` in `runners/rpc/spawn.ts`).
pub fn child_session_dir(state_dir: &std::path::Path, task_id: &str) -> PathBuf {
    state_dir
        .join("children")
        .join(task_id)
        .join("sessions")
        .join(task_id)
}

/// The newest `.jsonl` transcript by mtime (ties broken by the larger path).
pub fn newest_session_path(context: &LifecycleContext, task_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(child_session_dir(context.store.state_dir(), task_id)).ok()?;
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let newer = newest.as_ref().is_none_or(|(best_path, best_time)| {
            modified > *best_time || (modified == *best_time && path > *best_path)
        });
        if newer {
            newest = Some((path, modified));
        }
    }
    newest.map(|(path, _)| path)
}

fn heartbeat_state(context: &LifecycleContext, record: &TaskRecord) -> &'static str {
    match parse_iso_ms(&record.updated_at) {
        Some(updated) if (context.now)() - updated < HEARTBEAT_FRESH_MS => "fresh",
        _ => "stale",
    }
}

fn mark_lost(
    context: &LifecycleContext,
    record: &TaskRecord,
    message: &str,
) -> Result<(), LifecycleError> {
    let result = mark_record_lost_for_reconciliation(
        record,
        &context.now_iso(),
        message,
        lost_reason(record),
    );
    if result.applied {
        context.store.replace(&result.record)?;
        context
            .store
            .append_event(&record.task_id, &reconcile_lost_event(message))?;
    }
    if record.residency_state == ResidencyState::Resident {
        destroy_resident_task(context, &record.task_id, DestroyCause::ReconcileLost, None)?;
    }
    Ok(())
}
