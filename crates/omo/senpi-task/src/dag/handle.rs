//! `dag/handle.ts`: the wait/attach surface - ownership checks, terminal projection, and waiter
//! bookkeeping on one contract so no caller can observe a half-applied terminal state.
// allow: SIZE_OK - wait/attach ownership, terminal projection, and waiter bookkeeping share one
// contract so no caller can observe a half-applied terminal state.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::dag::journal::{DagJournalUnsubscribe, subscribe_dag_journal};
use crate::dag::manager::DagRunRecordV1;
use crate::dag::store::{DagEventReadOptions, DagFileStore};
use crate::dag::types::{
    DagNode, DagNodeCounts, DagNodeError, DagNodeErrorCode, DagNodeId, DagNodeState, DagRunEventPayload,
    DagRunEventType, DagRunId, DagRunSnapshot, DagRunStatus,
};
use crate::state::TaskRunStats;

const CANCEL_REASON_PAGE_LIMIT: usize = 64;
const DEFAULT_CANCEL_REASON: &str = "cancelled";

fn is_terminal_run_status(status: DagRunStatus) -> bool {
    matches!(
        status,
        DagRunStatus::Completed | DagRunStatus::Failed | DagRunStatus::Cancelled
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagWaitErrorCode {
    RunNotFound,
    RunNotOwned,
    InvalidArguments,
}

impl DagWaitErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunNotFound => "run_not_found",
            Self::RunNotOwned => "run_not_owned",
            Self::InvalidArguments => "invalid_arguments",
        }
    }
}

/// The ONLY rejection this surface produces, and only from a pre-dispatch check. A task outcome -
/// failed, cancelled, or otherwise - always resolves; callers read the outcome off [`DagRunResult`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DagWaitError {
    pub code: DagWaitErrorCode,
    pub message: String,
    pub run_id: Option<DagRunId>,
}

impl DagWaitError {
    fn new(code: DagWaitErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            run_id: None,
        }
    }

    fn with_run_id(mut self, run_id: DagRunId) -> Self {
        self.run_id = Some(run_id);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DagTerminalNodeResult {
    Completed {
        task_id: String,
        output: String,
        run_stats: Option<TaskRunStats>,
    },
    Failed {
        task_id: Option<String>,
        error: DagNodeError,
    },
    Cancelled {
        task_id: Option<String>,
        reason: String,
    },
    Skipped {
        dependency_ids: Vec<DagNodeId>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DagRunResult {
    pub run_id: DagRunId,
    pub status: DagRunStatus,
    pub snapshot: DagRunSnapshot,
    pub nodes: HashMap<String, DagTerminalNodeResult>,
}

pub struct DagRunHandle {
    pub run_id: DagRunId,
    surface: Arc<Inner>,
    parent_session_id: String,
}

impl DagRunHandle {
    pub fn snapshot(&self) -> Result<DagRunSnapshot, DagWaitError> {
        self.surface
            .owned_record(&self.run_id, &self.parent_session_id)
            .map(project_snapshot)
    }

    pub fn done(&self) -> Result<DagRunResult, DagWaitError> {
        Inner::wait(&self.surface, &self.run_id, &self.parent_session_id)
    }

    pub fn cancel(&self, reason: Option<&str>) -> Result<(), DagWaitError> {
        self.surface
            .owned_record(&self.run_id, &self.parent_session_id)?;
        if let Some(cancel) = &self.surface.cancel {
            cancel(&self.run_id, reason);
        }
        Ok(())
    }
}

pub type DagSubscribe =
    Arc<dyn Fn(&DagRunId, Arc<dyn Fn() + Send + Sync>) -> DagJournalUnsubscribe + Send + Sync>;
pub type DagCancel = Arc<dyn Fn(&DagRunId, Option<&str>) + Send + Sync>;
pub type DagReadOutput = Arc<dyn Fn(&DagRunId, &DagNodeId) -> Option<String> + Send + Sync>;

pub struct DagWaitSurfaceOptions {
    pub store: Arc<DagFileStore>,
    /// Journal subscription seam: the scheduler owns the live journal, this surface only listens.
    pub subscribe: DagSubscribe,
    pub cancel: Option<DagCancel>,
    /// Node output source; defaults to the durable per-node result artifact written by the
    /// scheduler.
    pub read_output: Option<DagReadOutput>,
}

struct RunWaiters {
    resolved: Option<DagRunResult>,
    unsubscribers: Vec<DagJournalUnsubscribe>,
    waiter_count: usize,
}

/// One wait slot per run: the waiters plus the condvar that wakes them.
type WaiterSlot = Arc<(Mutex<RunWaiters>, Condvar)>;

struct Inner {
    store: Arc<DagFileStore>,
    subscribe: DagSubscribe,
    cancel: Option<DagCancel>,
    read_output: DagReadOutput,
    waiters: Mutex<HashMap<DagRunId, WaiterSlot>>,
}

impl Inner {
    fn owned_record(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunRecordV1, DagWaitError> {
        if run_id.is_empty() {
            return Err(DagWaitError::new(
                DagWaitErrorCode::InvalidArguments,
                "runId must be a non-empty string",
            ));
        }
        if parent_session_id.is_empty() {
            return Err(DagWaitError::new(
                DagWaitErrorCode::InvalidArguments,
                "parentSessionId must be a non-empty string",
            )
            .with_run_id(run_id.clone()));
        }
        let record = self
            .store
            .read_checkpoint::<DagRunRecordV1>(run_id)
            .ok()
            .flatten()
            .ok_or_else(|| {
                DagWaitError::new(
                    DagWaitErrorCode::RunNotFound,
                    format!("unknown dag run \"{run_id}\""),
                )
                .with_run_id(run_id.clone())
            })?;
        // A run whose owning session is gone stays owned by it: a new session gets a rejection
        // here rather than a subscription that would never settle.
        if record.parent_session_id != parent_session_id {
            return Err(DagWaitError::new(
                DagWaitErrorCode::RunNotOwned,
                format!("dag run \"{run_id}\" belongs to another session"),
            )
            .with_run_id(run_id.clone()));
        }
        Ok(record)
    }

    fn slot(&self, run_id: &DagRunId) -> WaiterSlot {
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(waiters.entry(run_id.clone()).or_insert_with(|| {
            Arc::new((
                Mutex::new(RunWaiters {
                    resolved: None,
                    unsubscribers: Vec::new(),
                    waiter_count: 0,
                }),
                Condvar::new(),
            ))
        }))
    }

    fn settle(&self, run_id: &DagRunId, result: DagRunResult) {
        let slot = {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            waiters.remove(run_id)
        };
        let Some(slot) = slot else { return };
        let (state, condvar) = &*slot;
        let unsubscribers = {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            state.resolved = Some(result);
            std::mem::take(&mut state.unsubscribers)
        };
        for unsubscribe in unsubscribers {
            unsubscribe();
        }
        condvar.notify_all();
    }

    fn wait(self_arc: &Arc<Self>, run_id: &DagRunId, parent_session_id: &str) -> Result<DagRunResult, DagWaitError> {
        let record = self_arc.owned_record(run_id, parent_session_id)?;
        if is_terminal_run_status(record.status) {
            return Ok(self_arc.project_result(&record));
        }
        let slot = self_arc.slot(run_id);
        {
            let mut state = slot.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.waiter_count += 1;
        }
        Inner::register(self_arc, run_id, &slot);
        let (state, condvar) = &*slot;
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(result) = &state.resolved {
                return Ok(result.clone());
            }
            state = condvar
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn register(self_arc: &Arc<Self>, run_id: &DagRunId, slot: &WaiterSlot) {
        let weak: std::sync::Weak<Inner> = Arc::downgrade(self_arc);
        let on_journal_event = {
            let store = Arc::clone(&self_arc.store);
            let run_id = run_id.clone();
            let weak = weak.clone();
            move || {
                let Ok(Some(current)) = store.read_checkpoint::<DagRunRecordV1>(&run_id) else {
                    return;
                };
                if !is_terminal_run_status(current.status) {
                    return;
                }
                let Some(inner) = weak.upgrade() else { return };
                let result = inner.project_result(&current);
                inner.settle(&run_id, result);
            }
        };
        let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(on_journal_event.clone());
        let commit_unsubscribe = subscribe_dag_journal(
            &self_arc.store,
            run_id,
            Arc::new({
                let on_journal_event = on_journal_event.clone();
                move |_event| on_journal_event()
            }),
        );
        let live_unsubscribe = (self_arc.subscribe)(run_id, Arc::clone(&listener));
        {
            let mut state = slot.0.lock().unwrap_or_else(PoisonError::into_inner);
            if state.resolved.is_none() {
                state.unsubscribers.push(commit_unsubscribe);
                state.unsubscribers.push(live_unsubscribe);
            } else {
                commit_unsubscribe();
                live_unsubscribe();
                return;
            }
        }
        if let Ok(Some(now)) = self_arc.store.read_checkpoint::<DagRunRecordV1>(run_id)
            && is_terminal_run_status(now.status)
        {
            let result = self_arc.project_result(&now);
            self_arc.settle(run_id, result);
        }
    }

    fn waiter_count(&self, run_id: &DagRunId) -> usize {
        let waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        waiters
            .get(run_id)
            .map(|slot| {
                slot.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .waiter_count
            })
            .unwrap_or(0)
    }

    fn project_result(&self, record: &DagRunRecordV1) -> DagRunResult {
        let cancel_reason = if record.status == DagRunStatus::Cancelled {
            self.read_cancel_reason(&record.run_id)
        } else {
            DEFAULT_CANCEL_REASON.to_string()
        };
        let mut nodes = HashMap::new();
        for node in &record.nodes {
            if let Some(terminal) = project_node(node, &record.run_id, &cancel_reason, &self.read_output) {
                nodes.insert(node.id.clone(), terminal);
            }
        }
        DagRunResult {
            run_id: record.run_id.clone(),
            status: record.status,
            snapshot: project_snapshot(record.clone()),
            nodes,
        }
    }

    fn read_cancel_reason(&self, run_id: &DagRunId) -> String {
        let mut reason = DEFAULT_CANCEL_REASON.to_string();
        let mut since_seq = 0_u64;
        loop {
            let Ok(page) = self.store.read_events(
                run_id,
                since_seq,
                &DagEventReadOptions {
                    limit: CANCEL_REASON_PAGE_LIMIT,
                    types: Some(vec![DagRunEventType::RunCancelled]),
                    ..Default::default()
                },
            ) else {
                return reason;
            };
            for event in &page.events {
                if let DagRunEventPayload::RunCancelled {
                    reason: Some(event_reason),
                    ..
                } = &event.payload
                {
                    reason = event_reason.clone();
                }
            }
            if !page.has_more {
                return reason;
            }
            since_seq = page.next_since_seq;
        }
    }
}

/// A wait/attach surface over one store: `wait` blocks until the run reaches a terminal status;
/// `attach` returns a non-blocking handle with the same `done`/`cancel` surface.
#[derive(Clone)]
pub struct DagWaitSurface {
    inner: Arc<Inner>,
}

pub fn create_dag_wait_surface(options: DagWaitSurfaceOptions) -> DagWaitSurface {
    let store = options.store;
    let read_output = options.read_output.unwrap_or_else(|| {
        let store = Arc::clone(&store);
        Arc::new(move |run_id: &DagRunId, node_id: &DagNodeId| {
            store.read_result(run_id, node_id).ok().flatten()
        })
    });
    DagWaitSurface {
        inner: Arc::new(Inner {
            store,
            subscribe: options.subscribe,
            cancel: options.cancel,
            read_output,
            waiters: Mutex::new(HashMap::new()),
        }),
    }
}

impl DagWaitSurface {
    pub fn wait(&self, run_id: &DagRunId, parent_session_id: &str) -> Result<DagRunResult, DagWaitError> {
        Inner::wait(&self.inner, run_id, parent_session_id)
    }

    pub fn attach(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunHandle, DagWaitError> {
        self.inner.owned_record(run_id, parent_session_id)?;
        Ok(DagRunHandle {
            run_id: run_id.clone(),
            surface: Arc::clone(&self.inner),
            parent_session_id: parent_session_id.to_string(),
        })
    }

    /// Test-only observability proving a settled run retains no waiter bookkeeping.
    pub fn waiter_count(&self, run_id: &DagRunId) -> usize {
        self.inner.waiter_count(run_id)
    }
}

// Nonterminal nodes are omitted rather than invented: a terminal run has no nonterminal node, so
// an entry here would be a scheduler bug the caller must be able to see in the snapshot.
fn project_node(
    node: &DagNode,
    run_id: &DagRunId,
    cancel_reason: &str,
    read_output: &DagReadOutput,
) -> Option<DagTerminalNodeResult> {
    match node.state {
        DagNodeState::Completed => Some(DagTerminalNodeResult::Completed {
            // A completed node always carries the task it ran as; the empty fallback only exists
            // so a corrupt journal degrades to an inspectable result instead of a type lie.
            task_id: node.task_id.clone().unwrap_or_default(),
            output: read_output(run_id, &node.id).unwrap_or_default(),
            run_stats: node.run_stats.clone(),
        }),
        DagNodeState::Failed => Some(DagTerminalNodeResult::Failed {
            task_id: node.task_id.clone(),
            error: node.error.clone().unwrap_or_else(|| DagNodeError {
                code: DagNodeErrorCode::TaskError,
                message: format!("node \"{}\" failed without a recorded error", node.id),
                node_id: Some(node.id.clone()),
                at: node
                    .completed_at
                    .clone()
                    .unwrap_or_else(|| node.created_at.clone()),
            }),
        }),
        DagNodeState::Cancelled => Some(DagTerminalNodeResult::Cancelled {
            task_id: node.task_id.clone(),
            reason: cancel_reason.to_string(),
        }),
        DagNodeState::Skipped => Some(DagTerminalNodeResult::Skipped {
            dependency_ids: node.depends_on.clone(),
        }),
        DagNodeState::Pending
        | DagNodeState::Blocked
        | DagNodeState::Scheduled
        | DagNodeState::Running => None,
    }
}

fn project_snapshot(record: DagRunRecordV1) -> DagRunSnapshot {
    DagRunSnapshot {
        schema_version: crate::dag::types::SchemaVersion1,
        run_id: record.run_id,
        run_key: record.run_key,
        name: record.name,
        parent_session_id: record.parent_session_id,
        root_session_id: record.root_session_id,
        status: record.status,
        generation: record.generation,
        created_at: record.created_at,
        started_at: record.started_at,
        completed_at: record.completed_at,
        definition_fingerprint: record.definition_fingerprint,
        last_seq: record.checkpoint_seq,
        counts: count_nodes(&record.nodes),
        nodes: record.nodes,
        edges: record.edges,
        waves: record.waves,
        critical_path: record.critical_path,
        bottlenecks: record.bottlenecks,
        diagnostics: record.diagnostics,
    }
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
#[path = "handle_tests.rs"]
mod tests;
