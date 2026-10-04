//! `dag/scheduler.ts`: the admission loop, event reducer, and task outcome folding stay together
//! so the strict barrier cannot be bypassed by callers.
// allow: SIZE_OK - the admission loop, event reducer, and task outcome folding stay together so
// the strict barrier cannot be bypassed by callers.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::agents::AgentDefinition;
use crate::dag::execution_mode::{DagExecutionModeSources, resolve_dag_node_execution_mode};
use crate::dag::fingerprint::dag_fingerprint;
use crate::dag::journal::{
    DagJournal, DagJournalListener, DagJournalOptions, DagJournalUnsubscribe, create_dag_journal,
};
use crate::dag::manager::{DagPersistedNode, DagRunRecordV1};
use crate::dag::owner::{DagOwnerKind, DagTaskOwner, OwnedStartResult};
use crate::dag::results::{
    DagNodeResultArtifact, DagNodeResultPersistInput, DagNodeResultPersistOutcome,
    DagNodeResultReadInput, persist_dag_node_result, read_dag_node_result,
};
use crate::dag::store::DagFileStore;
use crate::dag::types::{
    DagDiagnostic, DagNode, DagNodeCounts, DagNodeError, DagNodeErrorCode, DagNodeId,
    DagNodeState, DagNodeTransitionReason, DagRoute, DagRunEvent, DagRunEventPayload, DagRunId,
};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{ManagerStartSpec, StartFailure, StartResult};
use crate::manager::TaskManager;
use crate::state::{TaskRecord, TaskRunStats, TaskStatus};
use crate::steering::CancelOptions;

fn is_terminal_node_state(state: DagNodeState) -> bool {
    matches!(
        state,
        DagNodeState::Completed | DagNodeState::Failed | DagNodeState::Cancelled | DagNodeState::Skipped
    )
}

pub struct DagSchedulerOptions {
    pub store: Arc<DagFileStore>,
    pub task_manager: Arc<TaskManager>,
    pub initial_record: DagRunRecordV1,
    pub execution_mode_agents: Option<Arc<BTreeMap<String, AgentDefinition>>>,
    pub execution_mode_config: Option<ExecutionMode>,
    pub ancestry_depth: Option<u32>,
    pub subscriber_ring: Option<usize>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

struct AttachedTask {
    settled: std::sync::mpsc::Receiver<AttachedTaskSettlement>,
}

enum AttachedTaskSettlement {
    Record(Box<TaskRecord>),
    Error(String),
}

struct SchedulerState {
    admission_stopped: std::sync::atomic::AtomicBool,
    running: Mutex<bool>,
    run_idle: Condvar,
    pending_errors: Mutex<BTreeMap<DagNodeId, DagNodeError>>,
    pending_terminal_results: Mutex<BTreeMap<DagNodeId, TaskRecord>>,
    attached_task_ids: Mutex<BTreeMap<DagNodeId, String>>,
    cancellation_started: Mutex<bool>,
    cancellation_condvar: Condvar,
    cancellation_operation_done: Mutex<Option<Result<(), String>>>,
    cancellation_completed_condvar: Condvar,
    admission_in_progress: Mutex<bool>,
    admission_condvar: Condvar,
}

pub struct DagSchedulerContext {
    task_manager: Arc<TaskManager>,
    #[cfg(test)]
    task_port: Mutex<Option<Arc<dyn TestTaskPort>>>,
    journal: Arc<DagJournal<DagRunRecordV1>>,
    definition_nodes: BTreeMap<DagNodeId, DagPersistedNode>,
    execution_mode_agents: Option<Arc<BTreeMap<String, AgentDefinition>>>,
    execution_mode_config: Option<ExecutionMode>,
    ancestry_depth: Option<u32>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    state: Arc<SchedulerState>,
}

pub fn create_dag_scheduler(
    options: DagSchedulerOptions,
) -> Result<Arc<DagSchedulerContext>, crate::dag::store::DagStoreError> {
    let now = options
        .now
        .unwrap_or_else(|| Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX)));
    let state = Arc::new(SchedulerState {
        admission_stopped: std::sync::atomic::AtomicBool::new(false),
        running: Mutex::new(false),
        run_idle: Condvar::new(),
        pending_errors: Mutex::new(BTreeMap::new()),
        pending_terminal_results: Mutex::new(BTreeMap::new()),
        attached_task_ids: Mutex::new(BTreeMap::new()),
        cancellation_started: Mutex::new(false),
        cancellation_condvar: Condvar::new(),
        cancellation_operation_done: Mutex::new(None),
        cancellation_completed_condvar: Condvar::new(),
        admission_in_progress: Mutex::new(false),
        admission_condvar: Condvar::new(),
    });
    let store = options.store;
    let reducer_state = Arc::clone(&state);
    let reducer_store = Arc::clone(&store);
    let reducer_now = Arc::clone(&now);
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: options.initial_record.run_id.clone(),
        initial_checkpoint: options.initial_record.clone(),
        apply_event: Arc::new(move |record: &DagRunRecordV1, event: &DagRunEvent| {
            apply_dag_scheduler_event_with_results(
                record,
                event,
                &reducer_state.pending_errors.lock().unwrap_or_else(PoisonError::into_inner),
                Some(&reducer_store),
                Some(&reducer_state.pending_terminal_results),
                Some(&reducer_now),
            )
        }),
        subscriber_ring: options.subscriber_ring,
        now: Some(Arc::clone(&now)),
    })?;
    let definition_nodes = options
        .initial_record
        .definition
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect();
    Ok(Arc::new(DagSchedulerContext {
        task_manager: options.task_manager,
        #[cfg(test)]
        task_port: Mutex::new(None),
        journal,
        definition_nodes,
        execution_mode_agents: options.execution_mode_agents,
        execution_mode_config: options.execution_mode_config,
        ancestry_depth: options.ancestry_depth,
        now,
        state,
    }))
}

/// The pure event reducer (`applyDagSchedulerEvent`), usable without result persistence for
/// replay-only callers.
pub fn apply_dag_scheduler_event(
    record: &DagRunRecordV1,
    event: &DagRunEvent,
    pending_errors: &BTreeMap<DagNodeId, DagNodeError>,
) -> DagRunRecordV1 {
    apply_dag_scheduler_event_with_results(record, event, pending_errors, None, None, None)
}

/// Recovery's `applyDagSchedulerEvent(record, event, pendingErrors, { store, pendingTerminalResults, now })`
/// call with a full result-persistence context; recovery reuses this reducer verbatim rather than
/// re-deriving node-transition/result-persistence semantics.
pub fn apply_dag_scheduler_event_with_results_for_recovery(
    record: &DagRunRecordV1,
    event: &DagRunEvent,
    pending_errors: &BTreeMap<DagNodeId, DagNodeError>,
    store: &Arc<DagFileStore>,
    pending_terminal_results: &Mutex<BTreeMap<DagNodeId, TaskRecord>>,
    now: &Arc<dyn Fn() -> i64 + Send + Sync>,
) -> DagRunRecordV1 {
    apply_dag_scheduler_event_with_results(
        record,
        event,
        pending_errors,
        Some(store),
        Some(pending_terminal_results),
        Some(now),
    )
}

fn apply_dag_scheduler_event_with_results(
    record: &DagRunRecordV1,
    event: &DagRunEvent,
    pending_errors: &BTreeMap<DagNodeId, DagNodeError>,
    store: Option<&Arc<DagFileStore>>,
    pending_terminal_results: Option<&Mutex<BTreeMap<DagNodeId, TaskRecord>>>,
    now: Option<&Arc<dyn Fn() -> i64 + Send + Sync>>,
) -> DagRunRecordV1 {
    match &event.payload {
        DagRunEventPayload::RunPaused { .. } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Paused, updated_at:event.at.clone(), ..record.clone()
        },
        DagRunEventPayload::RunResumed { generation } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Running, generation:*generation, updated_at:event.at.clone(), ..record.clone()
        },
        DagRunEventPayload::RunStarted { .. } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Running,
            started_at: record.started_at.clone().or_else(|| Some(event.at.clone())),
            updated_at: event.at.clone(),
            ..record.clone()
        },
        DagRunEventPayload::RunCompleted { .. } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Completed,
            completed_at: Some(event.at.clone()),
            updated_at: event.at.clone(),
            ..record.clone()
        },
        DagRunEventPayload::RunFailed { .. } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Failed,
            completed_at: Some(event.at.clone()),
            updated_at: event.at.clone(),
            ..record.clone()
        },
        DagRunEventPayload::RunCancelled { .. } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Cancelled,
            completed_at: Some(event.at.clone()),
            updated_at: event.at.clone(),
            ..record.clone()
        },
        DagRunEventPayload::NodeTransitioned { node_id, to, .. } => reduce_node_transitioned(
            record,
            event,
            node_id,
            *to,
            pending_errors,
            store,
            pending_terminal_results,
            now,
        ),
        DagRunEventPayload::NodeTaskAttached {
            node_id,
            task_id,
            attempt,
        } => DagRunRecordV1 {
            nodes: record
                .nodes
                .iter()
                .map(|node| {
                    if &node.id == node_id {
                        DagNode {
                            task_id: Some(task_id.clone()),
                            attempt: *attempt,
                            ..node.clone()
                        }
                    } else {
                        node.clone()
                    }
                })
                .collect(),
            updated_at: event.at.clone(),
            ..record.clone()
        },
        _ => DagRunRecordV1 {
            updated_at: event.at.clone(),
            ..record.clone()
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn reduce_node_transitioned(
    record: &DagRunRecordV1,
    event: &DagRunEvent,
    node_id: &DagNodeId,
    to: DagNodeState,
    pending_errors: &BTreeMap<DagNodeId, DagNodeError>,
    store: Option<&Arc<DagFileStore>>,
    pending_terminal_results: Option<&Mutex<BTreeMap<DagNodeId, TaskRecord>>>,
    now: Option<&Arc<dyn Fn() -> i64 + Send + Sync>>,
) -> DagRunRecordV1 {
    let terminal_result = if is_terminal_node_state(to) {
        pending_terminal_results.and_then(|results| {
            results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(node_id)
                .cloned()
        })
    } else {
        None
    };
    let replayed = if terminal_result.is_none() && store.is_some() && to == DagNodeState::Completed {
        store.and_then(|store| replay_dag_node_result(store, &record.run_id, node_id))
    } else {
        None
    };
    let persisted = if let (Some(task_record), Some(store), Some(now)) = (&terminal_result, store, now) {
        Some(persist_dag_node_result(DagNodeResultPersistInput {
            store,
            run_id: record.run_id.clone(),
            node_id: node_id.clone(),
            record: task_record,
            now: Some(now.as_ref()),
        }))
    } else {
        None
    };
    if let Some(results) = pending_terminal_results {
        results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(node_id);
    }
    let mut diagnostics = record.diagnostics.clone();
    let mut result_artifact: Option<DagNodeResultArtifact> = None;
    let mut run_stats: Option<TaskRunStats> = None;
    match persisted {
        Some(DagNodeResultPersistOutcome::Persisted { artifact }) => {
            run_stats = task_run_stats_of(&terminal_result);
            result_artifact = Some(artifact);
        }
        Some(DagNodeResultPersistOutcome::Failed { diagnostic }) => {
            diagnostics.push(match diagnostic {
                crate::dag::store::DagStoreDiagnostic::JournalCorrupt {
                    run_id,
                    path,
                    message,
                    at,
                } => DagDiagnostic::JournalCorrupt {
                    run_id,
                    path,
                    message,
                    at,
                },
                other => other_diagnostic_fallback(other),
            });
        }
        None => {
            if let Some(replayed) = replayed {
                result_artifact = Some(replayed.artifact);
                run_stats = replayed.run_stats;
            }
        }
    }
    let nodes = record
        .nodes
        .iter()
        .map(|node| {
            if &node.id != node_id {
                return node.clone();
            }
            let mut transitioned = transitioned_node(node, to, &event.at, pending_errors.get(node_id));
            if result_artifact.is_some() {
                transitioned.run_stats = run_stats.clone().or(transitioned.run_stats);
            }
            transitioned
        })
        .collect();
    DagRunRecordV1 {
        nodes,
        diagnostics,
        updated_at: event.at.clone(),
        ..record.clone()
    }
}

fn task_run_stats_of(terminal_result: &Option<TaskRecord>) -> Option<TaskRunStats> {
    terminal_result
        .as_ref()
        .and_then(|record| record.run_stats.clone())
}

fn other_diagnostic_fallback(_diagnostic: crate::dag::store::DagStoreDiagnostic) -> DagDiagnostic {
    DagDiagnostic::RunFlag {
        message: "dag store diagnostic of an unexpected kind".to_string(),
        at: String::new(),
    }
}

struct ReplayedResult {
    artifact: DagNodeResultArtifact,
    run_stats: Option<TaskRunStats>,
}

fn replay_dag_node_result(
    store: &DagFileStore,
    run_id: &DagRunId,
    node_id: &DagNodeId,
) -> Option<ReplayedResult> {
    let result = read_dag_node_result(DagNodeResultReadInput {
        store,
        run_id,
        node_id,
    })?;
    let output_path = store.paths.result(run_id, node_id);
    let output = fs::read_to_string(&output_path).ok()?;
    let stats_path = stats_sidecar_path(&output_path);
    let stats = read_optional_artifact(store, &stats_path);
    Some(ReplayedResult {
        artifact: DagNodeResultArtifact {
            stats,
            ..artifact_ref(&store.state_dir, &output_path, output.as_bytes())
        },
        run_stats: result.run_stats,
    })
}

fn stats_sidecar_path(output_path: &Path) -> std::path::PathBuf {
    let stem = output_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output_path.with_file_name(format!("{stem}.stats.json"))
}

fn read_optional_artifact(
    store: &DagFileStore,
    path: &Path,
) -> Option<crate::dag::results::DagResultArtifactRef> {
    let contents = fs::read_to_string(path).ok()?;
    Some(artifact_ref_from(&store.state_dir, path, contents.as_bytes()))
}

fn artifact_ref(
    state_dir: &Path,
    path: &Path,
    contents: &[u8],
) -> crate::dag::results::DagNodeResultArtifact {
    let reference = artifact_ref_from(state_dir, path, contents);
    crate::dag::results::DagNodeResultArtifact {
        relative_path: reference.relative_path,
        sha256: reference.sha256,
        bytes: reference.bytes,
        stats: None,
    }
}

fn artifact_ref_from(
    state_dir: &Path,
    path: &Path,
    contents: &[u8],
) -> crate::dag::results::DagResultArtifactRef {
    let relative_path = path
        .strip_prefix(state_dir)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    let digest = Sha256::digest(contents);
    crate::dag::results::DagResultArtifactRef {
        relative_path,
        sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        bytes: contents.len() as u64,
    }
}

impl DagSchedulerContext {
    pub fn admission_is_stopped(&self) -> bool { admission_stopped(self) }
    pub fn stop_admission(&self) {
        self.state.admission_stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        when_admission_idle(self);
        let mut running = self.state.running.lock().unwrap_or_else(PoisonError::into_inner);
        while *running { running = self.state.run_idle.wait(running).unwrap_or_else(PoisonError::into_inner); }
    }
    #[cfg(test)]
    pub(crate) fn set_task_port(&self, port: Arc<dyn TestTaskPort>) {
        *self.task_port.lock().unwrap() = Some(port);
    }

    pub fn run(&self) -> Result<DagRunRecordV1, crate::dag::store::DagStoreError> {
        {
            let mut running = self.state.running.lock().unwrap_or_else(PoisonError::into_inner);
            if *running { return Err(crate::dag::store::DagStoreError::Message("DAG scheduler is already running".into())); }
            if admission_stopped(self) { return Ok(self.journal.snapshot()); }
            *running = true;
        }
        struct RunGuard<'a>(&'a SchedulerState);
        impl Drop for RunGuard<'_> {
            fn drop(&mut self) {
                *self.0.running.lock().unwrap_or_else(PoisonError::into_inner) = false;
                self.0.run_idle.notify_all();
            }
        }
        let _running = RunGuard(&self.state);
        run_waves(self)
    }

    pub fn cancel(&self, run_id: &DagRunId, reason: Option<&str>) -> Result<(), String> {
        cancel_run(self, run_id, reason)
    }

    pub fn snapshot(&self) -> DagRunRecordV1 {
        self.journal.snapshot()
    }

    pub fn subscribe(self: &Arc<Self>, listener: DagJournalListener) -> DagJournalUnsubscribe {
        self.journal.subscribe(listener)
    }

    pub fn when_idle(&self) {
        self.journal.when_idle();
    }
}

fn cancel_run(
    context: &DagSchedulerContext,
    run_id: &DagRunId,
    reason: Option<&str>,
) -> Result<(), String> {
    let snapshot = context.journal.snapshot();
    if &snapshot.run_id != run_id {
        return Err(format!("scheduler does not own DAG run \"{run_id}\""));
    }
    if matches!(
        snapshot.status,
        crate::dag::types::DagRunStatus::Completed
            | crate::dag::types::DagRunStatus::Failed
            | crate::dag::types::DagRunStatus::Cancelled
    ) {
        return Ok(());
    }
    {
        let mut started = context
            .state
            .cancellation_started
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *started {
            // `return context.cancellationOperation`: a second caller joins the in-flight
            // cancellation instead of starting a second one, and observes its completion.
            return wait_for_cancellation_completed(context);
        }
        *started = true;
    }
    context.state.cancellation_condvar.notify_all();
    let result = perform_cancellation(context, reason);
    *context
        .state
        .cancellation_operation_done
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(result.clone());
    context.state.cancellation_completed_condvar.notify_all();
    result
}

/// `await context.cancellationCompleted`: blocks until the in-flight cancellation has stored its
/// outcome (the terminal node states and the run-cancelled event are durable by then).
fn wait_for_cancellation_completed(context: &DagSchedulerContext) -> Result<(), String> {
    let mut done = context
        .state
        .cancellation_operation_done
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    while done.is_none() {
        done = context
            .state
            .cancellation_completed_condvar
            .wait(done)
            .unwrap_or_else(PoisonError::into_inner);
    }
    done.clone().unwrap_or(Ok(()))
}

fn perform_cancellation(context: &DagSchedulerContext, reason: Option<&str>) -> Result<(), String> {
    when_admission_idle(context);
    let attached_task_ids: Vec<String> = context
        .state
        .attached_task_ids
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .cloned()
        .collect();
    let mut cancellation_failure: Option<String> = None;
    for task_id in &attached_task_ids {
        if let Err(error) = cancel_task(context, task_id, reason) && cancellation_failure.is_none()
        {
            cancellation_failure = Some(error.to_string());
        }
    }
    for node in context.journal.snapshot().nodes {
        if is_terminal_node_state(node.state) {
            continue;
        }
        transition(context, &node.id, DagNodeState::Cancelled, DagNodeTransitionReason::Cancelled);
    }
    let cancelled = context.journal.snapshot();
    context
        .journal
        .append(DagRunEventPayload::RunCancelled {
            reason: reason.map(str::to_string),
            counts: count_nodes(&cancelled.nodes),
        })
        .map_err(|error| error.to_string())?;
    context
        .state
        .attached_task_ids
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    match cancellation_failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn cancelled_snapshot(context: &DagSchedulerContext) -> DagRunRecordV1 {
    // `await context.cancellationCompleted`: the snapshot must observe the completed cancellation
    // (terminal node states plus the run-cancelled event), never the pre-cancellation record.
    let _ = wait_for_cancellation_completed(context);
    context.journal.snapshot()
}

fn when_admission_idle(context: &DagSchedulerContext) {
    let mut in_progress = context
        .state
        .admission_in_progress
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    while *in_progress {
        in_progress = context
            .state
            .admission_condvar
            .wait(in_progress)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

fn resolve_admission_idle(context: &DagSchedulerContext) {
    *context
        .state
        .admission_in_progress
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = false;
    context.state.admission_condvar.notify_all();
}

fn cancellation_started(context: &DagSchedulerContext) -> bool {
    *context
        .state
        .cancellation_started
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn run_waves(context: &DagSchedulerContext) -> Result<DagRunRecordV1, crate::dag::store::DagStoreError> {
    if admission_stopped(context) || context.journal.snapshot().status.is_terminal() {
        return Ok(context.journal.snapshot());
    }
    if context.journal.snapshot().status == crate::dag::types::DagRunStatus::Pending {
        let generation = context.journal.snapshot().generation;
        context
            .journal
            .append(DagRunEventPayload::RunStarted { generation })?;
    }

    for wave in context.journal.snapshot().waves.clone() {
        if admission_stopped(context) { return Ok(context.journal.snapshot()); }
        if cancellation_started(context) {
            return Ok(cancelled_snapshot(context));
        }
        apply_dependent_skip_cascade(context)?;
        let runnable: Vec<DagNodeId> = wave
            .node_ids
            .iter()
            .filter(|node_id| is_runnable(&context.journal.snapshot(), node_id))
            .cloned()
            .collect();
        if runnable.is_empty() {
            continue;
        }

        for node_id in &runnable {
            transition(context, node_id, DagNodeState::Scheduled, DagNodeTransitionReason::Scheduled);
        }
        context.journal.append(DagRunEventPayload::WaveStarted {
            wave_index: wave.index,
            node_ids: runnable.clone(),
        })?;
        if !admit_and_settle_wave(context, &runnable)? {
            if admission_stopped(context) { return Ok(context.journal.snapshot()); }
            return Ok(cancelled_snapshot(context));
        }
        context.journal.append(DagRunEventPayload::WaveCompleted {
            wave_index: wave.index,
            node_ids: runnable,
        })?;
    }

    apply_dependent_skip_cascade(context)?;
    if admission_stopped(context) { return Ok(context.journal.snapshot()); }
    let snapshot = context.journal.snapshot();
    if let Some(failed) = primary_failure(&snapshot) {
        let error = failed.error.clone().unwrap_or_else(|| {
            node_error(&failed.id, DagNodeErrorCode::StartFailed, "DAG node failed", &context.now)
        });
        context.journal.append(DagRunEventPayload::RunFailed {
            error,
            counts: count_nodes(&snapshot.nodes),
        })?;
    } else {
        context.journal.append(DagRunEventPayload::RunCompleted {
            counts: count_nodes(&snapshot.nodes),
        })?;
    }
    Ok(context.journal.snapshot())
}

fn admit_and_settle_wave(
    context: &DagSchedulerContext,
    node_ids: &[DagNodeId],
) -> Result<bool, crate::dag::store::DagStoreError> {
    let mut awaiting_admission: Vec<DagNodeId> = node_ids.to_vec();
    let mut attached: BTreeMap<DagNodeId, AttachedTask> = BTreeMap::new();

    while !awaiting_admission.is_empty() {
        if admission_stopped(context) { return Ok(false); }
        if cancellation_started(context) {
            return Ok(false);
        }
        {
            let mut in_progress = context.state.admission_in_progress.lock().unwrap_or_else(PoisonError::into_inner);
            if admission_stopped(context) { return Ok(false); }
            *in_progress = true;
        }
        let results: Vec<(DagNodeId, Result<OwnedStartResult, String>)> = awaiting_admission
            .iter()
            .map(|node_id| {
                let spec = start_spec(context, node_id);
                let owner_stamp = owner(context, node_id);
                (
                    node_id.clone(),
                    start_owned(context, &spec, &owner_stamp),
                )
            })
            .collect();
        let mut denied: Vec<DagNodeId> = Vec::new();
        let attachment_result = (|| -> Result<(), crate::dag::store::DagStoreError> {
        for (node_id, settled) in results {
            match settled {
                Err(error) => {
                    if !cancellation_started(context) {
                        fail_node(context, &node_id, DagNodeErrorCode::StartFailed, &error)?;
                    }
                }
                Ok(result) => {
                    if cancellation_started(context) {
                        if let OwnedStartResult::Started { task, reused } = result {
                            attach_started(context, &mut attached, &node_id, &task, reused)?;
                        }
                    } else if matches!(
                        result,
                        OwnedStartResult::NotStarted(StartResult::ResidencyDenied { .. })
                    ) {
                        denied.push(node_id);
                    } else {
                        attach_or_fail(context, &mut attached, &node_id, result)?;
                    }
                }
            }
        }
        Ok(())
        })();
        resolve_admission_idle(context);
        attachment_result?;
        if admission_stopped(context) { return Ok(false); }
        if cancellation_started(context) {
            return Ok(false);
        }
        awaiting_admission = denied;
        if awaiting_admission.is_empty() {
            break;
        }
        if attached.is_empty() {
            for node_id in &awaiting_admission {
                fail_node(
                    context,
                    node_id,
                    DagNodeErrorCode::ResidencyDenied,
                    "resident child cap reached and no task can free a slot",
                )?;
            }
            break;
        }
        if !settle_one(context, &mut attached)? {
            return Ok(false);
        }
    }

    while !attached.is_empty() {
        if !settle_one(context, &mut attached)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn attach_or_fail(
    context: &DagSchedulerContext,
    attached: &mut BTreeMap<DagNodeId, AttachedTask>,
    node_id: &DagNodeId,
    result: OwnedStartResult,
) -> Result<(), crate::dag::store::DagStoreError> {
    match result {
        OwnedStartResult::Started { task, reused } => attach_started(context, attached, node_id, &task, reused),
        OwnedStartResult::OwnerConflict { task_id, .. } => fail_node(
            context,
            node_id,
            DagNodeErrorCode::StartFailed,
            &format!("DAG task owner conflicts with existing task {task_id}"),
        ),
        OwnedStartResult::NotStarted(start_result) => {
            let failure = start_failure(&start_result);
            fail_node(context, node_id, failure.0, &failure.1)
        }
    }
}

fn attach_started(
    context: &DagSchedulerContext,
    attached: &mut BTreeMap<DagNodeId, AttachedTask>,
    node_id: &DagNodeId,
    task: &crate::manager::types::StartedTask,
    reused: bool,
) -> Result<(), crate::dag::store::DagStoreError> {
    let node = node_by_id(&context.journal.snapshot(), node_id)?;
    let attempt = node.attempt + 1;
    context.journal.append(DagRunEventPayload::NodeTaskAttached {
        node_id: node_id.clone(),
        task_id: task.task_id.clone(),
        attempt,
    })?;
    if task.status == TaskStatus::Pending {
        if !reused
            && let Some(queue_position) = task.queue_position
        {
            context.journal.append(DagRunEventPayload::NodeTransitioned {
                node_id: node_id.clone(),
                from: DagNodeState::Scheduled,
                to: DagNodeState::Scheduled,
                reason: DagNodeTransitionReason::TaskQueued {
                    queue_position: queue_position as u64,
                },
            })?;
        }
    } else if task.status == TaskStatus::Running {
        transition(
            context,
            node_id,
            DagNodeState::Running,
            if reused {
                DagNodeTransitionReason::Resumed
            } else {
                DagNodeTransitionReason::Started
            },
        );
    }
    context
        .state
        .attached_task_ids
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(node_id.clone(), task.task_id.clone());
    let (sender, receiver): (Sender<AttachedTaskSettlement>, _) = channel();
    spawn_wait_for(context, task.task_id.clone(), sender);
    attached.insert(
        node_id.clone(),
        AttachedTask {
            settled: receiver,
        },
    );
    Ok(())
}

fn spawn_wait_for(context: &DagSchedulerContext, task_id: String, sender: Sender<AttachedTaskSettlement>) {
    let task_manager = Arc::clone(&context.task_manager);
    #[cfg(test)]
    let port = context.task_port.lock().unwrap().clone();
    std::thread::spawn(move || {
        #[cfg(test)]
        let result = match port {
            Some(port) => port.wait_for(&task_id),
            None => task_manager.wait_for(&task_id, None, None).map_err(|error| error.to_string()),
        };
        #[cfg(not(test))]
        let result = task_manager.wait_for(&task_id, None, None);
        let settlement = match result {
            Ok(record) => AttachedTaskSettlement::Record(Box::new(record)),
            Err(error) => AttachedTaskSettlement::Error(error.to_string()),
        };
        let _ = sender.send(settlement);
    });
}

#[cfg(test)]
pub(crate) trait TestTaskPort: Send + Sync {
    fn start_owned(&self, spec: &ManagerStartSpec, owner: &DagTaskOwner) -> Result<OwnedStartResult, String>;
    fn wait_for(&self, task_id: &str) -> Result<TaskRecord, String>;
    fn cancel_task(&self, task_id: &str) -> Result<(), String>;
}

fn start_owned(context: &DagSchedulerContext, spec: &ManagerStartSpec, owner: &DagTaskOwner) -> Result<OwnedStartResult, String> {
    #[cfg(test)]
    if let Some(port) = context.task_port.lock().unwrap().clone() {
        return port.start_owned(spec, owner);
    }
    Ok(context.task_manager.start_owned(spec, owner))
}

fn cancel_task(context: &DagSchedulerContext, task_id: &str, reason: Option<&str>) -> Result<(), String> {
    #[cfg(test)]
    if let Some(port) = context.task_port.lock().unwrap().clone() {
        return port.cancel_task(task_id);
    }
    context.task_manager.cancel_task(task_id, reason, CancelOptions {
        abort: crate::steering::CancelAbort::Skip,
    }).map(|_| ()).map_err(|error| error.to_string())
}

fn settle_one(
    context: &DagSchedulerContext,
    attached: &mut BTreeMap<DagNodeId, AttachedTask>,
) -> Result<bool, crate::dag::store::DagStoreError> {
    loop {
        if admission_stopped(context) { return Ok(false); }
        if cancellation_started(context) {
            return Ok(false);
        }
        let mut ready: Option<(DagNodeId, AttachedTaskSettlement)> = None;
        for (node_id, task) in attached.iter() {
            if let Ok(settlement) = task.settled.try_recv() {
                ready = Some((node_id.clone(), settlement));
                break;
            }
        }
        if let Some((node_id, settlement)) = ready {
            attached.remove(&node_id);
            context
                .state
                .attached_task_ids
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&node_id);
            match settlement {
                AttachedTaskSettlement::Error(message) => {
                    fail_node(context, &node_id, DagNodeErrorCode::TaskError, &message)?;
                }
                AttachedTaskSettlement::Record(record) => {
                    fold_task_outcome(context, &node_id, *record)?;
                }
            }
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn admission_stopped(context: &DagSchedulerContext) -> bool {
    context.state.admission_stopped.load(std::sync::atomic::Ordering::SeqCst)
}

fn fold_task_outcome(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    task: TaskRecord,
) -> Result<(), crate::dag::store::DagStoreError> {
    if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
        return fail_node(
            context,
            node_id,
            DagNodeErrorCode::TaskError,
            &format!("TaskManager.wait_for returned nonterminal task {}", task.task_id),
        );
    }
    let status = task.status;
    let error_message = task.error_message.clone();
    context
        .state
        .pending_terminal_results
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(node_id.clone(), task);
    if status == TaskStatus::Completed {
        transition(context, node_id, DagNodeState::Completed, DagNodeTransitionReason::Succeeded);
        return Ok(());
    }
    let failure = task_failure(status, error_message.as_deref());
    fail_node(context, node_id, failure.0, &failure.1)
}

fn apply_dependent_skip_cascade(
    context: &DagSchedulerContext,
) -> Result<(), crate::dag::store::DagStoreError> {
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot = context.journal.snapshot();
        for node in &snapshot.nodes {
            if !matches!(node.state, DagNodeState::Pending | DagNodeState::Blocked) {
                continue;
            }
            let should_skip = node.depends_on.iter().any(|dependency_id| {
                node_by_id(&snapshot, dependency_id)
                    .map(|dependency| {
                        is_terminal_node_state(dependency.state) && dependency.state != DagNodeState::Completed
                    })
                    .unwrap_or(false)
            });
            if should_skip {
                transition(context, &node.id, DagNodeState::Skipped, DagNodeTransitionReason::Skipped);
                changed = true;
            }
        }
    }
    Ok(())
}

fn is_runnable(record: &DagRunRecordV1, node_id: &DagNodeId) -> bool {
    let Ok(node) = node_by_id(record, node_id) else {
        return false;
    };
    matches!(node.state, DagNodeState::Pending | DagNodeState::Blocked)
        && node.depends_on.iter().all(|dependency_id| {
            node_by_id(record, dependency_id)
                .map(|dependency| dependency.state == DagNodeState::Completed)
                .unwrap_or(false)
        })
}

fn transition(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    to: DagNodeState,
    reason: DagNodeTransitionReason,
) {
    let Ok(node) = node_by_id(&context.journal.snapshot(), node_id) else {
        return;
    };
    let _ = context.journal.append(DagRunEventPayload::NodeTransitioned {
        node_id: node_id.clone(),
        from: node.state,
        to,
        reason,
    });
}

fn fail_node(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    code: DagNodeErrorCode,
    message: &str,
) -> Result<(), crate::dag::store::DagStoreError> {
    context
        .state
        .pending_errors
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(node_id.clone(), node_error(node_id, code, message, &context.now));
    transition(context, node_id, DagNodeState::Failed, DagNodeTransitionReason::Failed);
    context
        .state
        .pending_errors
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(node_id);
    Ok(())
}

fn start_spec(context: &DagSchedulerContext, node_id: &DagNodeId) -> ManagerStartSpec {
    let record = context.journal.snapshot();
    let node = node_by_id(&record, node_id).expect("scheduled node exists");
    let persisted = context
        .definition_nodes
        .get(node_id)
        .expect("persisted definition for scheduled node");
    let execution_mode = context.execution_mode_agents.as_ref().map(|agents| {
        resolve_dag_node_execution_mode(&DagExecutionModeSources {
            route: &node.route,
            agents,
            config_mode: context.execution_mode_config,
        })
    });
    let (category, subagent_type, model) = match &node.route {
        DagRoute::Category { category } => (Some(category.clone()), None, None),
        DagRoute::Agent { agent, model } => (None, Some(agent.clone()), model.clone()),
    };
    ManagerStartSpec {
        prompt: persisted.effective_prompt.clone(),
        task_summary: persisted.task_summary.clone(),
        parent_session_id: record.parent_session_id.clone(),
        root_session_id: Some(record.root_session_id.clone()),
        depth: context.ancestry_depth.unwrap_or(0) + 1,
        category,
        subagent_type,
        model,
        execution_mode,
        name: Some(node.id.clone()),
        description: persisted.description.clone(),
        run_in_background: true,
        ..ManagerStartSpec::default()
    }
}

fn owner(context: &DagSchedulerContext, node_id: &DagNodeId) -> DagTaskOwner {
    let record = context.journal.snapshot();
    DagTaskOwner {
        kind: DagOwnerKind::Dag,
        run_id: record.run_id,
        node_id: node_id.clone(),
        fingerprint: dag_fingerprint(&serde_json::json!({
            "definitionFingerprint": record.definition_fingerprint,
            "nodeId": node_id,
        })),
    }
}

fn start_failure(result: &StartResult) -> (DagNodeErrorCode, String) {
    match result {
        StartResult::DepthDenied { reason, .. } => (DagNodeErrorCode::DepthDenied, reason.clone()),
        StartResult::PlanUnresolved(error) => (DagNodeErrorCode::PlanUnresolved, error.message.clone()),
        StartResult::StartFailed(StartFailure { error_message, .. }) => {
            (DagNodeErrorCode::StartFailed, error_message.clone())
        }
        StartResult::ResidencyDenied { reason } => (DagNodeErrorCode::ResidencyDenied, reason.clone()),
        StartResult::Started(_) => unreachable!("Started never reaches start_failure"),
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

fn transitioned_node(
    node: &DagNode,
    state: DagNodeState,
    at: &str,
    error: Option<&DagNodeError>,
) -> DagNode {
    DagNode {
        state,
        started_at: if state == DagNodeState::Running && node.started_at.is_none() {
            Some(at.to_string())
        } else {
            node.started_at.clone()
        },
        completed_at: if is_terminal_node_state(state) {
            Some(at.to_string())
        } else {
            node.completed_at.clone()
        },
        error: error.cloned().or_else(|| node.error.clone()),
        ..node.clone()
    }
}

fn node_error(
    node_id: &DagNodeId,
    code: DagNodeErrorCode,
    message: &str,
    now: &Arc<dyn Fn() -> i64 + Send + Sync>,
) -> DagNodeError {
    DagNodeError {
        code,
        message: message.to_string(),
        node_id: Some(node_id.clone()),
        at: crate::shared::iso_from_ms(now()),
    }
}

fn primary_failure(record: &DagRunRecordV1) -> Option<DagNode> {
    for wave in &record.waves {
        for node_id in &wave.node_ids {
            if let Ok(node) = node_by_id(record, node_id)
                && node.state == DagNodeState::Failed
            {
                return Some(node);
            }
        }
    }
    None
}

fn node_by_id(record: &DagRunRecordV1, node_id: &DagNodeId) -> Result<DagNode, crate::dag::store::DagStoreError> {
    record
        .nodes
        .iter()
        .find(|node| &node.id == node_id)
        .cloned()
        .ok_or_else(|| crate::dag::store::DagStoreError::Message(format!("unknown DAG node \"{node_id}\"")))
}

fn count_nodes(nodes: &[DagNode]) -> DagNodeCounts {
    let mut counts = DagNodeCounts {
        total: nodes.len(),
        ..DagNodeCounts::default()
    };
    for node in nodes {
        match node.state {
            DagNodeState::Pending => counts.pending += 1,
            DagNodeState::Blocked => counts.blocked += 1,
            DagNodeState::Scheduled => counts.scheduled += 1,
            DagNodeState::Running => counts.running += 1,
            DagNodeState::Completed => counts.completed += 1,
            DagNodeState::Failed => counts.failed += 1,
            DagNodeState::Cancelled => counts.cancelled += 1,
            DagNodeState::Skipped => counts.skipped += 1,
        }
    }
    counts
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
