//! `dag/recovery.ts`: crash-safety boundary - lease claiming, node reconciliation, and resumed
//! wave admission stay on one contract.
// allow: SIZE_OK - recovery keeps lease claiming, node reconciliation, and resumed wave admission
// in one crash-safety boundary.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::dag::fingerprint::dag_fingerprint;
use crate::dag::journal::{DagJournal, DagJournalOptions, create_dag_journal};
use crate::dag::manager::{DagPersistedNode, DagRunRecordV1};
use crate::dag::owner::{DagOwnerKind, DagTaskOwner};
use crate::dag::results::{DagNodeResultReadInput, read_dag_node_result};
use crate::dag::scheduler::{
    DagNodeSpawnPolicy, DagSchedulerAncestry, DagSchedulerOptions,
    apply_dag_scheduler_event_with_results_for_recovery, create_dag_scheduler,
};
use crate::dag::store::DagFileStore;
use crate::dag::types::{
    DagNode, DagNodeError, DagNodeErrorCode, DagNodeId, DagNodeState, DagNodeTransitionReason,
    DagRoute, DagRunEvent, DagRunEventPayload, DagRunId, DagRunStatus,
};
use crate::manager::types::{ManagerStartSpec, OwnedStartResult, StartFailure, StartResult};
use crate::manager::TaskManager;
use crate::state::{TaskRecord, TaskStatus};

fn is_live_run_status(status: DagRunStatus) -> bool {
    matches!(status, DagRunStatus::Pending | DagRunStatus::Running)
}

/// Injected seams and shared pending-state handles threaded through the recovery boundary.
type LivenessProbe = Arc<dyn Fn(i64) -> bool + Send + Sync>;
type StopAdmission = Arc<dyn Fn(&DagRunId) + Send + Sync>;
type Reattach = Arc<dyn Fn(&DagRunId, &str) + Send + Sync>;
type HeldInProcess = Arc<dyn Fn(&DagRunId) -> bool + Send + Sync>;
type PendingErrors = Arc<Mutex<BTreeMap<DagNodeId, DagNodeError>>>;
type PendingTerminalResults = Arc<Mutex<BTreeMap<DagNodeId, TaskRecord>>>;
type PendingRecoveryState = (PendingErrors, PendingTerminalResults);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagRecoverySkipReason {
    ForeignSession,
    LiveLease,
    NotPaused,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DagRecoveryOutcome {
    Resumed {
        run_id: DagRunId,
        record: Box<DagRunRecordV1>,
        reused_outputs: HashMap<DagNodeId, String>,
    },
    Skipped {
        run_id: DagRunId,
        reason: DagRecoverySkipReason,
        holder_pid: Option<i64>,
    },
    Adopted { run_id: DagRunId, record: Box<DagRunRecordV1>, reused_outputs: HashMap<DagNodeId, String> },
}

/// `RecoverableRecord`: the journal checkpoint extended with lease-holder bookkeeping the
/// recovery boundary owns exclusively; no other module reads or writes these two fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverableRecord {
    #[serde(flatten)]
    pub record: DagRunRecordV1,
    #[serde(default)]
    pub lease_holder_pid: Option<i64>,
    #[serde(default)]
    pub previous_lease_holder_pid: Option<i64>,
}

impl crate::dag::journal::DagJournalCheckpoint for RecoverableRecord {
    fn checkpoint_seq(&self) -> u64 {
        self.record.checkpoint_seq
    }

    fn with_checkpoint_seq(&self, checkpoint_seq: u64) -> Self {
        Self {
            record: DagRunRecordV1 {
                checkpoint_seq,
                ..self.record.clone()
            },
            ..self.clone()
        }
    }
}

pub struct DagRecoveryOptions {
    pub store: Arc<DagFileStore>,
    pub task_manager: Arc<TaskManager>,
    pub host_pid: Option<i64>,
    /// Liveness probe for a paused run's previous lease holder; defaults to the shared allowlisted
    /// signal-0 probe so this module never invokes a process signal directly.
    pub is_process_alive: Option<LivenessProbe>,
    pub is_run_held_in_process: Option<HeldInProcess>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub subscriber_ring: Option<usize>,
    pub stop_admission: Option<StopAdmission>,
    pub reattach: Option<Reattach>,
    pub ancestry: Option<DagSchedulerAncestry>,
    pub node_spawn_policy: Option<DagNodeSpawnPolicy>,
}

struct RecoveryContext {
    store: Arc<DagFileStore>,
    task_manager: Arc<TaskManager>,
    host_pid: i64,
    is_process_alive: LivenessProbe,
    is_run_held_in_process: Option<HeldInProcess>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    subscriber_ring: Option<usize>,
    stop_admission: Option<StopAdmission>,
    reattach: Option<Reattach>,
    ancestry: Option<DagSchedulerAncestry>,
    node_spawn_policy: Option<DagNodeSpawnPolicy>,
}

pub struct DagRecovery {
    context: Arc<RecoveryContext>,
}

pub fn create_dag_recovery(options: DagRecoveryOptions) -> DagRecovery {
    let host_pid = options.host_pid.unwrap_or_else(|| i64::from(std::process::id()));
    let is_process_alive = options
        .is_process_alive
        .unwrap_or_else(|| Arc::new(crate::dag::store::default_is_process_alive));
    let now = options
        .now
        .unwrap_or_else(|| Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX)));
    DagRecovery {
        context: Arc::new(RecoveryContext {
            store: options.store,
            task_manager: options.task_manager,
            host_pid,
            is_process_alive,
            is_run_held_in_process: options.is_run_held_in_process.clone(),
            now,
            subscriber_ring: options.subscriber_ring,
            stop_admission: options.stop_admission,
            reattach: options.reattach,
            ancestry: options.ancestry,
            node_spawn_policy: options.node_spawn_policy,
        }),
    }
}

impl DagRecovery {
    pub fn try_pause_runs_for_shutdown(&self, parent_session_id: &str) -> Result<Vec<DagRunId>, crate::dag::store::DagStoreError> {
        pause_runs_for_shutdown(&self.context, parent_session_id)
    }
    pub fn pause_runs_for_shutdown(&self, parent_session_id: &str) -> Vec<DagRunId> {
        self.try_pause_runs_for_shutdown(parent_session_id).expect("DAG shutdown pause persisted")
    }

    pub fn resume_paused_runs(&self, parent_session_id: &str) -> Vec<DagRecoveryOutcome> {
        self.resume_paused_runs_from(parent_session_id, None)
    }
    pub fn resume_paused_runs_from(&self, parent_session_id: &str, fork_source_session_id: Option<&str>) -> Vec<DagRecoveryOutcome> {
        resume_paused_runs(&self.context, parent_session_id, fork_source_session_id)
    }
}

fn pause_runs_for_shutdown(context: &RecoveryContext, parent_session_id: &str) -> Result<Vec<DagRunId>, crate::dag::store::DagStoreError> {
    let mut paused = Vec::new();
    for observed in list_run_records(&context.store) {
        if observed.record.parent_session_id != parent_session_id
            || !is_live_run_status(observed.record.status)
        {
            continue;
        }
        if let Some(stop_admission) = &context.stop_admission {
            stop_admission(&observed.record.run_id);
        }
        let journal = recovery_journal(context, observed.clone(), None);
        journal.append(DagRunEventPayload::RunPaused {
            reason: Some("session_shutdown".to_string()),
        })?;
        let run_id = observed.record.run_id.clone();
        context.store.with_run_lock(&run_id, || -> Result<(), crate::dag::store::DagStoreError> {
            let Some(fresh) = context.store.read_checkpoint::<RecoverableRecord>(&run_id)? else {
                return Ok(());
            };
            if fresh.record.status != DagRunStatus::Paused {
                return Ok(());
            }
            let previous_lease_holder_pid = fresh.lease_holder_pid.or(Some(context.host_pid));
            let released = RecoverableRecord {
                lease_holder_pid: None,
                previous_lease_holder_pid,
                ..fresh
            };
            context.store.write_checkpoint(&run_id, &released)
        })??;
        paused.push(run_id);
    }
    Ok(paused)
}

fn resume_paused_runs(context: &RecoveryContext, parent_session_id: &str, fork_source: Option<&str>) -> Vec<DagRecoveryOutcome> {
    let mut outcomes = Vec::new();
    for observed in list_run_records(&context.store) {
        let foreign = observed.record.parent_session_id != parent_session_id;
        let source = fork_source.map(str::trim).filter(|s| !s.is_empty());
        if foreign && source != Some(observed.record.parent_session_id.trim()) {
            continue;
        }
        let run_id = observed.record.run_id.clone();
        match if foreign { claim_orphaned_run(context, &observed, parent_session_id) } else { claim_paused_run(context, &run_id, parent_session_id) } {
            ClaimedRun::Skipped(DagRecoverySkipReason::LiveLease, holder_pid) if !foreign => {
                outcomes.push(DagRecoveryOutcome::Skipped {
                    run_id,
                    reason: DagRecoverySkipReason::LiveLease,
                    holder_pid,
                });
            }
            ClaimedRun::Skipped(DagRecoverySkipReason::LiveLease, _) => {}
            ClaimedRun::Skipped(_, _) => {}
            ClaimedRun::Claimed(claimed) => {
                let mut outcome = resume_claimed_run(context, *claimed);
                if foreign && let DagRecoveryOutcome::Resumed { run_id, record, reused_outputs } = outcome {
                    outcome = DagRecoveryOutcome::Adopted { run_id, record, reused_outputs };
                }
                outcomes.push(outcome);
            }
        }
    }
    outcomes
}

enum ClaimedRun {
    Claimed(Box<RecoverableRecord>),
    Skipped(DagRecoverySkipReason, Option<i64>),
}

fn claim_paused_run(context: &RecoveryContext, run_id: &DagRunId, parent_session_id: &str) -> ClaimedRun {
    context
        .store
        .with_run_lock(run_id, || {
            let Ok(Some(fresh)) = context.store.read_checkpoint::<RecoverableRecord>(run_id) else {
                return ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None);
            };
            if fresh.record.status != DagRunStatus::Paused {
                return ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None);
            }
            if fresh.record.parent_session_id != parent_session_id {
                return ClaimedRun::Skipped(DagRecoverySkipReason::ForeignSession, None);
            }
            let prior_holder = fresh.lease_holder_pid.or(fresh.previous_lease_holder_pid);
            if let Some(pid) = prior_holder
                && holder_still_live(context, run_id, pid)
            {
                return ClaimedRun::Skipped(DagRecoverySkipReason::LiveLease, Some(pid));
            }
            let claimed = RecoverableRecord {
                lease_holder_pid: Some(context.host_pid),
                previous_lease_holder_pid: None,
                ..fresh
            };
            if context.store.write_checkpoint(run_id, &claimed).is_err() {
                return ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None);
            }
            ClaimedRun::Claimed(Box::new(claimed))
        })
        .unwrap_or(ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None))
}

fn holder_still_live(context: &RecoveryContext, run_id: &DagRunId, pid: i64) -> bool {
    if pid == context.host_pid { context.is_run_held_in_process.as_ref().is_some_and(|held| held(run_id)) }
    else { (context.is_process_alive)(pid) }
}

fn claim_orphaned_run(context: &RecoveryContext, observed: &RecoverableRecord, parent: &str) -> ClaimedRun {
    let run_id = &observed.record.run_id;
    context.store.with_run_lock(run_id, || {
        let Ok(Some(fresh)) = context.store.read_checkpoint::<RecoverableRecord>(run_id) else { return ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None) };
        if fresh.record.status != DagRunStatus::Paused { return ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None) }
        if fresh.record.parent_session_id != observed.record.parent_session_id { return ClaimedRun::Skipped(DagRecoverySkipReason::ForeignSession, None) }
        let Some(pid) = fresh.lease_holder_pid.or(fresh.previous_lease_holder_pid) else { return ClaimedRun::Skipped(DagRecoverySkipReason::ForeignSession, None) };
        if pid != context.host_pid && (context.is_process_alive)(pid) { return ClaimedRun::Skipped(DagRecoverySkipReason::LiveLease, Some(pid)) }
        let claimed = RecoverableRecord { record: DagRunRecordV1 { parent_session_id: parent.to_string(), root_session_id: parent.to_string(), ..fresh.record.clone() }, lease_holder_pid: Some(context.host_pid), previous_lease_holder_pid: None };
        if context.store.write_checkpoint(run_id, &claimed).is_err() {
            return ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None);
        }
        ClaimedRun::Claimed(Box::new(claimed))
    }).unwrap_or(ClaimedRun::Skipped(DagRecoverySkipReason::NotPaused, None))
}

fn resume_claimed_run(context: &RecoveryContext, claimed: RecoverableRecord) -> DagRecoveryOutcome {
    let pending_errors = Arc::new(Mutex::new(BTreeMap::<DagNodeId, DagNodeError>::new()));
    let pending_terminal_results: Arc<Mutex<BTreeMap<DagNodeId, TaskRecord>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let journal = recovery_journal(
        context,
        claimed.clone(),
        Some((Arc::clone(&pending_errors), Arc::clone(&pending_terminal_results))),
    );
    let run_id = claimed.record.run_id.clone();
    let mut reused_outputs = HashMap::new();
    let mut pre_attached_tasks = BTreeMap::new();
    reconcile_nodes(context, &journal, &mut reused_outputs, &pending_errors, &pending_terminal_results, &mut pre_attached_tasks);
    let generation = journal.snapshot().record.generation + 1;
    let _ = journal.append(DagRunEventPayload::RunResumed { generation });
    let scheduler_result = create_dag_scheduler(DagSchedulerOptions {
        store: Arc::clone(&context.store),
        task_manager: Arc::clone(&context.task_manager),
        initial_record: journal.snapshot().record,
        execution_mode: None,
        ancestry: context.ancestry,
        node_spawn_policy: context.node_spawn_policy.clone(),
        subscriber_ring: context.subscriber_ring,
        pre_attached_tasks: (!pre_attached_tasks.is_empty()).then_some(pre_attached_tasks),
        now: Some(Arc::clone(&context.now)),
    });
    let outcome = match scheduler_result {
        Ok(scheduler) => match scheduler.run() {
            Ok(record) => DagRecoveryOutcome::Resumed {
                run_id: run_id.clone(),
                record: Box::new(record),
                reused_outputs,
            },
            Err(_) => DagRecoveryOutcome::Skipped {
                run_id: run_id.clone(),
                reason: DagRecoverySkipReason::NotPaused,
                holder_pid: None,
            },
        },
        Err(_) => DagRecoveryOutcome::Skipped {
            run_id: run_id.clone(),
            reason: DagRecoverySkipReason::NotPaused,
            holder_pid: None,
        },
    };
    release_lease(context, &run_id);
    outcome
}

#[allow(clippy::too_many_lines)]
fn reconcile_nodes(
    context: &RecoveryContext,
    journal: &Arc<DagJournal<RecoverableRecord>>,
    reused_outputs: &mut HashMap<DagNodeId, String>,
    pending_errors: &Arc<Mutex<BTreeMap<DagNodeId, DagNodeError>>>,
    pending_terminal_results: &Arc<Mutex<BTreeMap<DagNodeId, TaskRecord>>>,
    pre_attached_tasks: &mut BTreeMap<DagNodeId, String>,
) {
    let nodes = journal.snapshot().record.nodes;
    for observed in &nodes {
        match observed.state {
            DagNodeState::Completed => {
                if let Some(result) = read_dag_node_result(DagNodeResultReadInput {
                    store: &context.store,
                    run_id: &journal.snapshot().record.run_id,
                    node_id: &observed.id,
                }) {
                    reused_outputs.insert(observed.id.clone(), result.output);
                    if let Some(task_id) = &observed.task_id {
                        let _ = journal.append(DagRunEventPayload::NodeReused {
                            node_id: observed.id.clone(),
                            task_id: task_id.clone(),
                            source_run_id: journal.snapshot().record.run_id.clone(),
                        });
                    }
                }
                continue;
            }
            DagNodeState::Failed | DagNodeState::Cancelled | DagNodeState::Skipped => continue,
            DagNodeState::Scheduled | DagNodeState::Running => {}
            DagNodeState::Pending | DagNodeState::Blocked => continue,
        }

        let owner_stamp = task_owner(&journal.snapshot().record, &observed.id, observed.exec_attempt);
        let owned = context.task_manager.find_owned_task(&owner_stamp.run_id, &owner_stamp.node_id);
        let mut task = owned.or_else(|| observed.task_id.as_ref().and_then(|task_id| context.task_manager.get(task_id)));
        if let Some(replacement) = &task
            && observed.task_id.as_deref() != Some(replacement.task_id.as_str())
        {
            attach_task(journal, &observed.id, &replacement.task_id);
        }
        let mut started_here = false;

        if task.is_none() && observed.state == DagNodeState::Scheduled && observed.task_id.is_none() {
            let spec = start_spec(&journal.snapshot().record, &observed.id, context.ancestry.map_or(0, |ancestry| ancestry.depth));
            let owner = task_owner(&journal.snapshot().record, &observed.id, observed.exec_attempt);
            let result = context.task_manager.start_owned(&spec, &owner);
            match result {
                OwnedStartResult::NotStarted(StartResult::ResidencyDenied { .. }) => {
                    let _ = journal.append(DagRunEventPayload::NodeTransitioned {
                        node_id: observed.id.clone(),
                        from: DagNodeState::Scheduled,
                        to: DagNodeState::Pending,
                        reason: DagNodeTransitionReason::Resumed,
                    });
                    continue;
                }
                OwnedStartResult::Started { task: started, .. } => {
                    attach_task(journal, &observed.id, &started.task_id);
                    started_here = true;
                    task = context.task_manager.get(&started.task_id);
                }
                other => {
                    let failure = start_failure(&other);
                    fail_node(journal, &observed.id, failure.0, &failure.1, &context.now, pending_errors);
                    continue;
                }
            }
        }

        let Some(current_task) = task else {
            let result = read_dag_node_result(DagNodeResultReadInput {
                store: &context.store,
                run_id: &journal.snapshot().record.run_id,
                node_id: &observed.id,
            });
            if let Some(result) = result {
                reused_outputs.insert(observed.id.clone(), result.output);
                transition_terminal(journal, &observed.id, DagNodeState::Completed);
                continue;
            }
            let transcript = observed
                .task_id
                .as_ref()
                .is_some_and(|task_id| has_transcript(&context.store.state_dir, task_id));
            let message = match &observed.task_id {
                Some(task_id) if transcript => {
                    format!("task {task_id} has a transcript but no recoverable task owner record")
                }
                Some(task_id) => format!("task {task_id} and all recovery artifacts are missing"),
                None => "task unknown and all recovery artifacts are missing".to_string(),
            };
            fail_node(
                journal,
                &observed.id,
                DagNodeErrorCode::ResumeTaskMissing,
                &message,
                &context.now,
                pending_errors,
            );
            continue;
        };

        if matches!(current_task.status, TaskStatus::Pending | TaskStatus::Running) {
            if !started_here && context.task_manager.get_resident_handle(&current_task.task_id).is_none() {
                let detail = format!("residency_state={}, host_pid={:?}, runner_kind={:?}", current_task.residency_state.as_str(), current_task.host_pid, current_task.runner_kind);
                fail_node(journal, &observed.id, DagNodeErrorCode::ResumeTaskOrphaned,
                    &format!("task {} still reads {:?} but no child of this host backs it ({detail}); its settlement can never be observed here. Retry or send revives the node.", current_task.task_id, current_task.status),
                    &context.now, pending_errors);
                continue;
            }
            if let Some(reattach) = &context.reattach { reattach(&journal.snapshot().record.run_id, &current_task.task_id); }
            if observed.task_id.as_deref() != Some(current_task.task_id.as_str()) {
                attach_task(journal, &observed.id, &current_task.task_id);
            }
            transition_to_running(journal, &observed.id);
            pre_attached_tasks.insert(observed.id.clone(), current_task.task_id);
            continue;
        }
        // Legacy task records without a launch stamp remain eligible; a stamp proves the runner
        // launch boundary was crossed even if the DAG admission batch never attached the task.
        if current_task.status == TaskStatus::Lost
            && current_task.started_at.is_none()
            && observed.state == DagNodeState::Scheduled
            && observed.exec_attempt.unwrap_or(0) < MAX_RECOVERY_READMISSIONS {
            let next_attempt = observed.exec_attempt.unwrap_or(0) + 1;
            pending_terminal_results.lock().unwrap_or_else(PoisonError::into_inner).remove(&observed.id);
            let _ = journal.append(DagRunEventPayload::NodeRetried {
                node_id: observed.id.clone(), prior_task_id: Some(current_task.task_id),
                exec_attempt: next_attempt, prompt_changed: false,
            });
            continue;
        }
        fold_task_outcome(context, journal, &observed.id, current_task, pending_errors, pending_terminal_results);
    }
}

fn fold_task_outcome(
    context: &RecoveryContext,
    journal: &Arc<DagJournal<RecoverableRecord>>,
    node_id: &DagNodeId,
    task: TaskRecord,
    pending_errors: &Arc<Mutex<BTreeMap<DagNodeId, DagNodeError>>>,
    pending_terminal_results: &Arc<Mutex<BTreeMap<DagNodeId, TaskRecord>>>,
) {
    if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
        fail_node(
            journal,
            node_id,
            DagNodeErrorCode::TaskError,
            &format!("TaskManager.wait_for returned nonterminal task {}", task.task_id),
            &context.now,
            pending_errors,
        );
        return;
    }
    if task.status == TaskStatus::Completed {
        pending_terminal_results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node_id.clone(), task);
        transition_terminal(journal, node_id, DagNodeState::Completed);
        return;
    }
    let error_message = task.error_message.clone();
    let failure = task_failure(task.status, error_message.as_deref());
    fail_node(journal, node_id, failure.0, &failure.1, &context.now, pending_errors);
}

fn recovery_journal(
    context: &RecoveryContext,
    record: RecoverableRecord,
    pending: Option<PendingRecoveryState>,
) -> Arc<DagJournal<RecoverableRecord>> {
    let store = Arc::clone(&context.store);
    let now = Arc::clone(&context.now);
    let (pending_errors, pending_terminal_results) = pending.unwrap_or_else(|| {
        (
            Arc::new(Mutex::new(BTreeMap::new())),
            Arc::new(Mutex::new(BTreeMap::new())),
        )
    });
    create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: record.record.run_id.clone(),
        initial_checkpoint: record,
        apply_event: Arc::new(move |checkpoint: &RecoverableRecord, event: &DagRunEvent| {
            apply_recovery_event(
                checkpoint,
                event,
                &pending_errors.lock().unwrap_or_else(PoisonError::into_inner),
                &store,
                &pending_terminal_results,
                &now,
            )
        }),
        subscriber_ring: context.subscriber_ring,
        now: Some(Arc::clone(&context.now)),
    })
    .expect("recovery journal recovers a checkpoint the caller already validated")
}

fn apply_recovery_event(
    record: &RecoverableRecord,
    event: &DagRunEvent,
    pending_errors: &BTreeMap<DagNodeId, DagNodeError>,
    store: &Arc<DagFileStore>,
    pending_terminal_results: &Mutex<BTreeMap<DagNodeId, TaskRecord>>,
    now: &Arc<dyn Fn() -> i64 + Send + Sync>,
) -> RecoverableRecord {
    match &event.payload {
        DagRunEventPayload::RunPaused { .. } => RecoverableRecord {
            record: DagRunRecordV1 {
                status: DagRunStatus::Paused,
                updated_at: event.at.clone(),
                ..record.record.clone()
            },
            ..record.clone()
        },
        DagRunEventPayload::RunResumed { generation } => RecoverableRecord {
            record: DagRunRecordV1 {
                status: DagRunStatus::Running,
                generation: *generation,
                completed_at: None,
                updated_at: event.at.clone(),
                ..record.record.clone()
            },
            ..record.clone()
        },
        _ => {
            let updated = apply_dag_scheduler_event_with_results_for_recovery(
                &record.record,
                event,
                pending_errors,
                store,
                pending_terminal_results,
                now,
            );
            RecoverableRecord {
                record: updated,
                ..record.clone()
            }
        }
    }
}

fn attach_task(journal: &Arc<DagJournal<RecoverableRecord>>, node_id: &DagNodeId, task_id: &str) {
    let Some(node) = node_by_id(&journal.snapshot().record, node_id) else { return };
    let _ = journal.append(DagRunEventPayload::NodeTaskAttached {
        node_id: node_id.clone(),
        task_id: task_id.to_string(),
        attempt: node.attempt + 1,
    });
}

fn transition_terminal(journal: &Arc<DagJournal<RecoverableRecord>>, node_id: &DagNodeId, to: DagNodeState) {
    let Some(node) = node_by_id(&journal.snapshot().record, node_id) else { return };
    let reason = if to == DagNodeState::Completed {
        DagNodeTransitionReason::Succeeded
    } else {
        DagNodeTransitionReason::Failed
    };
    let _ = journal.append(DagRunEventPayload::NodeTransitioned {
        node_id: node_id.clone(),
        from: node.state,
        to,
        reason,
    });
}

fn transition_to_running(journal: &Arc<DagJournal<RecoverableRecord>>, node_id: &DagNodeId) {
    let Some(node) = node_by_id(&journal.snapshot().record, node_id) else { return };
    if node.state == DagNodeState::Running { return; }
    let _ = journal.append(DagRunEventPayload::NodeTransitioned { node_id: node_id.clone(), from: node.state,
        to: DagNodeState::Running, reason: DagNodeTransitionReason::Resumed });
}

fn fail_node(
    journal: &Arc<DagJournal<RecoverableRecord>>,
    node_id: &DagNodeId,
    code: DagNodeErrorCode,
    message: &str,
    now: &Arc<dyn Fn() -> i64 + Send + Sync>,
    pending_errors: &Arc<Mutex<BTreeMap<DagNodeId, DagNodeError>>>,
) {
    pending_errors.lock().unwrap_or_else(PoisonError::into_inner).insert(
        node_id.clone(),
        DagNodeError {
            code,
            message: message.to_string(),
            node_id: Some(node_id.clone()),
            at: crate::shared::iso_from_ms(now()),
        },
    );
    transition_terminal(journal, node_id, DagNodeState::Failed);
    pending_errors.lock().unwrap_or_else(PoisonError::into_inner).remove(node_id);
}

fn release_lease(context: &RecoveryContext, run_id: &DagRunId) {
    let _ = context.store.with_run_lock(run_id, || {
        let Ok(Some(fresh)) = context.store.read_checkpoint::<RecoverableRecord>(run_id) else {
            return;
        };
        if fresh.lease_holder_pid != Some(context.host_pid) {
            return;
        }
        let released = RecoverableRecord {
            lease_holder_pid: None,
            previous_lease_holder_pid: None,
            ..fresh
        };
        let _ = context.store.write_checkpoint(run_id, &released);
    });
}

fn list_run_records(store: &DagFileStore) -> Vec<RecoverableRecord> {
    let Ok(entries) = fs::read_dir(&store.paths.runs) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Ok(Some(record)) = store.read_checkpoint::<RecoverableRecord>(stem) {
            records.push(record);
        }
    }
    records
}

fn start_spec(record: &DagRunRecordV1, node_id: &DagNodeId, session_depth: u32) -> ManagerStartSpec {
    let node = node_by_id(record, node_id).expect("reconciled node exists");
    let persisted = persisted_node(record, node_id).expect("persisted definition exists");
    let (category, subagent_type, model) = match &node.route {
        DagRoute::Category { category } => (Some(category.clone()), None, None),
        DagRoute::Agent { agent, model } => (None, Some(agent.clone()), model.clone()),
    };
    ManagerStartSpec {
        prompt: persisted.effective_prompt.clone(),
        task_summary: persisted.task_summary.clone(),
        parent_session_id: record.parent_session_id.clone(),
        root_session_id: Some(record.root_session_id.clone()),
        depth: session_depth + 1,
        category,
        subagent_type,
        model,
        name: Some(node.id.clone()),
        description: persisted.description.clone(),
        run_in_background: true,
        ..ManagerStartSpec::default()
    }
}

const MAX_RECOVERY_READMISSIONS: u32 = 3;

fn task_owner(record: &DagRunRecordV1, node_id: &DagNodeId, exec_attempt: Option<u32>) -> DagTaskOwner {
    let mut identity = serde_json::json!({
        "definitionFingerprint": record.definition_fingerprint,
        "nodeId": node_id,
    });
    if let Some(exec_attempt) = exec_attempt { identity["execAttempt"] = serde_json::json!(exec_attempt); }
    DagTaskOwner {
        kind: DagOwnerKind::Dag,
        run_id: record.run_id.clone(),
        node_id: node_id.clone(),
        fingerprint: dag_fingerprint(&identity),
    }
}

fn node_by_id(record: &DagRunRecordV1, node_id: &DagNodeId) -> Option<DagNode> {
    record.nodes.iter().find(|node| &node.id == node_id).cloned()
}

fn persisted_node(record: &DagRunRecordV1, node_id: &DagNodeId) -> Option<DagPersistedNode> {
    record
        .definition
        .nodes
        .iter()
        .find(|node| &node.id == node_id)
        .cloned()
}

fn start_failure(result: &OwnedStartResult) -> (DagNodeErrorCode, String) {
    match result {
        OwnedStartResult::OwnerConflict { task_id, .. } => (
            DagNodeErrorCode::StartFailed,
            format!("DAG task owner conflicts with existing task {task_id}"),
        ),
        OwnedStartResult::NotStarted(StartResult::DepthDenied { reason, .. }) => {
            (DagNodeErrorCode::DepthDenied, reason.clone())
        }
        OwnedStartResult::NotStarted(StartResult::PlanUnresolved(error)) => {
            (DagNodeErrorCode::PlanUnresolved, error.message.clone())
        }
        OwnedStartResult::NotStarted(StartResult::StartFailed(StartFailure { error_message, .. })) => {
            (DagNodeErrorCode::StartFailed, error_message.clone())
        }
        OwnedStartResult::NotStarted(StartResult::ResidencyDenied { reason, .. }) => {
            (DagNodeErrorCode::ResidencyDenied, reason.clone())
        }
        OwnedStartResult::Started { .. } | OwnedStartResult::NotStarted(StartResult::Started(_)) => {
            unreachable!("Started never reaches start_failure")
        }
    }
}

fn task_failure(status: TaskStatus, message: Option<&str>) -> (DagNodeErrorCode, String) {
    match status {
        TaskStatus::Error => (DagNodeErrorCode::TaskError, message.unwrap_or("task failed").to_string()),
        TaskStatus::Interrupted => (
            DagNodeErrorCode::TaskInterrupted,
            message.unwrap_or("task was interrupted").to_string(),
        ),
        TaskStatus::Lost => (DagNodeErrorCode::TaskLost, message.unwrap_or("task was lost").to_string()),
        TaskStatus::Cancelled => (
            DagNodeErrorCode::TaskCancelled,
            message.unwrap_or("task was cancelled").to_string(),
        ),
        TaskStatus::Completed | TaskStatus::Pending | TaskStatus::Running => {
            (DagNodeErrorCode::TaskError, "task in a non-terminal-failure status".to_string())
        }
    }
}

fn has_transcript(state_dir: &Path, task_id: &str) -> bool {
    if state_dir.join("logs").join(format!("{task_id}.jsonl")).is_file() {
        return true;
    }
    let child_dir = state_dir.join("children").join(task_id);
    fn contains_jsonl(dir: &Path) -> bool {
        let Ok(entries) = fs::read_dir(dir) else { return false };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if contains_jsonl(&path) {
                    return true;
                }
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                return true;
            }
        }
        false
    }
    contains_jsonl(&child_dir)
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
