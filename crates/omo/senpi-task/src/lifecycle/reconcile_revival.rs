//! Scoped revival of this session's children (`lifecycle/reconcile-revival.ts`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use super::context::{LifecycleContext, is_terminal, reconciliation_active};
use super::errors::LifecycleError;
use super::host_session::host_session_resume_path;
use super::reconcile_reclamation::{
    SessionPathResolver, deferred, is_claim_held, is_orphan, reclaim_resident, revive_claimed,
};
use super::residency::{
    BatchAdmissionDeferral, BatchAdmissionOptions, BatchAdmissionOutcome, admit_suspended_batch,
    is_revivable_status, is_suspended_residency,
};
use super::types::{ReconcileOutcome, ReconcileOutcomeKind};
use crate::state::{ResidencyState, TaskRecord, TaskStatus};
use crate::store::StoreError;

enum Disposal {
    Disposed,
    NotApplied,
    LockContended,
}

pub fn reconcile_scoped_revival(
    context: &LifecycleContext,
    parent_session_id: &str,
    records: &[TaskRecord],
    session_path_for: &SessionPathResolver<'_>,
) -> Result<Vec<ReconcileOutcome>, LifecycleError> {
    if !reconciliation_active(context) || !context.config.resume_children {
        return Ok(Vec::new());
    }
    let mut outcomes = Vec::new();
    let session_records: Vec<&TaskRecord> = records
        .iter()
        .filter(|record| record.parent_session_id == parent_session_id)
        .collect();
    // Only the snapshot this pass observed may be claimed: a claim installed meanwhile (same
    // host pid included) changes residency_claim, run epoch or residency and is left alone.
    let expected: Arc<Mutex<HashMap<String, TaskRecord>>> = Arc::new(Mutex::new(
        session_records
            .iter()
            .filter(|record| is_suspended_residency(record.residency_state))
            .map(|record| (record.task_id.clone(), (*record).clone()))
            .collect(),
    ));
    let outer = context.reconcile_admission.clone();
    let snapshot = Arc::clone(&expected);
    let can_claim: super::residency::CanClaimFn = Arc::new(move |fresh: &TaskRecord| {
        let expected = snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        expected.get(&fresh.task_id).is_some_and(|observed| {
            outer.allows(fresh)
                && fresh.host_pid == observed.host_pid
                && fresh.residency_claim == observed.residency_claim
                && fresh.notification.run_epoch == observed.notification.run_epoch
                && fresh.residency_state == observed.residency_state
        })
    });
    for observed in &session_records {
        if observed.residency_state == ResidencyState::Resident && is_orphan(context, observed) {
            let reclaimed = reclaim_resident(context, observed, session_path_for)?;
            let resumed = reclaimed.kind == ReconcileOutcomeKind::Resumed;
            outcomes.push(reclaimed);
            if !reconciliation_active(context) {
                return Ok(outcomes);
            }
            // Our terminal reclamation can detach a transcript-bearing resident for admission
            // below. A lost ownership race never authorizes a fresh claim on the winner's record.
            if resumed
                && let Some(fresh) = context.store.load(&observed.task_id)?
                && is_suspended_residency(fresh.residency_state)
            {
                expected
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(fresh.task_id.clone(), fresh);
            }
        }
    }

    let mut excluded = context.reconcile_admission.exclude_task_ids.clone();
    for observed in &session_records {
        if !is_suspended_residency(observed.residency_state)
            || !is_terminal(observed.status)
            || observed.status == TaskStatus::Lost
            || observed.killed == Some(true)
            || transcript_for(observed, session_path_for).is_some()
        {
            continue;
        }
        excluded.insert(observed.task_id.clone());
        match dispose_suspended_terminal_without_transcript(context, observed) {
            Disposal::Disposed => outcomes.push(ReconcileOutcome::new(
                &observed.task_id,
                ReconcileOutcomeKind::Resumed,
                Some("terminal without transcript disposed; persisted result preserved"),
            )),
            Disposal::LockContended => outcomes.push(deferred(&observed.task_id, "lock_contended")),
            Disposal::NotApplied => {}
        }
    }

    let candidates: Vec<TaskRecord> = suspended_candidates(context, parent_session_id)?
        .into_iter()
        .filter(|record| !excluded.contains(&record.task_id) && can_claim.as_ref()(record))
        .collect();
    if context.config.reattach_on_reconcile == Some(false) {
        outcomes.extend(
            candidates
                .iter()
                .map(|record| deferred(&record.task_id, "reattach_disabled")),
        );
        record_deferrals(context, &outcomes);
        return Ok(outcomes);
    }
    let prior: HashMap<&str, ResidencyState> = candidates
        .iter()
        .map(|record| (record.task_id.as_str(), record.residency_state))
        .collect();
    let options = BatchAdmissionOptions {
        exclude_task_ids: excluded,
        can_claim: Some(can_claim),
        ..context.reconcile_admission.clone()
    };
    let admission = admit_suspended_batch(context, parent_session_id, &options)?;
    let mut reported = HashSet::new();
    for admitted in &admission.outcomes {
        reported.insert(admitted.task_id().to_string());
        let task_id = match admitted {
            BatchAdmissionOutcome::Deferred { task_id, reason } => {
                let reason = if *reason == BatchAdmissionDeferral::LeaseLost {
                    "lock_contended"
                } else {
                    reason.as_str()
                };
                outcomes.push(deferred(task_id, reason));
                continue;
            }
            BatchAdmissionOutcome::Claimed { task_id } => task_id,
        };
        let Some(prior_residency) = prior.get(task_id.as_str()).copied() else {
            outcomes.push(deferred(task_id, "foreign_live_owner"));
            continue;
        };
        let claimed = context
            .store
            .load(task_id)?
            .filter(|claimed| is_claim_held(context, claimed, parent_session_id));
        let Some(claimed) = claimed else {
            outcomes.push(deferred(task_id, "foreign_live_owner"));
            continue;
        };
        let session_path = transcript_for(&claimed, session_path_for);
        outcomes.push(revive_claimed(
            context,
            &claimed,
            prior_residency,
            session_path.as_deref(),
        )?);
    }
    for candidate in &candidates {
        if !reported.contains(&candidate.task_id) {
            outcomes.push(deferred(&candidate.task_id, "foreign_live_owner"));
        }
    }
    record_deferrals(context, &outcomes);
    Ok(outcomes)
}

/// The session is resumed now, so a child this pass deferred carries the deferral on its record.
/// A stale pass writes nothing, and a record the pass may not claim is not annotated.
fn record_deferrals(context: &LifecycleContext, outcomes: &[ReconcileOutcome]) {
    if !reconciliation_active(context) {
        return;
    }
    for outcome in outcomes {
        if outcome.kind != ReconcileOutcomeKind::Deferred {
            continue;
        }
        let Some(reason) = &outcome.reason else {
            continue;
        };
        match context.store.load(&outcome.task_id) {
            Ok(Some(fresh)) if context.reconcile_admission.allows(&fresh) => {}
            _ => continue,
        }
        if let Err(error) =
            super::host_session_record::mark_revival_deferred(context, &outcome.task_id, reason)
        {
            eprintln!(
                "senpi-task revival deferral not recorded: {}: {error}",
                outcome.task_id
            );
        }
    }
}

/// A daemon-hosted child NAMES its transcript on the record; the disk scan only knows the child's
/// own session dir, so preferring the record keeps a parked host session from reading as
/// transcript-less and being disposed (TS reconcile-revival.ts `transcriptFor`).
fn transcript_for(
    record: &TaskRecord,
    session_path_for: &SessionPathResolver<'_>,
) -> Option<std::path::PathBuf> {
    host_session_resume_path(record)
        .map(std::path::PathBuf::from)
        .or_else(|| session_path_for(&record.task_id))
}

fn dispose_suspended_terminal_without_transcript(
    context: &LifecycleContext,
    observed: &TaskRecord,
) -> Disposal {
    let mut applied = false;
    let timestamp = context.now_iso();
    let result = context.store.mutate(&observed.task_id, &mut |fresh| {
        if !reconciliation_active(context)
            || fresh.residency_claim != observed.residency_claim
            || fresh.host_pid != observed.host_pid
            || fresh.notification.run_epoch != observed.notification.run_epoch
        {
            return fresh.clone();
        }
        if !is_suspended_residency(fresh.residency_state) || !is_terminal(fresh.status) {
            return fresh.clone();
        }
        applied = true;
        let mut next = fresh.clone();
        next.host_pid = None;
        next.residency_state = ResidencyState::Disposed;
        next.updated_at.clone_from(&timestamp);
        next
    });
    match result {
        Err(_) => Disposal::LockContended,
        Ok(_) if applied => Disposal::Disposed,
        Ok(_) => Disposal::NotApplied,
    }
}

fn suspended_candidates(
    context: &LifecycleContext,
    parent_session_id: &str,
) -> Result<Vec<TaskRecord>, StoreError> {
    // PERF-DIVERGENCE(senpi task-record-store.ts): upstream lists every record then filters; this port filters inside the store so only matching records are cloned. Same records returned.
    Ok(context
        .store
        .list_matching(&mut |record| {
            record.parent_session_id == parent_session_id
                && is_suspended_residency(record.residency_state)
                && is_revivable_status(record.status)
                && record.killed != Some(true)
        })?
        .records)
}
