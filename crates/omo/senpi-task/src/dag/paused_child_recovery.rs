//! `dag/paused-child-recovery.ts`: restores paused children under inherited custody.

use std::collections::BTreeMap;

use crate::dag::node_control_context::{control_journal, owned_record};
use crate::dag::paused_controls::require_user_paused;
use crate::dag::scheduler::{DagNodeControlError, DagNodeControlErrorCode, DagSchedulerOptions};
use crate::dag::types::{DagNodeError, DagNodeErrorCode, DagNodeState, DagRunEventPayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PausedChildStatus {
    Live,
    Restorable,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunRefKind {
    Dag,
    Task,
    Workpool,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRef {
    pub kind: RunRefKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPauseOwner {
    pub parent_session_id: String,
    pub parent_pause_seq: u64,
    pub run_ref: RunRef,
}

pub trait PausedChildRecoveryPort: Send + Sync {
    fn inspect(&self, task_id: &str) -> PausedChildStatus;
    fn restore_paused(&self, task_id: &str, owner: &RunPauseOwner) -> Result<u64, String>;
}

pub fn recover_paused_dag_children(
    options: &DagSchedulerOptions,
    port: &dyn PausedChildRecoveryPort,
) -> Result<(), DagNodeControlError> {
    let record = owned_record(options, &options.initial_record.run_id)?;
    let pauses = crate::dag::paused_controls::DagPauseStore::from_store(options.store.clone());
    let pause = pauses.read(&record.run_id).map_err(|error| {
        DagNodeControlError::new(
            crate::dag::scheduler::DagNodeControlErrorCode::InvalidArguments,
            error.to_string(),
        )
    })?;
    require_user_paused(pause.pause.as_ref(), &record.run_id)?;
    let pause_seq = record
        .pause
        .as_ref()
        .map(|p| p.pause_seq)
        .expect("paused run has no pause sequence");
    for node in &record.nodes {
        if node.state != DagNodeState::Paused || node.task_id.is_none() {
            continue;
        }
        let task_id = node.task_id.as_ref().expect("validated recovery state is present");
        let fresh = owned_record(options, &record.run_id)?;
        if fresh.pause.as_ref().map(|p| p.pause_seq) != Some(pause_seq)
            || fresh.status != crate::dag::types::DagRunStatus::Paused
        {
            return Ok(());
        }
        let journal = control_journal(options, fresh, BTreeMap::new()).map_err(|e| {
            DagNodeControlError::new(DagNodeControlErrorCode::InvalidArguments, e.to_string())
                .with_run_id(record.run_id.clone())
        })?;
        match port.inspect(task_id) {
            PausedChildStatus::Live => {}
            PausedChildStatus::Lost => {
                let now_ms = options
                    .now
                    .as_ref()
                    .map_or_else(crate::state::system_now_ms, |f| f() as u64);
                let at = crate::shared::iso_from_ms(now_ms as i64);
                journal
                    .append(DagRunEventPayload::NodePauseLost {
                        node_id: node.id.clone(),
                        error: DagNodeError {
                            code: DagNodeErrorCode::LostWhilePaused,
                            message: format!("task {task_id} cannot be restored"),
                            node_id: Some(node.id.clone()),
                            at,
                        },
                    })
                    .map_err(|e| {
                        DagNodeControlError::new(
                            DagNodeControlErrorCode::InvalidArguments,
                            e.to_string(),
                        )
                        .with_run_id(record.run_id.clone())
                    })?;
            }
            PausedChildStatus::Restorable => {
                let owner = RunPauseOwner {
                    parent_session_id: record.parent_session_id.clone(),
                    parent_pause_seq: pause_seq,
                    run_ref: RunRef {
                        kind: RunRefKind::Dag,
                        id: record.run_id.clone(),
                    },
                };
                let child_seq = port.restore_paused(task_id, &owner).map_err(|e| {
                    DagNodeControlError::new(
                        DagNodeControlErrorCode::InvalidArguments,
                        format!("restore failed: {e}"),
                    )
                    .with_run_id(record.run_id.clone())
                })?;
                let current = owned_record(options, &record.run_id)?;
                if current.pause.as_ref().map(|p| p.pause_seq) != Some(pause_seq)
                    || current.status != crate::dag::types::DagRunStatus::Paused
                {
                    return Ok(());
                }
                let current_journal =
                    control_journal(options, current, BTreeMap::new()).map_err(|e| {
                        DagNodeControlError::new(
                            DagNodeControlErrorCode::InvalidArguments,
                            e.to_string(),
                        )
                        .with_run_id(record.run_id.clone())
                    })?;
                current_journal
                    .append(DagRunEventPayload::NodePauseRecovered {
                        node_id: node.id.clone(),
                        child_seq,
                    })
                    .map_err(|e| {
                        DagNodeControlError::new(
                            DagNodeControlErrorCode::InvalidArguments,
                            e.to_string(),
                        )
                        .with_run_id(record.run_id.clone())
                    })?;
            }
        }
    }
    Ok(())
}
