//! Session-start reconciliation (`lifecycle/reconcile.ts`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::json;

use std::sync::Arc;

use super::context::{LifecycleContext, is_terminal, reconciliation_active};
use super::destroy::{destroy_resident_task, term_then_kill_claimed};
use super::errors::LifecycleError;
use super::host_session::host_session_identity;
use super::host_session_revive::reconcile_host_session_orphan;
use super::port::{
    DestroyCause, ReattachFailureKind, ReattachResult, RespawnResult, get_lifecycle_reattach_ports,
};
use super::provisional_exit::{reconcile_provisional_exit, settle_provisional_exit_loss};
use super::reconcile_crashed_resident::{lost_reason, reconcile_lost_event};
use super::reconcile_reclamation::{
    begin_local_reclamation, holds_claim, rollback_or_deferred, suspend_revived,
};
use super::reconcile_revival::reconcile_scoped_revival;
use super::residency::{
    BatchAdmissionOptions, ResidencyClaimResult, is_suspended_residency, reclaim_orphaned_resident,
};
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
    if !reconciliation_active(context) {
        return Ok(ReconcileResult::default());
    }
    // ONE daemon snapshot per pass: every host-session record is matched against it by path.
    context.host_session_probe.refresh(None);
    let mut groups: Vec<(String, Vec<TaskRecord>)> = Vec::new();
    for record in context.store.list()?.records {
        let key =
            host_session_identity(&record).map_or_else(String::new, |host| host.socket.clone());
        match groups.iter_mut().find(|(socket, _)| *socket == key) {
            Some((_, records)) => records.push(record),
            None => groups.push((key, vec![record])),
        }
    }
    // Admission leases cover only local claims. A host's probe/respawn never holds up another
    // host: each socket group reconciles on its own scoped thread.
    // PERF-DIVERGENCE(omo lifecycle/reconcile.ts): upstream awaits Promise.all over the groups;
    // the native probes block, so the groups run on scoped threads (inline when there is one).
    let results: Vec<Result<ReconcileResult, LifecycleError>> = if groups.len() <= 1 {
        groups
            .into_iter()
            .map(|(_, records)| reconcile_records(context, parent_session_id, records))
            .collect()
    } else {
        std::thread::scope(|scope| {
            let workers: Vec<_> = groups
                .into_iter()
                .map(|(_, records)| {
                    scope.spawn(move || reconcile_records(context, parent_session_id, records))
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        })
    };
    let mut outcomes = Vec::new();
    for result in results {
        outcomes.extend(result?.outcomes);
    }
    if !reconciliation_active(context) {
        return Ok(ReconcileResult::default());
    }
    reclaim_isolations(context);
    Ok(ReconcileResult { outcomes })
}

/// The probe says nothing about a claim installed while it was in flight, even at the same pid.
fn ownership_changed(fresh: Option<&TaskRecord>, record: &TaskRecord) -> bool {
    fresh.is_none_or(|fresh| {
        fresh.host_pid != record.host_pid
            || fresh.residency_claim != record.residency_claim
            || fresh.notification.run_epoch != record.notification.run_epoch
            || fresh.updated_at != record.updated_at
            || fresh.residency_state != record.residency_state
            || fresh.status != record.status
    })
}

fn reconcile_records(
    context: &LifecycleContext,
    parent_session_id: Option<&str>,
    records: Vec<TaskRecord>,
) -> Result<ReconcileResult, LifecycleError> {
    let mut outcomes = Vec::new();
    let mut candidates = Vec::new();
    let mut excluded = context.reconcile_admission.exclude_task_ids.clone();
    let mut owner_liveness: HashMap<i64, bool> = HashMap::new();
    let mut registry_ids: Option<HashSet<String>> = None;
    // Ownership is checked before terminality, residency, or mode. A live sibling owns the record
    // in every status and this process must not mutate it.
    for record in records {
        if !reconciliation_active(context) {
            return Ok(ReconcileResult { outcomes });
        }
        let foreign =
            has_foreign_live_owner(context, &record, parent_session_id, &mut owner_liveness);
        if !reconciliation_active(context) {
            return Ok(ReconcileResult { outcomes });
        }
        if ownership_changed(context.store.load(&record.task_id)?.as_ref(), &record) {
            excluded.insert(record.task_id.clone());
            outcomes.push(outcome(
                &record.task_id,
                ForeignLiveOwner,
                "ownership changed during probe",
            ));
            continue;
        }
        if foreign {
            // Scoped admission re-reads the store: carry the ownership exclusion into it too.
            excluded.insert(record.task_id.clone());
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
        if has_registered_handle(context, &record.task_id, &mut registry_ids) {
            if record.uncertain_inputs.is_some() {
                reconcile_inputs_in_background(context, &record.task_id);
            }
            outcomes.push(outcome(&record.task_id, Resumed, "owned by this process"));
            continue;
        }
        if record.provisional_exit.is_some()
            && !is_terminal(record.status)
            && !(parent_session_id.is_some_and(|parent| record.parent_session_id != parent)
                && record.host_pid == Some(context.host_pid))
        {
            outcomes.push(reconcile_provisional_exit(context, &record)?);
            if !reconciliation_active(context) {
                return Ok(ReconcileResult { outcomes });
            }
            excluded.insert(record.task_id.clone());
            continue;
        }
        candidates.push(record);
    }
    let Some(parent_session_id) = parent_session_id else {
        for record in &candidates {
            if !is_suspended_residency(record.residency_state) {
                outcomes.push(reconcile_legacy_record(context, record)?);
                if !reconciliation_active(context) {
                    return Ok(ReconcileResult { outcomes });
                }
            }
        }
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
        if !reconciliation_active(context) {
            return Ok(ReconcileResult { outcomes });
        }
    }
    // Scoped admission may only claim the snapshot this pass approved.
    let approved: HashSet<String> = candidates
        .iter()
        .map(|record| record.task_id.clone())
        .collect();
    let outer = context.reconcile_admission.clone();
    let mut scoped_context = context.clone();
    scoped_context.reconcile_admission = BatchAdmissionOptions {
        exclude_task_ids: excluded,
        can_claim: Some(Arc::new(move |fresh: &TaskRecord| {
            approved.contains(&fresh.task_id) && outer.allows(fresh)
        })),
        ..context.reconcile_admission.clone()
    };
    let scoped: Vec<TaskRecord> = candidates
        .into_iter()
        .filter(|record| record.parent_session_id == parent_session_id)
        .collect();
    let resolver = |task_id: &str| newest_session_path(context, task_id);
    outcomes.extend(reconcile_scoped_revival(
        &scoped_context,
        parent_session_id,
        &scoped,
        &resolver,
    )?);
    Ok(ReconcileResult { outcomes })
}

/// A live handle with inputs whose acknowledgement was lost re-reads them in the background:
/// each retry can wait out a full request deadline, and session start never holds on it.
fn reconcile_inputs_in_background(context: &LifecycleContext, task_id: &str) {
    let Some(handle) = context.registry.get(task_id) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("senpi-task-reconcile-inputs".to_string())
        .spawn(move || handle.reconcile_inputs());
    if let Err(error) = spawned {
        utils::logger::log(
            "senpi-task uncertain input reconcile failed",
            Some(&json!({ "taskId": task_id, "error": error.to_string() })),
        );
    }
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
    let _ = sweep_isolations(
        runtime,
        &records,
        std::sync::Arc::clone(&context.isolation_probe),
    );
}

pub(crate) fn reconcile_legacy_record(
    context: &LifecycleContext,
    observed: &TaskRecord,
) -> Result<ReconcileOutcome, LifecycleError> {
    if !reconciliation_active(context) {
        return Ok(ReconcileOutcome::deferred(
            &observed.task_id,
            "session_unavailable",
        ));
    }
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
    if record.provisional_exit.is_some() {
        return settle_provisional_exit_loss(context, &record);
    }
    // A daemon-hosted child has no pid at all. Its liveness is the daemon plus its session path,
    // and "the daemon is gone" parks it - the pid-shaped path below would mark it lost.
    if host_session_identity(&record).is_some() {
        return reconcile_host_session_orphan(context, &record);
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
    let terminated = term_then_kill_claimed(context, &record)?;
    if !reconciliation_active(context) {
        return Ok(rollback_or_deferred(
            context,
            &record.task_id,
            ResidencyState::RpcDetached,
            "session_unavailable",
            Some(&record),
        ));
    }
    let still_held = context
        .store
        .load(&record.task_id)?
        .is_some_and(|fresh| holds_claim(context, &fresh, Some(&record)));
    if !still_held {
        return Ok(ReconcileOutcome::deferred(
            &record.task_id,
            "foreign_live_owner",
        ));
    }
    if !terminated {
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
        term_then_kill_claimed(context, record)?;
        if !reconciliation_active(context) {
            return Ok(ReconcileOutcome::deferred(
                &record.task_id,
                "session_unavailable",
            ));
        }
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
    let owns = || {
        context
            .store
            .load(&record.task_id)
            .ok()
            .flatten()
            .is_some_and(|fresh| {
                fresh.host_pid == Some(context.host_pid)
                    && fresh.residency_claim == record.residency_claim
            })
    };
    let respawned = (ports.respawn)(record, Some(&session_path));
    if !reconciliation_active(context) || !owns() {
        if let RespawnResult::Ok(handle) = respawned {
            suspend_revived(context, record, handle, &owns);
        }
        return Ok(rollback_or_deferred(
            context,
            &record.task_id,
            ResidencyState::RpcDetached,
            "session_unavailable",
            Some(record),
        ));
    }
    let handle = match respawned {
        RespawnResult::Ok(handle) => handle,
        RespawnResult::Failed {
            code: super::port::RespawnFailureCode::RunPaused,
            ..
        } => {
            return Ok(ReconcileOutcome::deferred(&record.task_id, "run_paused"));
        }
        RespawnResult::Failed { reason, .. } => {
            mark_lost(context, record, &format!("reattach failed: {reason}"))?;
            return Ok(outcome(&record.task_id, Lost, &reason));
        }
    };
    let reattached = (ports.reattach)(record, Arc::clone(&handle));
    if !reconciliation_active(context) || !owns() {
        if reattached == ReattachResult::Ok {
            suspend_revived(context, record, handle, &owns);
        }
        return Ok(rollback_or_deferred(
            context,
            &record.task_id,
            ResidencyState::RpcDetached,
            "session_unavailable",
            Some(record),
        ));
    }
    match reattached {
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

/// Probes each distinct foreign owner pid once per pass: many records share one live owner. A
/// daemon-hosted child is owned while its session is live on the daemon; a pass that goes stale
/// between the two probes treats the record as foreign and touches nothing.
fn has_foreign_live_owner(
    context: &LifecycleContext,
    record: &TaskRecord,
    parent_session_id: Option<&str>,
    liveness: &mut HashMap<i64, bool>,
) -> bool {
    if let Some(host_session) = host_session_identity(record) {
        if record.residency_state != ResidencyState::Resident {
            return false;
        }
        let alive = context.host_session_probe.daemon_alive(host_session);
        if !reconciliation_active(context) {
            return true;
        }
        let session_live = alive && context.host_session_probe.session_live(host_session);
        if !reconciliation_active(context) {
            return true;
        }
        if !session_live {
            return false;
        }
        // The child's session outlives the process that owned its manager. When that owner is
        // dead, the parent session reopening elsewhere is the only one left to reclaim it.
        let dead_foreign_owner = record
            .host_pid
            .is_some_and(|pid| pid != context.host_pid && !context.signaller.is_alive(pid));
        return !(parent_session_id == Some(record.parent_session_id.as_str())
            && dead_foreign_owner);
    }
    record.host_pid.is_some_and(|pid| {
        pid != context.host_pid
            && *liveness
                .entry(pid)
                .or_insert_with(|| context.signaller.is_alive(pid))
    })
}

/// `has_live_handle` with the registry's `entries()` fallback snapshotted once per pass instead of
/// rebuilt for every record that misses the point lookup.
fn has_registered_handle(
    context: &LifecycleContext,
    task_id: &str,
    entry_ids: &mut Option<HashSet<String>>,
) -> bool {
    context.registry.get(task_id).is_some()
        || entry_ids
            .get_or_insert_with(|| {
                context
                    .registry
                    .entries()
                    .iter()
                    .map(|handle| handle.task_id().to_string())
                    .collect()
            })
            .contains(task_id)
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
