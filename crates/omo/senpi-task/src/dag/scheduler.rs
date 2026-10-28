//! `dag/scheduler.ts`: the admission loop, event reducer, and task outcome folding stay together
//! so the strict barrier cannot be bypassed by callers.
// allow: SIZE_OK - the admission loop, event reducer, and task outcome folding stay together so
// the strict barrier cannot be bypassed by callers.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use sha2::{Digest, Sha256};

use crate::agents::AgentDefinition;
use crate::dag::execution_mode::{DagExecutionModeSources, resolve_dag_node_execution_mode};
use crate::dag::fingerprint::dag_fingerprint;
use crate::dag::infrastructure_retry::{
    InfrastructureRetries, RetrySchedule, create_infrastructure_retries,
};
use crate::dag::journal::{
    DagJournal, DagJournalListener, DagJournalOptions, DagJournalUnsubscribe, create_dag_journal,
};
use crate::dag::manager::{DagPersistedNode, DagRunRecordV1};
use crate::dag::node_retry::{DagRunReentry, retry_dag_nodes};
use crate::dag::node_send::{DagNodeSendResult, send_to_dag_node};
use crate::dag::owner::{DagOwnerKind, DagTaskOwner, OwnedStartResult};
use crate::dag::results::{
    DagNodeResultArtifact, DagNodeResultPersistInput, DagNodeResultPersistOutcome,
    parse_stats_sidecar, persist_dag_node_result,
};
use crate::dag::store::DagFileStore;
use crate::dag::types::{
    DagDiagnostic, DagNode, DagNodeCounts, DagNodeError, DagNodeErrorCode, DagNodeId, DagNodeState,
    DagNodeTransitionReason, DagRoute, DagRunEvent, DagRunEventPayload, DagRunId,
};
use crate::manager::TaskManager;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{ManagerStartSpec, ResidencyDeniedCause, StartFailure, StartResult};
use crate::state::{TaskRecord, TaskRunStats, TaskStatus};
use crate::steering::CancelOptions;

fn is_terminal_node_state(state: DagNodeState) -> bool {
    matches!(
        state,
        DagNodeState::Completed
            | DagNodeState::Failed
            | DagNodeState::Cancelled
            | DagNodeState::Skipped
    )
}

pub struct DagSchedulerOptions {
    pub store: Arc<DagFileStore>,
    pub task_manager: Arc<TaskManager>,
    pub initial_record: DagRunRecordV1,
    pub execution_mode: Option<DagExecutionModeOptions>,
    pub ancestry: Option<DagSchedulerAncestry>,
    pub node_spawn_policy: Option<DagNodeSpawnPolicy>,
    pub subscriber_ring: Option<usize>,
    pub pre_attached_tasks: Option<BTreeMap<DagNodeId, String>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Injectable retry-backoff timer; defaults to a real timer thread.
    pub resume_credential: Option<crate::pause::admission_barrier::ResumeCredential>,
    pub schedule_retry: Option<RetrySchedule>,
}

impl Clone for DagSchedulerOptions {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            task_manager: Arc::clone(&self.task_manager),
            initial_record: self.initial_record.clone(),
            execution_mode: self
                .execution_mode
                .as_ref()
                .map(|mode| DagExecutionModeOptions {
                    agents: Arc::clone(&mode.agents),
                    config: mode.config,
                    auto_mode: mode.auto_mode,
                    config_is_auto: mode.config_is_auto,
                }),
            ancestry: self.ancestry,
            node_spawn_policy: self.node_spawn_policy.clone(),
            subscriber_ring: self.subscriber_ring,
            pre_attached_tasks: self.pre_attached_tasks.clone(),
            now: self.now.clone(),
            resume_credential: self.resume_credential.clone(),
            schedule_retry: self.schedule_retry.clone(),
        }
    }
}

pub struct DagExecutionModeOptions {
    pub agents: Arc<BTreeMap<String, AgentDefinition>>,
    pub config: Option<ExecutionMode>,
    pub auto_mode: Option<ExecutionMode>,
    pub config_is_auto: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DagSchedulerAncestry {
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagNodeSpawnPolicyVerdict {
    Allow,
    Deny { message: String },
    Force { prompt: String },
}

pub type DagNodeSpawnPolicy =
    Arc<dyn Fn(&DagNodeId, &str, &str, &str) -> DagNodeSpawnPolicyVerdict + Send + Sync>;

enum SchedulerWake {
    Settled(DagNodeId, String, AttachedTaskSettlement),
    ResidencyReleased(u64),
    Wake,
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
    wake: Condvar,
    settlement_tx: Sender<SchedulerWake>,
    settlement_rx: Mutex<std::sync::mpsc::Receiver<SchedulerWake>>,
}

pub struct DagSchedulerContext {
    task_manager: Arc<TaskManager>,
    #[cfg(test)]
    task_port: Mutex<Option<Arc<dyn TestTaskPort>>>,
    journal: Arc<DagJournal<DagRunRecordV1>>,
    definition_nodes: BTreeMap<DagNodeId, DagPersistedNode>,
    execution_mode_agents: Option<Arc<BTreeMap<String, AgentDefinition>>>,
    execution_mode_config: Option<ExecutionMode>,
    execution_mode_auto: Option<ExecutionMode>,
    execution_mode_is_auto: bool,
    ancestry: Option<DagSchedulerAncestry>,
    node_spawn_policy: Option<DagNodeSpawnPolicy>,
    residency_queued: Mutex<std::collections::BTreeSet<DagNodeId>>,
    pending_admission: Mutex<Vec<DagNodeId>>,
    /// Recovery-reattached children whose settlement waits start with the run loop.
    pre_attached: Mutex<Vec<(DagNodeId, String)>>,
    /// Generation of the residency release armed by the current admission pass; a release from an
    /// earlier pass is stale (TS races only the promise the current pass armed).
    residency_generation: std::sync::atomic::AtomicU64,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    infrastructure_retries: InfrastructureRetries,
    state: Arc<SchedulerState>,
    options: DagSchedulerOptions,
}

pub fn create_dag_scheduler(
    options: DagSchedulerOptions,
) -> Result<Arc<DagSchedulerContext>, crate::dag::store::DagStoreError> {
    let now = options.now.clone().unwrap_or_else(|| {
        Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX))
    });
    let mut options = options;
    options.now = Some(Arc::clone(&now));
    let (settlement_tx, settlement_rx) = channel();
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
        wake: Condvar::new(),
        settlement_tx,
        settlement_rx: Mutex::new(settlement_rx),
    });
    let store = Arc::clone(&options.store);
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
                &reducer_state
                    .pending_errors
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
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
    let pre_attached_tasks = options.pre_attached_tasks.clone().unwrap_or_default();
    let task_manager_key = Arc::as_ptr(&options.task_manager) as usize;
    let wake_tx = state.settlement_tx.clone();
    let infrastructure_retries = create_infrastructure_retries(
        &options,
        Arc::clone(&journal),
        Arc::clone(&now),
        Arc::new(move || {
            let _ = wake_tx.send(SchedulerWake::Wake);
        }),
    )?;
    let context = Arc::new(DagSchedulerContext {
        task_manager: Arc::clone(&options.task_manager),
        #[cfg(test)]
        task_port: Mutex::new(None),
        journal,
        definition_nodes,
        execution_mode_agents: options
            .execution_mode
            .as_ref()
            .map(|mode| Arc::clone(&mode.agents)),
        execution_mode_config: options.execution_mode.as_ref().and_then(|mode| mode.config),
        execution_mode_auto: options
            .execution_mode
            .as_ref()
            .and_then(|mode| mode.auto_mode),
        execution_mode_is_auto: options
            .execution_mode
            .as_ref()
            .is_some_and(|mode| mode.config_is_auto),
        ancestry: options.ancestry,
        node_spawn_policy: options.node_spawn_policy.clone(),
        residency_queued: Mutex::new(std::collections::BTreeSet::new()),
        pending_admission: Mutex::new(Vec::new()),
        pre_attached: Mutex::new(Vec::new()),
        residency_generation: std::sync::atomic::AtomicU64::new(0),
        now,
        infrastructure_retries,
        state,
        options: DagSchedulerOptions {
            task_manager: Arc::clone(&options.task_manager),
            node_spawn_policy: options.node_spawn_policy.clone(),
            ..options
        },
    });
    for (node_id, task_id) in pre_attached_tasks {
        context
            .state
            .attached_task_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node_id.clone(), task_id.clone());
        let snapshot = context.journal.snapshot();
        let node = node_by_id(&snapshot, &node_id).ok();
        if let Some(node) = node
            && node.task_id.as_deref() != Some(&task_id)
        {
            let _ = context
                .journal
                .append(DagRunEventPayload::NodeTaskAttached {
                    node_id: node_id.clone(),
                    task_id: task_id.clone(),
                    attempt: node.attempt,
                });
        }
        context
            .pre_attached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((node_id, task_id));
    }
    if let Some(registry) = DAG_SCHEDULER_OBSERVERS.get() {
        let observers: Vec<_> = registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(registered, _)| *registered == task_manager_key)
            .map(|(_, observer)| Arc::clone(observer))
            .collect();
        for observer in observers {
            observer(Arc::clone(&context));
        }
    }
    Ok(context)
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
    if let Some(updated) = crate::dag::manager::apply_paused_dag_event(record, event)
        .or_else(|| crate::dag::manager::apply_run_pause(record, event))
    {
        return updated;
    }
    match &event.payload {
        DagRunEventPayload::RunPaused { .. } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Paused,
            updated_at: event.at.clone(),
            ..record.clone()
        },
        DagRunEventPayload::RunResumed { generation } => DagRunRecordV1 {
            status: crate::dag::types::DagRunStatus::Running,
            generation: *generation,
            completed_at: None,
            updated_at: event.at.clone(),
            ..record.clone()
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
        } => with_nodes(
            record,
            record
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
            record.diagnostics.clone(),
            event.at.clone(),
        ),
        DagRunEventPayload::NodeSteered {
            node_id,
            task_id,
            delivery,
        } => with_nodes(
            record,
            record
                .nodes
                .iter()
                .map(|node| {
                    if &node.id == node_id
                        && *delivery == crate::dag::types::DagNodeDelivery::Revive
                    {
                        DagNode {
                            task_id: Some(task_id.clone()),
                            ..node.clone()
                        }
                    } else {
                        node.clone()
                    }
                })
                .collect(),
            record.diagnostics.clone(),
            event.at.clone(),
        ),
        DagRunEventPayload::NodeRetried {
            node_id,
            exec_attempt,
            ..
        } => with_nodes(
            record,
            record
                .nodes
                .iter()
                .map(|node| {
                    if &node.id == node_id {
                        DagNode {
                            state: DagNodeState::Pending,
                            exec_attempt: Some(*exec_attempt),
                            error: None,
                            completed_at: None,
                            output: None,
                            output_bytes: None,
                            run_stats: None,
                            ..node.clone()
                        }
                    } else {
                        node.clone()
                    }
                })
                .collect(),
            record.diagnostics.clone(),
            event.at.clone(),
        ),
        DagRunEventPayload::DefinitionAmended {
            fingerprint,
            invalidated_node_ids,
            ..
        } => {
            let mut amended = with_nodes(
                record,
                record
                    .nodes
                    .iter()
                    .map(|node| {
                        if invalidated_node_ids.contains(&node.id) {
                            DagNode {
                                state: DagNodeState::Pending,
                                error: None,
                                completed_at: None,
                                output: None,
                                output_bytes: None,
                                run_stats: None,
                                ..node.clone()
                            }
                        } else {
                            node.clone()
                        }
                    })
                    .collect(),
                record.diagnostics.clone(),
                event.at.clone(),
            );
            amended.definition_fingerprint = fingerprint.clone();
            amended
        }
        _ => DagRunRecordV1 {
            updated_at: event.at.clone(),
            ..record.clone()
        },
    }
}

/// The next record with replaced nodes/diagnostics: field-by-field so the superseded `nodes` and
/// `diagnostics` are not cloned only to be dropped (`..record.clone()` would copy them first).
fn with_nodes(
    record: &DagRunRecordV1,
    nodes: Vec<DagNode>,
    diagnostics: Vec<DagDiagnostic>,
    updated_at: String,
) -> DagRunRecordV1 {
    DagRunRecordV1 {
        schema_version: record.schema_version,
        checkpoint_seq: record.checkpoint_seq,
        run_id: record.run_id.clone(),
        run_key: record.run_key.clone(),
        name: record.name.clone(),
        parent_session_id: record.parent_session_id.clone(),
        root_session_id: record.root_session_id.clone(),
        definition_fingerprint: record.definition_fingerprint.clone(),
        definition: record.definition.clone(),
        status: record.status,
        generation: record.generation,
        created_at: record.created_at.clone(),
        updated_at,
        started_at: record.started_at.clone(),
        completed_at: record.completed_at.clone(),
        nodes,
        edges: record.edges.clone(),
        waves: record.waves.clone(),
        critical_path: record.critical_path.clone(),
        bottlenecks: record.bottlenecks.clone(),
        diagnostics,
        amend_history: record.amend_history.clone(),
        lease_holder_pid: record.lease_holder_pid,
        previous_lease_holder_pid: record.previous_lease_holder_pid,
        guidance: record.guidance.clone(),
        retry_on_resume: record.retry_on_resume.clone(),
        cancel_requested_nodes: record.cancel_requested_nodes.clone(),
        paused_child_acks: record.paused_child_acks.clone(),
        pause: record.pause.clone(),
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
        // Taken, not cloned: the entry is removed below either way, and it carries the full
        // `final_response`.
        pending_terminal_results.and_then(|results| {
            results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(node_id)
        })
    } else {
        None
    };
    let replayed = if terminal_result.is_none() && store.is_some() && to == DagNodeState::Completed
    {
        store.and_then(|store| replay_dag_node_result(store, &record.run_id, node_id))
    } else {
        None
    };
    let persisted =
        if let (Some(task_record), Some(store), Some(now)) = (&terminal_result, store, now) {
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
    let mut result_preview: Option<(String, u64)> = None;
    match persisted {
        Some(DagNodeResultPersistOutcome::Persisted { artifact }) => {
            run_stats = task_run_stats_of(&terminal_result);
            // The artifact was just written from `final_response`; preview it without re-reading.
            let output = terminal_result
                .as_ref()
                .and_then(|record| record.final_response.as_deref())
                .unwrap_or_default();
            result_preview = Some((output_preview(output), artifact.bytes));
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
                result_preview = Some((output_preview(&replayed.output), replayed.artifact.bytes));
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
            let mut transitioned =
                transitioned_node(node, to, &event.at, pending_errors.get(node_id));
            if result_artifact.is_some() {
                transitioned.run_stats = run_stats.clone().or(transitioned.run_stats);
                if let Some((preview, bytes)) = &result_preview {
                    transitioned.output = Some(preview.clone());
                    transitioned.output_bytes = Some(*bytes);
                }
            }
            transitioned
        })
        .collect();
    with_nodes(record, nodes, diagnostics, event.at.clone())
}

fn task_run_stats_of(terminal_result: &Option<TaskRecord>) -> Option<TaskRunStats> {
    terminal_result
        .as_ref()
        .and_then(|record| record.run_stats.clone())
}

fn output_preview(output: &str) -> String {
    output
        .chars()
        .take(crate::dag::types::DAG_NODE_OUTPUT_PREVIEW_CHARS)
        .collect()
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
    output: String,
}

fn replay_dag_node_result(
    store: &DagFileStore,
    run_id: &DagRunId,
    node_id: &DagNodeId,
) -> Option<ReplayedResult> {
    let output_path = store.paths.result(run_id, node_id);
    let output = fs::read_to_string(&output_path).ok()?;
    let stats_path = stats_sidecar_path(&output_path);
    // PERF-DIVERGENCE(senpi dag-results.ts): upstream reads the stats sidecar once for run_stats and again for its artifact hash; this port reads the file once and derives both from the same bytes. Unreadable file still yields None for both.
    let (run_stats, stats) = match fs::read(&stats_path) {
        Ok(bytes) => (
            parse_stats_sidecar(&String::from_utf8_lossy(&bytes)),
            std::str::from_utf8(&bytes)
                .ok()
                .map(|_| artifact_ref_from(&store.state_dir, &stats_path, &bytes)),
        ),
        Err(_) => (None, None),
    };
    Some(ReplayedResult {
        artifact: DagNodeResultArtifact {
            stats,
            ..artifact_ref(&store.state_dir, &output_path, output.as_bytes())
        },
        run_stats,
        output,
    })
}

fn stats_sidecar_path(output_path: &Path) -> std::path::PathBuf {
    let stem = output_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output_path.with_file_name(format!("{stem}.stats.json"))
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
    pub fn admission_is_stopped(&self) -> bool {
        admission_stopped(self)
    }
    pub fn stop_admission(&self) {
        self.state
            .admission_stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        when_admission_idle(self);
        let _ = self.state.settlement_tx.send(SchedulerWake::Wake);
        let mut running = self
            .state
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while *running {
            self.state.wake.notify_all();
            running = self
                .state
                .run_idle
                .wait(running)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
    #[cfg(test)]
    pub(crate) fn set_task_port(&self, port: Arc<dyn TestTaskPort>) {
        *self.task_port.lock().unwrap() = Some(port);
    }

    pub fn run(&self) -> Result<DagRunRecordV1, crate::dag::store::DagStoreError> {
        {
            let mut running = self
                .state
                .running
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // TS `run: () => running ??= runFrontier(context)`: a concurrent caller joins the
            // in-flight frontier loop and settles with it instead of failing.
            if *running {
                while *running {
                    running = self
                        .state
                        .run_idle
                        .wait(running)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                return Ok(self.journal.snapshot());
            }
            if admission_stopped(self) {
                return Ok(self.journal.snapshot());
            }
            *running = true;
        }
        struct RunGuard<'a>(&'a SchedulerState);
        impl Drop for RunGuard<'_> {
            fn drop(&mut self) {
                *self
                    .0
                    .running
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = false;
                self.0.run_idle.notify_all();
            }
        }
        let _running = RunGuard(&self.state);
        let pre_attached = std::mem::take(
            &mut *self
                .pre_attached
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (node_id, task_id) in pre_attached {
            spawn_wait_for(self, node_id, task_id);
        }
        run_frontier(self)
    }

    pub fn suspend(&self) {
        self.state
            .admission_stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.infrastructure_retries.cancel_pending_timers();
        self.state.admission_condvar.notify_all();
        let _ = self.state.settlement_tx.send(SchedulerWake::Wake);
        self.state.wake.notify_all();
        when_admission_idle(self);
        self.journal.when_idle();
    }

    pub fn retry_node(
        &self,
        run_id: DagRunId,
        node_ids: Option<&[DagNodeId]>,
        retry_options: Option<crate::dag::node_retry::DagRetryOptions>,
    ) -> Result<DagRunReentry, DagNodeControlError> {
        retry_dag_nodes(self.options.clone(), run_id, node_ids, retry_options)
    }

    pub fn send_to_node(
        &self,
        run_id: &DagRunId,
        node_id: &DagNodeId,
        message: &str,
    ) -> std::thread::JoinHandle<Result<DagNodeSendResult, DagNodeControlError>> {
        let options = self.options.clone();
        let run_id = run_id.clone();
        let node_id = node_id.clone();
        let message = message.to_string();
        std::thread::spawn(move || send_to_dag_node(&options, &run_id, &node_id, &message))
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

type DagSchedulerObserver = Arc<dyn Fn(Arc<DagSchedulerContext>) + Send + Sync>;
static DAG_SCHEDULER_OBSERVERS: std::sync::OnceLock<Mutex<Vec<(usize, DagSchedulerObserver)>>> =
    std::sync::OnceLock::new();
pub use crate::dag::node_retry::{
    DagRetryOptions as DagSchedulerRetryOptions, DagRunReentry as DagSchedulerRunReentry,
    reenter_dag_run,
};
pub use crate::dag::node_send::{
    DagNodeDeliveryResult as DagSchedulerNodeDeliveryResult,
    DagNodeSendResult as DagSchedulerNodeSendResult,
};
pub fn observe_dag_schedulers(
    task_manager: &Arc<TaskManager>,
    observer: Arc<dyn Fn(Arc<DagSchedulerContext>) + Send + Sync>,
) -> Box<dyn FnOnce() + Send> {
    let key = Arc::as_ptr(task_manager) as usize;
    DAG_SCHEDULER_OBSERVERS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((key, observer));
    Box::new(move || {
        DAG_SCHEDULER_OBSERVERS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(registered, _)| *registered != key)
    })
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
    context.infrastructure_retries.cancel_pending_timers();
    context.state.cancellation_condvar.notify_all();
    context.state.admission_condvar.notify_all();
    context.state.wake.notify_all();
    let _ = context.state.settlement_tx.send(SchedulerWake::Wake);
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
        if let Err(error) = cancel_task(context, task_id, reason)
            && cancellation_failure.is_none()
        {
            cancellation_failure = Some(error.to_string());
        }
    }
    for node in context.journal.snapshot().nodes {
        if is_terminal_node_state(node.state) {
            continue;
        }
        transition_from(
            context,
            &node.id,
            node.state,
            DagNodeState::Cancelled,
            DagNodeTransitionReason::Cancelled,
        );
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

fn run_frontier(
    context: &DagSchedulerContext,
) -> Result<DagRunRecordV1, crate::dag::store::DagStoreError> {
    let initial = context.journal.snapshot();
    if admission_stopped(context) || initial.status.is_terminal() {
        return Ok(initial);
    }
    if initial.status == crate::dag::types::DagRunStatus::Pending {
        let generation = initial.generation;
        context
            .journal
            .append(DagRunEventPayload::RunStarted { generation })?;
    }
    let mut emitted_admissions = std::collections::BTreeSet::new();
    let mut emitted_completions = std::collections::BTreeSet::new();
    loop {
        if admission_stopped(context) {
            return Ok(context.journal.snapshot());
        }
        if cancellation_started(context) {
            return Ok(cancelled_snapshot(context));
        }
        // The skip cascade runs only at frontier quiescence (nothing attached): a failed node stays
        // revivable while any sibling is still mid-flight.
        if attached_is_empty(context) {
            apply_dependent_skip_cascade(context)?;
        }
        // Completions are reported before the next admission pass so a finishing wave reads as
        // settled before later-wave nodes it unblocked start interleaving their own events.
        emit_completed_waves(context, &emitted_admissions, &mut emitted_completions)?;
        if !admit_frontier(context, &mut emitted_admissions)? {
            if admission_stopped(context) {
                return Ok(context.journal.snapshot());
            }
            return Ok(cancelled_snapshot(context));
        }
        let snapshot = context.journal.snapshot();
        if snapshot
            .nodes
            .iter()
            .all(|node| is_terminal_node_state(node.state))
        {
            break;
        }
        if attached_is_empty(context) && !context.infrastructure_retries.pending(None) {
            // A retry timer may fire while admission yields, after its frontier scan.
            if context.infrastructure_retries.take_ready() {
                continue;
            }
            // Every admission of this pass failed at start: the dependents' skip cascade runs at the
            // top of the loop instead of leaving the run live forever.
            if has_cascadable_dependent(&snapshot) {
                continue;
            }
            return Err(crate::dag::store::DagStoreError::Message(format!(
                "DAG run \"{}\" cannot terminalize while nodes are active",
                snapshot.run_id
            )));
        }
        if !settle_one(context, None)? {
            if admission_stopped(context) {
                return Ok(context.journal.snapshot());
            }
            return Ok(cancelled_snapshot(context));
        }
    }

    let snapshot = context.journal.snapshot();
    if let Some(failed) = primary_failure(&snapshot) {
        let error = failed.error.clone().unwrap_or_else(|| {
            node_error(
                &failed.id,
                DagNodeErrorCode::StartFailed,
                "DAG node failed",
                &context.now,
            )
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

fn attached_is_empty(context: &DagSchedulerContext) -> bool {
    context
        .state
        .attached_task_ids
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_empty()
}

/// A waiting node whose dependency ended in any terminal state other than completed can never run.
fn has_cascadable_dependent(record: &DagRunRecordV1) -> bool {
    record.nodes.iter().any(|node| {
        matches!(node.state, DagNodeState::Pending | DagNodeState::Blocked)
            && node.depends_on.iter().any(|dependency| {
                node_by_id(record, dependency).is_ok_and(|dependency| {
                    is_terminal_node_state(dependency.state)
                        && dependency.state != DagNodeState::Completed
                })
            })
    })
}

struct ArmedResidency {
    change: crate::manager::residency_signal::ResidencyChange,
    generation: u64,
    #[cfg(test)]
    fake_change: Option<std::sync::mpsc::Receiver<()>>,
}

/// Arms the session-wide residency release BEFORE the admission probe (the wake is edge
/// triggered): a release observed by the waiter thread lands on the settlement channel.
fn arm_residency_release(context: &DagSchedulerContext, parent_session_id: &str) -> ArmedResidency {
    let generation = context
        .residency_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    let change = context.task_manager.residency_changed(parent_session_id);
    #[cfg(test)]
    let fake_change = {
        let port = context
            .task_port
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        port.map(|port| {
            let (tx, rx) = channel();
            port.residency_changed(parent_session_id, tx);
            rx
        })
    };
    ArmedResidency {
        change,
        generation,
        #[cfg(test)]
        fake_change,
    }
}

/// Starts the waiter thread only once a cap denial actually parks admission; the change was
/// captured before the probe, so a release that already fired still wakes immediately.
fn spawn_residency_wake(context: &DagSchedulerContext, armed: &mut ArmedResidency) {
    let wake_tx = context.state.settlement_tx.clone();
    let generation = armed.generation;
    #[cfg(test)]
    let fake_change = armed.fake_change.take();
    let change = armed.change.clone();
    let _ = std::thread::Builder::new()
        .name("dag-residency-wake".into())
        .spawn(move || {
            #[cfg(test)]
            if let Some(receiver) = fake_change {
                let _ = receiver.recv();
            } else {
                change.wait();
            }
            #[cfg(not(test))]
            change.wait();
            let _ = wake_tx.send(SchedulerWake::ResidencyReleased(generation));
        });
}

// One admission pass over the frontier: the ordered residency-denied queue first (oldest denial
// gets the first freed slot), then every newly ready node. Denials park in the queue and retry
// after ANY slot-freeing event in the parent session; a lease denial re-probes immediately.
fn admit_frontier(
    context: &DagSchedulerContext,
    emitted: &mut std::collections::BTreeSet<usize>,
) -> Result<bool, crate::dag::store::DagStoreError> {
    loop {
        if admission_stopped(context) || cancellation_started(context) {
            return Ok(false);
        }
        let denied = std::mem::take(
            &mut *context
                .pending_admission
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let denied_set: std::collections::BTreeSet<_> = denied.iter().cloned().collect();
        let snapshot = context.journal.snapshot();
        let runnable: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|node| {
                !denied_set.contains(&node.id)
                    && !context.infrastructure_retries.pending(Some(&node.id))
                    && is_runnable(&snapshot, &node.id)
            })
            .map(|node| node.id.clone())
            .collect();
        if denied.is_empty() && runnable.is_empty() {
            return Ok(true);
        }
        for node_id in &runnable {
            if let Ok(from) = node_by_id(&snapshot, node_id).map(|node| node.state) {
                transition_from(
                    context,
                    node_id,
                    from,
                    DagNodeState::Scheduled,
                    DagNodeTransitionReason::Scheduled,
                );
            }
        }
        emit_wave_admissions(&snapshot, context, &runnable, emitted)?;
        let mut residency_released = arm_residency_release(context, &snapshot.parent_session_id);
        *context
            .state
            .admission_in_progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        let mut next_denied = Vec::new();
        let mut lease_denied = false;
        // TS Promise.allSettled: every start of the pass settles before any result is folded, so
        // attachments and failures land after the whole admission batch.
        let awaiting: Vec<_> = denied.into_iter().chain(runnable).collect();
        let results: Vec<_> = awaiting
            .into_iter()
            .map(|node_id| {
                let (spec, policy_failure) = start_spec(context, &snapshot, &node_id);
                let result = if let Some(message) = policy_failure {
                    Err(message)
                } else {
                    start_owned(context, &spec, &owner_of(&snapshot, &node_id))
                };
                (node_id, result)
            })
            .collect();
        if admission_stopped(context) && !cancellation_started(context) {
            resolve_admission_idle(context);
            return Ok(false);
        }
        for (node_id, result) in results {
            match result {
                Err(message) => {
                    if !cancellation_started(context) {
                        fail_node(context, &node_id, DagNodeErrorCode::StartFailed, &message)?;
                    }
                }
                Ok(result) if cancellation_started(context) => {
                    if matches!(result, OwnedStartResult::Started { .. }) {
                        attach_or_fail(context, &node_id, result)?;
                    }
                }
                Ok(OwnedStartResult::NotStarted(StartResult::ResidencyDenied {
                    reason,
                    cause,
                })) => match cause {
                    ResidencyDeniedCause::Lease => {
                        lease_denied = true;
                        next_denied.push(node_id);
                    }
                    ResidencyDeniedCause::Residents { residents, .. } if residents.is_empty() => {
                        context
                            .residency_queued
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&node_id);
                        fail_node(
                            context,
                            &node_id,
                            DagNodeErrorCode::ResidencyDenied,
                            &format!(
                                "resident child cap reached and the denial names no resident child that could free a slot: {reason}"
                            ),
                        )?;
                    }
                    ResidencyDeniedCause::Residents { residents, .. } => {
                        let first = context
                            .residency_queued
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(node_id.clone());
                        if first {
                            let held_by_other_owners = {
                                let own_ids = context
                                    .state
                                    .attached_task_ids
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner);
                                residents
                                    .iter()
                                    .filter(|resident| {
                                        !own_ids.values().any(|own| own == &resident.task_id)
                                    })
                                    .count() as u64
                            };
                            context
                                .journal
                                .append(DagRunEventPayload::NodeTransitioned {
                                    node_id: node_id.clone(),
                                    from: DagNodeState::Scheduled,
                                    to: DagNodeState::Scheduled,
                                    reason: DagNodeTransitionReason::ResidencyQueued {
                                        residents: residents.len() as u64,
                                        held_by_other_owners,
                                    },
                                })?;
                        }
                        next_denied.push(node_id);
                    }
                },
                Ok(result) => {
                    context
                        .residency_queued
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&node_id);
                    attach_or_fail(context, &node_id, result)?;
                }
            }
        }
        resolve_admission_idle(context);
        // The task owner record is durable even when admission finishes after suspension; this
        // retired controller must not attach, fail, or promote the node.
        if cancellation_started(context) {
            return Ok(false);
        }
        let parked = !next_denied.is_empty();
        context
            .pending_admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(next_denied);
        if !parked {
            return Ok(true);
        }
        // A contended or displaced admission lease is a bounded wait inside acquisition itself, so
        // the next probe is already paced; only a cap denial has to wait for a slot to free.
        if lease_denied {
            continue;
        }
        spawn_residency_wake(context, &mut residency_released);
        if !settle_one(
            context,
            Some((&residency_released.change, residency_released.generation)),
        )? {
            return Ok(false);
        }
    }
}

// dag.wave.started is informational: one event per wave index touched by this admission pass,
// listing exactly the nodes the pass scheduled.
fn emit_wave_admissions(
    snapshot: &DagRunRecordV1,
    context: &DagSchedulerContext,
    scheduled: &[DagNodeId],
    emitted: &mut std::collections::BTreeSet<usize>,
) -> Result<(), crate::dag::store::DagStoreError> {
    if scheduled.is_empty() {
        return Ok(());
    }
    let mut groups: BTreeMap<usize, Vec<DagNodeId>> = BTreeMap::new();
    for node_id in scheduled {
        if let Some(wave) = snapshot
            .waves
            .iter()
            .find(|wave| wave.node_ids.contains(node_id))
        {
            groups.entry(wave.index).or_default().push(node_id.clone());
        }
    }
    for (index, node_ids) in groups {
        context.journal.append(DagRunEventPayload::WaveStarted {
            wave_index: index,
            node_ids,
        })?;
        emitted.insert(index);
    }
    Ok(())
}

// dag.wave.completed is informational and once per instance per wave index: it fires when EVERY
// member of the compiled wave holds a terminal state. Waves this instance never admitted stay silent.
fn emit_completed_waves(
    context: &DagSchedulerContext,
    admitted: &std::collections::BTreeSet<usize>,
    completed: &mut std::collections::BTreeSet<usize>,
) -> Result<(), crate::dag::store::DagStoreError> {
    // `completed` only ever holds admitted indices, so equal sizes mean no wave can newly complete.
    if completed.len() >= admitted.len() {
        return Ok(());
    }
    let snapshot = context.journal.snapshot();
    for wave in &snapshot.waves {
        if !admitted.contains(&wave.index) || completed.contains(&wave.index) {
            continue;
        }
        if !wave.node_ids.iter().all(|id| {
            node_by_id(&snapshot, id).is_ok_and(|node| is_terminal_node_state(node.state))
        }) {
            continue;
        }
        context.journal.append(DagRunEventPayload::WaveCompleted {
            wave_index: wave.index,
            node_ids: wave.node_ids.clone(),
        })?;
        completed.insert(wave.index);
    }
    Ok(())
}

fn attach_or_fail(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    result: OwnedStartResult,
) -> Result<(), crate::dag::store::DagStoreError> {
    match result {
        OwnedStartResult::Started { task, reused } => {
            attach_started(context, node_id, &task, reused)
        }
        OwnedStartResult::OwnerConflict { task_id, .. } => {
            #[cfg(test)]
            let record = context
                .task_port
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|port| port.get_task(&task_id))
                .or_else(|| context.task_manager.get(&task_id));
            #[cfg(not(test))]
            let record = context.task_manager.get(&task_id);
            let Some(record) = record else {
                return fail_node(
                    context,
                    node_id,
                    DagNodeErrorCode::StartFailed,
                    &format!("DAG owner-conflict task {task_id} is unavailable"),
                );
            };
            attach_started(
                context,
                node_id,
                &crate::manager::types::StartedTask {
                    task_id,
                    status: record.status,
                    name: record.name.unwrap_or_default(),
                    resolved_model: record.resolved_model,
                    queue_position: None,
                    name_warning: None,
                },
                true,
            )
        }
        OwnedStartResult::Held { pause_seq } => {
            context.journal.append(DagRunEventPayload::RunPaused {
                reason: Some("user_stop".into()),
                pause_seq: Some(pause_seq),
            })?;
            Ok(())
        }
        OwnedStartResult::NotStarted(start_result) => {
            let failure = start_failure(&start_result);
            fail_node(context, node_id, failure.0, &failure.1)?;
            if let StartResult::StartFailed(failed) = &start_result
                && let Some(task) = context.task_manager.get(&failed.task_id)
            {
                context.infrastructure_retries.retry(node_id, &task)?;
            }
            Ok(())
        }
    }
}

fn attach_started(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    task: &crate::manager::types::StartedTask,
    reused: bool,
) -> Result<(), crate::dag::store::DagStoreError> {
    let (attempt, from) = {
        let snapshot = context.journal.snapshot();
        let node = node_by_id(&snapshot, node_id)?;
        (node.attempt + 1, node.state)
    };
    context
        .journal
        .append(DagRunEventPayload::NodeTaskAttached {
            node_id: node_id.clone(),
            task_id: task.task_id.clone(),
            attempt,
        })?;
    if task.status == TaskStatus::Pending {
        if !reused && let Some(queue_position) = task.queue_position {
            context
                .journal
                .append(DagRunEventPayload::NodeTransitioned {
                    node_id: node_id.clone(),
                    from: DagNodeState::Scheduled,
                    to: DagNodeState::Scheduled,
                    reason: DagNodeTransitionReason::TaskQueued {
                        queue_position: queue_position as u64,
                    },
                })?;
        }
    } else if task.status == TaskStatus::Running {
        // `NodeTaskAttached` leaves the node state untouched, so `from` read above still holds.
        transition_from(
            context,
            node_id,
            from,
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
    spawn_wait_for(context, node_id.clone(), task.task_id.clone());
    Ok(())
}

fn spawn_wait_for(context: &DagSchedulerContext, node_id: DagNodeId, task_id: String) {
    let task_manager = Arc::clone(&context.task_manager);
    let state = Arc::clone(&context.state);
    #[cfg(test)]
    let port = context.task_port.lock().unwrap().clone();
    std::thread::spawn(move || {
        #[cfg(test)]
        let result = match port {
            Some(port) => port.wait_for(&task_id),
            None => task_manager
                .wait_for(&task_id, None, None)
                .map_err(|error| error.to_string()),
        };
        #[cfg(not(test))]
        let result = task_manager.wait_for(&task_id, None, None);
        let settlement = match result {
            Ok(record) => AttachedTaskSettlement::Record(Box::new(record)),
            Err(error) => AttachedTaskSettlement::Error(error.to_string()),
        };
        let _ = state
            .settlement_tx
            .send(SchedulerWake::Settled(node_id, task_id, settlement));
        state.wake.notify_all();
    });
}

#[cfg(test)]
pub(crate) trait TestTaskPort: Send + Sync {
    fn start_owned(
        &self,
        spec: &ManagerStartSpec,
        owner: &DagTaskOwner,
    ) -> Result<OwnedStartResult, String>;
    fn wait_for(&self, task_id: &str) -> Result<TaskRecord, String>;
    fn cancel_task(&self, task_id: &str) -> Result<(), String>;
    fn residency_changed(&self, _parent_session_id: &str, _sender: Sender<()>) {}
    fn get_task(&self, _task_id: &str) -> Option<TaskRecord> {
        None
    }
}

fn start_owned(
    context: &DagSchedulerContext,
    spec: &ManagerStartSpec,
    owner: &DagTaskOwner,
) -> Result<OwnedStartResult, String> {
    #[cfg(test)]
    if let Some(port) = context.task_port.lock().unwrap().clone() {
        return port.start_owned(spec, owner);
    }
    Ok(context.task_manager.start_owned(spec, owner))
}

fn cancel_task(
    context: &DagSchedulerContext,
    task_id: &str,
    reason: Option<&str>,
) -> Result<(), String> {
    #[cfg(test)]
    if let Some(port) = context.task_port.lock().unwrap().clone() {
        return port.cancel_task(task_id);
    }
    context
        .task_manager
        .cancel_task(
            task_id,
            reason,
            CancelOptions {
                abort: crate::steering::CancelAbort::Skip,
                expected_run_epoch: None,
            },
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

// Waits for the next settle-loop wake: one of this run's own children settling (folded here),
// cancellation/suspension, or - when admission parked a residency denial - the session-wide
// residency release the caller armed before its probe. Returns true when the caller should run
// another admission pass, false when the run is cancelled or suspended.
fn settle_one(
    context: &DagSchedulerContext,
    residency_released: Option<(&crate::manager::residency_signal::ResidencyChange, u64)>,
) -> Result<bool, crate::dag::store::DagStoreError> {
    loop {
        if admission_stopped(context) || cancellation_started(context) {
            return Ok(false);
        }
        if context.infrastructure_retries.take_ready() {
            return Ok(true);
        }
        if residency_released.is_some_and(|(change, _)| change.is_fired()) {
            return Ok(true);
        }
        let wake = context
            .state
            .settlement_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv();
        match wake {
            Ok(SchedulerWake::Settled(node_id, task_id, settlement)) => {
                // A settlement for a task no longer attached to the node (a duplicate or a stale
                // attempt) is not this run's wake.
                let current = context
                    .state
                    .attached_task_ids
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&node_id)
                    .cloned();
                if current.as_deref() != Some(task_id.as_str()) {
                    continue;
                }
                if admission_stopped(context) || cancellation_started(context) {
                    return Ok(false);
                }
                context
                    .state
                    .attached_task_ids
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&node_id);
                match settlement {
                    AttachedTaskSettlement::Error(message) => {
                        fail_node(context, &node_id, DagNodeErrorCode::TaskError, &message)?
                    }
                    AttachedTaskSettlement::Record(record) => {
                        fold_task_outcome(context, &node_id, *record)?
                    }
                }
                return Ok(true);
            }
            Ok(SchedulerWake::ResidencyReleased(generation)) => {
                if residency_released.is_some_and(|(_, armed)| armed == generation) {
                    return Ok(true);
                }
            }
            Ok(SchedulerWake::Wake) => {}
            Err(_) => return Ok(false),
        }
    }
}

fn admission_stopped(context: &DagSchedulerContext) -> bool {
    context
        .state
        .admission_stopped
        .load(std::sync::atomic::Ordering::SeqCst)
}

fn fold_task_outcome(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    task: TaskRecord,
) -> Result<(), crate::dag::store::DagStoreError> {
    if matches!(
        task.status,
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused
    ) {
        return fail_node(
            context,
            node_id,
            DagNodeErrorCode::TaskError,
            &format!(
                "TaskManager.wait_for returned nonterminal task {}",
                task.task_id
            ),
        );
    }
    let status = task.status;
    let error_message = task.error_message.clone();
    let retry_candidate = task.clone();
    context
        .state
        .pending_terminal_results
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(node_id.clone(), task);
    if status == TaskStatus::Completed {
        transition(
            context,
            node_id,
            DagNodeState::Completed,
            DagNodeTransitionReason::Succeeded,
        );
        return Ok(());
    }
    let failure = task_failure(status, error_message.as_deref());
    fail_node(context, node_id, failure.0, &failure.1)?;
    context
        .infrastructure_retries
        .retry(node_id, &retry_candidate)
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
                        is_terminal_node_state(dependency.state)
                            && dependency.state != DagNodeState::Completed
                    })
                    .unwrap_or(false)
            });
            if should_skip {
                transition_from(
                    context,
                    &node.id,
                    node.state,
                    DagNodeState::Skipped,
                    DagNodeTransitionReason::Skipped,
                );
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
    let Ok(from) = node_by_id(&context.journal.snapshot(), node_id).map(|node| node.state) else {
        return;
    };
    transition_from(context, node_id, from, to, reason);
}

/// `transition` for a caller that already read the node's current state from a snapshot: skips the
/// full-record snapshot clone `transition` pays per call.
fn transition_from(
    context: &DagSchedulerContext,
    node_id: &DagNodeId,
    from: DagNodeState,
    to: DagNodeState,
    reason: DagNodeTransitionReason,
) {
    let _ = context
        .journal
        .append(DagRunEventPayload::NodeTransitioned {
            node_id: node_id.clone(),
            from,
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
        .insert(
            node_id.clone(),
            node_error(node_id, code, message, &context.now),
        );
    transition(
        context,
        node_id,
        DagNodeState::Failed,
        DagNodeTransitionReason::Failed,
    );
    context
        .state
        .pending_errors
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(node_id);
    Ok(())
}

fn start_spec(
    context: &DagSchedulerContext,
    record: &DagRunRecordV1,
    node_id: &DagNodeId,
) -> (ManagerStartSpec, Option<String>) {
    let node = node_by_id(record, node_id).expect("scheduled node exists");
    let parent_session_id = record.parent_session_id.clone();
    let root_session_id = record.root_session_id.clone();
    let policy_parent_session_id = parent_session_id.clone();
    let persisted = context
        .definition_nodes
        .get(node_id)
        .expect("persisted definition for scheduled node");
    let execution_mode = context.execution_mode_agents.as_ref().and_then(|agents| {
        resolve_dag_node_execution_mode(&DagExecutionModeSources {
            route: &node.route,
            agents,
            config_mode: context.execution_mode_config,
            config_is_auto: context.execution_mode_is_auto,
            auto_mode: context.execution_mode_auto,
        })
    });
    let (category, subagent_type, model) = match &node.route {
        DagRoute::Category { category } => (Some(category.clone()), None, None),
        DagRoute::Agent { agent, model } => (None, Some(agent.clone()), model.clone()),
    };
    let mut spec = ManagerStartSpec {
        resume_credential: context.options.resume_credential.clone(),
        prompt: persisted.effective_prompt.clone(),
        task_summary: persisted.task_summary.clone(),
        parent_session_id: parent_session_id.clone(),
        root_session_id: Some(root_session_id),
        depth: context.ancestry.map_or(0, |ancestry| ancestry.depth) + 1,
        category,
        subagent_type,
        model,
        execution_mode,
        name: Some(node.id.clone()),
        description: persisted.description.clone(),
        run_in_background: true,
        ..ManagerStartSpec::default()
    };
    let mut policy_failure = None;
    if let (Some(policy), Some(agent)) = (&context.node_spawn_policy, &spec.subagent_type) {
        match policy(node_id, agent, &spec.prompt, &policy_parent_session_id) {
            DagNodeSpawnPolicyVerdict::Allow => {}
            DagNodeSpawnPolicyVerdict::Deny { message } => {
                policy_failure = Some(format!(
                    "DAG node \"{node_id}\" denied by spawn policy: {message}"
                ))
            }
            DagNodeSpawnPolicyVerdict::Force { prompt } => spec.prompt = prompt,
        }
    }
    (spec, policy_failure)
}

#[cfg(test)]
fn owner(context: &DagSchedulerContext, node_id: &DagNodeId) -> DagTaskOwner {
    owner_of(&context.journal.snapshot(), node_id)
}

fn owner_of(record: &DagRunRecordV1, node_id: &DagNodeId) -> DagTaskOwner {
    let exec_attempt = node_by_id(record, node_id)
        .ok()
        .and_then(|node| node.exec_attempt);
    DagTaskOwner {
        kind: DagOwnerKind::Dag,
        run_id: record.run_id.clone(),
        node_id: node_id.clone(),
        fingerprint: dag_fingerprint(&serde_json::json!({
            "definitionFingerprint": record.definition_fingerprint,
            "nodeId": node_id,
            "execAttempt": exec_attempt,
        })),
    }
}

fn start_failure(result: &StartResult) -> (DagNodeErrorCode, String) {
    match result {
        StartResult::Held(_) => (
            DagNodeErrorCode::StartFailed,
            "Task admission is held until the run resumes".into(),
        ),
        StartResult::DepthDenied { reason, .. } => (DagNodeErrorCode::DepthDenied, reason.clone()),
        StartResult::PlanUnresolved(error) => {
            (DagNodeErrorCode::PlanUnresolved, error.message.clone())
        }
        StartResult::StartFailed(StartFailure { error_message, .. }) => {
            (DagNodeErrorCode::StartFailed, error_message.clone())
        }
        StartResult::ResidencyDenied { reason, .. } => {
            (DagNodeErrorCode::ResidencyDenied, reason.clone())
        }
        StartResult::Started(_) => unreachable!("Started never reaches start_failure"),
    }
}

fn task_failure(status: TaskStatus, message: Option<&str>) -> (DagNodeErrorCode, String) {
    match status {
        TaskStatus::Error => (
            DagNodeErrorCode::TaskError,
            message.unwrap_or("task failed").to_string(),
        ),
        TaskStatus::Interrupted => (
            DagNodeErrorCode::TaskInterrupted,
            message.unwrap_or("task was interrupted").to_string(),
        ),
        TaskStatus::Lost => (
            DagNodeErrorCode::TaskLost,
            message.unwrap_or("task was lost").to_string(),
        ),
        TaskStatus::Cancelled => (
            DagNodeErrorCode::TaskCancelled,
            message.unwrap_or("task was cancelled").to_string(),
        ),
        TaskStatus::Completed | TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused => (
            DagNodeErrorCode::TaskError,
            "task in a non-terminal-failure status".to_string(),
        ),
    }
}

fn transitioned_node(
    node: &DagNode,
    state: DagNodeState,
    at: &str,
    error: Option<&DagNodeError>,
) -> DagNode {
    let clear_terminal = state == DagNodeState::Running && is_terminal_node_state(node.state);
    // One clone, mutated in place: struct-update over `node.clone()` copied every overwritten field
    // (output, run_stats, error) just to drop it.
    let mut next = node.clone();
    next.state = state;
    if state == DagNodeState::Running && next.started_at.is_none() {
        next.started_at = Some(at.to_string());
    }
    if is_terminal_node_state(state) {
        next.completed_at = Some(at.to_string());
    } else if clear_terminal {
        next.completed_at = None;
    }
    if clear_terminal {
        next.error = None;
        next.output = None;
        next.output_bytes = None;
        next.run_stats = None;
    } else if let Some(error) = error {
        next.error = Some(error.clone());
    }
    next
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
                return Some(node.clone());
            }
        }
    }
    None
}

fn node_by_id<'a>(
    record: &'a DagRunRecordV1,
    node_id: &DagNodeId,
) -> Result<&'a DagNode, crate::dag::store::DagStoreError> {
    record
        .nodes
        .iter()
        .find(|node| &node.id == node_id)
        .ok_or_else(|| {
            crate::dag::store::DagStoreError::Message(format!("unknown DAG node \"{node_id}\""))
        })
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
            DagNodeState::Paused => *counts.paused.get_or_insert(0) += 1,
            DagNodeState::Running => counts.running += 1,
            DagNodeState::Completed => counts.completed += 1,
            DagNodeState::Failed => counts.failed += 1,
            DagNodeState::Cancelled => counts.cancelled += 1,
            DagNodeState::Skipped => counts.skipped += 1,
        }
    }
    counts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagNodeControlErrorCode {
    RunNotFound,
    RunNotOwned,
    RunStillActive,
    InvalidArguments,
    NodeNotFound,
    NodeNotRetryable,
    NodeHasNoTask,
    NodeNotContinuable,
    RunPausedByUser,
    RunNotUserPaused,
    GuidanceFrozen,
    GuidanceNotFound,
}

pub const DAG_NODE_CONTROL_ERROR_CODES: [DagNodeControlErrorCode; 12] = [
    DagNodeControlErrorCode::RunNotFound,
    DagNodeControlErrorCode::RunNotOwned,
    DagNodeControlErrorCode::RunStillActive,
    DagNodeControlErrorCode::InvalidArguments,
    DagNodeControlErrorCode::NodeNotFound,
    DagNodeControlErrorCode::NodeNotRetryable,
    DagNodeControlErrorCode::NodeHasNoTask,
    DagNodeControlErrorCode::NodeNotContinuable,
    DagNodeControlErrorCode::RunPausedByUser,
    DagNodeControlErrorCode::RunNotUserPaused,
    DagNodeControlErrorCode::GuidanceFrozen,
    DagNodeControlErrorCode::GuidanceNotFound,
];

impl DagNodeControlErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunNotFound => "run_not_found",
            Self::RunNotOwned => "run_not_owned",
            Self::RunStillActive => "run_still_active",
            Self::InvalidArguments => "invalid_arguments",
            Self::NodeNotFound => "node_not_found",
            Self::NodeNotRetryable => "node_not_retryable",
            Self::NodeHasNoTask => "node_has_no_task",
            Self::NodeNotContinuable => "node_not_continuable",
            Self::RunPausedByUser => "run_paused_by_user",
            Self::RunNotUserPaused => "run_not_user_paused",
            Self::GuidanceFrozen => "guidance_frozen",
            Self::GuidanceNotFound => "guidance_not_found",
        }
    }
}

/// Every refusal the per-node control verbs produce. `code` is the wire vocabulary the dag tool
/// surfaces verbatim; `node_ids` names the exact nodes that caused the refusal.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct DagNodeControlError {
    pub code: DagNodeControlErrorCode,
    pub message: String,
    pub run_id: Option<DagRunId>,
    pub node_ids: Vec<DagNodeId>,
}

impl DagNodeControlError {
    pub fn new(code: DagNodeControlErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            run_id: None,
            node_ids: Vec::new(),
        }
    }

    pub fn with_run_id(mut self, run_id: DagRunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    pub fn with_node_ids(mut self, node_ids: Vec<DagNodeId>) -> Self {
        self.node_ids = node_ids;
        self
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
