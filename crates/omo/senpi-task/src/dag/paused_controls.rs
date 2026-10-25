//! `dag/paused-controls.ts`: the control verbs a user-stopped DAG run accepts while it stays paused.
//!
//! A run the user stopped must not be restarted by a model: `guide` collects the guidance its nodes
//! receive when the user resumes it, and `cancel_node` ends one node's child without reopening
//! scheduling. Both refuse unless the run is paused BY THE USER (`run_not_user_paused`), which is
//! why the pause reason is persisted beside the run rather than inferred from its status alone.
//!
//! The Rust run record predates the guidance field, so the pause reason, the pause sequence, and the
//! guidance records live in one sidecar file inside the run's directory
//! (`<dag state dir>/runs/<run_id>.pause.json`). Nothing else in the checkpoint shape changes.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::dag::journal::DagJournal;
use crate::dag::manager::DagRunRecordV1;
use crate::dag::node_control_context::{control_journal, owned_record};
use crate::dag::scheduler::{DagNodeControlError, DagNodeControlErrorCode, DagSchedulerOptions};
use crate::dag::store::DagStoreError;
use crate::dag::types::{DagNodeId, DagRunId};
use crate::pause::guidance::{
    Guidance, GuidanceError, GuidanceScope, add_guidance, change_guidance,
};
use crate::steering::{CancelOptions, CancelOutcome};

/// Why a run is paused. Only `UserStop` accepts paused controls; a shutdown pause resumes by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagRunPauseReason {
    UserStop,
    SessionShutdown,
}

/// The pause a run carries (`DagRunRecordV1.pause`): who paused it, and at which sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagRunPause {
    pub reason: DagRunPauseReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_seq: Option<u64>,
}

/// The sidecar state a paused run owns: its pause, and the guidance its children will receive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagPauseState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<DagRunPause>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guidance: Vec<Guidance>,
}

/// The paused-run sidecar, keyed by run id inside the store's own `runs/` directory.
pub struct DagPauseStore {
    runs: std::path::PathBuf,
}

impl DagPauseStore {
    /// `runs_dir` is the DAG store's `runs/` directory: the same folder the run checkpoints live in.
    pub fn new(runs_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            runs: runs_dir.into(),
        }
    }

    /// The sidecar of the store this scheduler runs over.
    pub fn from_store(store: &crate::dag::store::DagFileStore) -> Self {
        Self::new(store.state_dir().join("runs"))
    }

    fn path(&self, run_id: &DagRunId) -> std::path::PathBuf {
        self.runs.join(format!("{run_id}.pause.json"))
    }

    /// A run with no sidecar has no pause and no guidance: both are additive state.
    pub fn read(&self, run_id: &DagRunId) -> Result<DagPauseState, DagStoreError> {
        let path = self.path(run_id);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DagPauseState::default());
            }
            Err(error) => {
                return Err(DagStoreError::Message(format!(
                    "pause state for dag run \"{run_id}\" is unreadable: {error}"
                )));
            }
        };
        serde_json::from_str(&text).map_err(|error| {
            DagStoreError::Message(format!(
                "pause state for dag run \"{run_id}\" is malformed: {error}"
            ))
        })
    }

    pub fn write(&self, run_id: &DagRunId, state: &DagPauseState) -> Result<(), DagStoreError> {
        let path = self.path(run_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                DagStoreError::Message(format!(
                    "pause state directory for dag run \"{run_id}\" is unwritable: {error}"
                ))
            })?;
        }
        let text = serde_json::to_string(state).map_err(|error| {
            DagStoreError::Message(format!(
                "pause state for dag run \"{run_id}\" is not serializable: {error}"
            ))
        })?;
        std::fs::write(&path, text).map_err(|error| {
            DagStoreError::Message(format!(
                "pause state for dag run \"{run_id}\" is unwritable: {error}"
            ))
        })
    }
}

/// The guidance command a paused run accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PausedDagGuideCommand {
    Add {
        text: String,
        node_id: Option<DagNodeId>,
    },
    Edit {
        id: String,
        text: String,
    },
    Remove {
        id: String,
    },
}

fn now_iso(options: &DagSchedulerOptions) -> String {
    let now = options
        .now
        .clone()
        .map(|now| now())
        .unwrap_or_else(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX));
    chrono::DateTime::from_timestamp_millis(now)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `requireUserPaused`: only a run the user stopped accepts paused controls.
pub fn require_user_paused(
    pause: Option<&DagRunPause>,
    run_id: &DagRunId,
) -> Result<(), DagNodeControlError> {
    if pause.map(|pause| &pause.reason) == Some(&DagRunPauseReason::UserStop) {
        return Ok(());
    }
    Err(DagNodeControlError::new(
        DagNodeControlErrorCode::RunNotUserPaused,
        "this action requires a user-paused run",
    )
    .with_run_id(run_id.clone()))
}

fn guidance_failure(error: GuidanceError, run_id: &DagRunId) -> DagNodeControlError {
    match error {
        crate::pause::guidance::GuidanceError::NotFound => DagNodeControlError::new(
            DagNodeControlErrorCode::GuidanceNotFound,
            "Guidance does not exist.",
        )
        .with_run_id(run_id.clone()),
        crate::pause::guidance::GuidanceError::Frozen => DagNodeControlError::new(
            DagNodeControlErrorCode::GuidanceFrozen,
            "Dispatched guidance cannot be changed.",
        )
        .with_run_id(run_id.clone()),
        crate::pause::guidance::GuidanceError::DuplicateId => DagNodeControlError::new(
            DagNodeControlErrorCode::InvalidArguments,
            "Guidance ids must be unique.",
        )
        .with_run_id(run_id.clone()),
    }
}

fn journal_guidance(
    options: &DagSchedulerOptions,
    record: DagRunRecordV1,
) -> Result<Arc<DagJournal<DagRunRecordV1>>, DagNodeControlError> {
    control_journal(options, record, std::collections::BTreeMap::new()).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
    })
}

/// `guidePausedDag`: append one draft guidance record to a user-paused run.
pub fn guide_paused_dag(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
    text: &str,
    node_id: Option<DagNodeId>,
    pauses: &DagPauseStore,
) -> Result<Guidance, DagNodeControlError> {
    let record = owned_record(options, run_id)?;
    let mut state = pauses.read(run_id).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
            .with_run_id(run_id.clone())
    })?;
    require_user_paused(state.pause.as_ref(), run_id)?;
    if let Some(node_id) = &node_id
        && !record.nodes.iter().any(|node| &node.id == node_id)
    {
        return Err(DagNodeControlError::new(
            DagNodeControlErrorCode::NodeNotFound,
            format!("unknown dag node \"{node_id}\""),
        )
        .with_run_id(run_id.clone())
        .with_node_ids(vec![node_id.clone()]));
    }
    let draft = Guidance {
        revision: 0,
        state: crate::pause::guidance::GuidanceState::Draft,
        edited_at: None,
        deliveries: vec![],
        id: uuid_like(run_id, state.guidance.len()),
        scope: match &node_id {
            None => GuidanceScope::Run,
            Some(node_id) => GuidanceScope::Node {
                node_id: node_id.to_string(),
            },
        },
        text: text.to_string(),
        created_at: now_iso(options),
    };
    let created_id = draft.id.clone();
    state.guidance = add_guidance(
        &state.guidance,
        draft.id,
        draft.scope,
        draft.text,
        draft.created_at,
    )
    .map_err(|error| guidance_failure(error, run_id))?;
    pauses.write(run_id, &state).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
            .with_run_id(run_id.clone())
    })?;
    let _ = journal_guidance(options, record)?;
    state
        .guidance
        .into_iter()
        .find(|entry| entry.id == created_id)
        .ok_or_else(|| {
            DagNodeControlError::new(
                DagNodeControlErrorCode::InvalidArguments,
                "guidance was not created",
            )
            .with_run_id(run_id.clone())
        })
}

/// A stable, run-scoped guidance id: the run id plus the record's position in the list.
fn uuid_like(run_id: &DagRunId, position: usize) -> String {
    format!("{run_id}-guidance-{}", position.saturating_add(1))
}

/// `applyPausedDagGuidance`: the tool-facing entry point over add/edit/remove.
pub fn apply_paused_dag_guidance(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
    command: PausedDagGuideCommand,
    pauses: &DagPauseStore,
) -> Result<Guidance, DagNodeControlError> {
    match command {
        PausedDagGuideCommand::Add { text, node_id } => {
            guide_paused_dag(options, run_id, &text, node_id, pauses)
        }
        PausedDagGuideCommand::Edit { id, text } => {
            change_paused_guidance(options, run_id, id, Some(text), false, pauses)
        }
        PausedDagGuideCommand::Remove { id } => {
            change_paused_guidance(options, run_id, id, None, true, pauses)
        }
    }
}

fn change_paused_guidance(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
    id: String,
    text: Option<String>,
    remove: bool,
    pauses: &DagPauseStore,
) -> Result<Guidance, DagNodeControlError> {
    let record = owned_record(options, run_id)?;
    let mut state = pauses.read(run_id).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
            .with_run_id(run_id.clone())
    })?;
    require_user_paused(state.pause.as_ref(), run_id)?;
    state.guidance = change_guidance(&state.guidance, &id, text, remove, now_iso(options))
        .map_err(|error| guidance_failure(error, run_id))?;
    pauses.write(run_id, &state).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
            .with_run_id(run_id.clone())
    })?;
    let _ = journal_guidance(options, record)?;
    state
        .guidance
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| {
            DagNodeControlError::new(
                DagNodeControlErrorCode::InvalidArguments,
                "guidance change did not retain its record",
            )
            .with_run_id(run_id.clone())
        })
}

/// The inherited-hold close port: a cancelled child's hold is closed only once the child ended.
pub trait PausedNodeHoldPort: Send + Sync {
    fn close_hold(
        &self,
        task_id: &str,
        cause: PausedNodeCancelCause,
    ) -> Result<(), DagNodeControlError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PausedNodeCancelCause {
    NodeCancelled,
    RunCancelled,
}

impl PausedNodeCancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NodeCancelled => "node_cancelled",
            Self::RunCancelled => "run_cancelled",
        }
    }
}

/// `cancelPausedDagNode`: end one node's child while the run stays paused, then journal the node
/// cancelled. The run's pause must still be the one the caller read, or the cancel is refused.
pub fn cancel_paused_dag_node_with_store(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
    node_id: &DagNodeId,
    ports: &dyn PausedNodeHoldPort,
    pauses: &DagPauseStore,
) -> Result<(), DagNodeControlError> {
    let record = owned_record(options, run_id)?;
    let state = pauses.read(run_id).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
            .with_run_id(run_id.clone())
    })?;
    require_user_paused(state.pause.as_ref(), run_id)?;
    let node = record
        .nodes
        .iter()
        .find(|entry| &entry.id == node_id)
        .ok_or_else(|| {
            DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotFound,
                format!("unknown dag node \"{node_id}\""),
            )
            .with_run_id(run_id.clone())
            .with_node_ids(vec![node_id.clone()])
        })?;
    if matches!(
        node.state,
        crate::dag::types::DagNodeState::Completed | crate::dag::types::DagNodeState::Cancelled
    ) {
        return Ok(());
    }
    let _ = journal_guidance(options, record.clone())?;
    if let Some(task_id) = &node.task_id {
        let task = options.task_manager.get(task_id);
        let outcome = options
            .task_manager
            .cancel_task(
                task_id,
                Some(PausedNodeCancelCause::NodeCancelled.as_str()),
                CancelOptions {
                    abort: Default::default(),
                    expected_run_epoch: task.as_ref().map(|task| task.notification.run_epoch),
                },
            )
            .map_err(|error| {
                DagNodeControlError::new(
                    DagNodeControlErrorCode::NodeNotContinuable,
                    format!("child cancellation was refused: {error}"),
                )
                .with_run_id(run_id.clone())
                .with_node_ids(vec![node_id.clone()])
            })?;
        if !matches!(
            outcome,
            CancelOutcome::Cancelled { .. }
                | CancelOutcome::Noop { .. }
                | CancelOutcome::NotFound { .. }
                | CancelOutcome::Stale { .. }
        ) {
            return Err(DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotContinuable,
                "child cancellation was refused",
            )
            .with_run_id(run_id.clone())
            .with_node_ids(vec![node_id.clone()]));
        }
        ports.close_hold(task_id, PausedNodeCancelCause::NodeCancelled)?;
    }
    let fresh = owned_record(options, run_id)?;
    let fresh_state = pauses.read(run_id).map_err(|error| {
        DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
            .with_run_id(run_id.clone())
    })?;
    require_user_paused(fresh_state.pause.as_ref(), run_id)?;
    let current = fresh.nodes.iter().find(|entry| &entry.id == node_id);
    if fresh_state.pause != state.pause || current.is_none() {
        return Err(DagNodeControlError::new(
            DagNodeControlErrorCode::RunPausedByUser,
            "the pause changed during cancellation",
        )
        .with_run_id(run_id.clone()));
    }
    let _ = journal_guidance(options, fresh)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_id() -> DagRunId {
        "dag_test".to_string()
    }

    #[test]
    fn a_run_that_is_not_user_paused_refuses_paused_controls() {
        assert_eq!(
            require_user_paused(None, &run_id()).map_err(|error| error.code),
            Err(DagNodeControlErrorCode::RunNotUserPaused)
        );
        assert_eq!(
            require_user_paused(
                Some(&DagRunPause {
                    reason: DagRunPauseReason::SessionShutdown,
                    pause_seq: Some(2)
                }),
                &run_id()
            )
            .map_err(|error| error.code),
            Err(DagNodeControlErrorCode::RunNotUserPaused)
        );
        require_user_paused(
            Some(&DagRunPause {
                reason: DagRunPauseReason::UserStop,
                pause_seq: Some(3),
            }),
            &run_id(),
        )
        .expect("a user stop accepts paused controls");
    }

    #[test]
    fn a_pause_state_round_trips_through_the_run_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pauses = DagPauseStore::new(dir.path().join("runs"));
        assert_eq!(
            pauses.read(&run_id()).expect("absent"),
            DagPauseState::default()
        );
        let state = DagPauseState {
            pause: Some(DagRunPause {
                reason: DagRunPauseReason::UserStop,
                pause_seq: Some(4),
            }),
            guidance: Vec::new(),
        };
        pauses.write(&run_id(), &state).expect("write");
        assert_eq!(pauses.read(&run_id()).expect("read"), state);
    }
}

pub use crate::dag::manager::paused_amendment_unsafe;

/// Checkpoint-backed callers use the same sidecar cancellation implementation.
pub fn cancel_paused_dag_node(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
    node_id: &DagNodeId,
    close: impl Fn(&str, &str) + Send + Sync,
) -> Result<(), DagNodeControlError> {
    struct Close<F>(F);
    impl<F: Fn(&str, &str) + Send + Sync> PausedNodeHoldPort for Close<F> {
        fn close_hold(
            &self,
            task: &str,
            cause: PausedNodeCancelCause,
        ) -> Result<(), DagNodeControlError> {
            (self.0)(task, cause.as_str());
            Ok(())
        }
    }
    let pauses = DagPauseStore::new(options.store.state_dir());
    if let Some(record) = options
        .store
        .read_checkpoint::<DagRunRecordV1>(run_id)
        .map_err(|error| {
            DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, error.to_string())
        })?
    {
        if let Some(pause) = record.pause {
            let mut state = pauses.read(run_id).map_err(|error| {
                DagNodeControlError::new(
                    DagNodeControlErrorCode::InvalidArguments,
                    error.to_string(),
                )
            })?;
            state.pause = Some(DagRunPause {
                reason: DagRunPauseReason::UserStop,
                pause_seq: Some(pause.pause_seq),
            });
            pauses.write(run_id, &state).map_err(|error| {
                DagNodeControlError::new(
                    DagNodeControlErrorCode::InvalidArguments,
                    error.to_string(),
                )
            })?;
        }
    }
    cancel_paused_dag_node_with_store(options, run_id, node_id, &Close(close), &pauses)
}
