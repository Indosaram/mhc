//! `dag-paused-cancel.ts`: ending a user-stopped run's paused children.
//!
//! A cancelled child's session is gone, and its inherited hold with it: closure is PROVEN by the
//! child having ended (terminal record, no live resident handle), never assumed. A child still alive
//! refuses, and the refusal names the child.

use std::sync::Arc;

use senpi_task::{
    dag::{
        manager::DagRunRecordV1,
        paused_controls::{
            DagPauseStore, PausedNodeCancelCause, PausedNodeHoldPort,
            cancel_paused_dag_node_with_store,
        },
        scheduler::{DagNodeControlError, DagNodeControlErrorCode, DagSchedulerOptions},
    },
    manager::TaskManager,
    state::TaskStatus,
};

/// `endedChildClosure`: the inherited hold closes only once the child ended.
pub fn ended_child_closure(manager: Arc<TaskManager>) -> Arc<dyn PausedNodeHoldPort> {
    Arc::new(EndedChildClosure { manager })
}

struct EndedChildClosure {
    manager: Arc<TaskManager>,
}

impl PausedNodeHoldPort for EndedChildClosure {
    fn close_hold(
        &self,
        task_id: &str,
        _cause: PausedNodeCancelCause,
    ) -> Result<(), DagNodeControlError> {
        let Some(record) = self.manager.get(task_id) else {
            return Ok(());
        };
        let ended = record.status.is_terminal();
        if !ended || self.manager.get_resident_handle(task_id).is_some() {
            return Err(DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotContinuable,
                format!("task {task_id} has not ended, so its run hold is still in place"),
            )
            .with_node_ids(vec![task_id.to_string()]));
        }
        Ok(())
    }
}

/// `endPausedRunChildren`: every paused node child of a user-stopped run is ended before the run is
/// journaled cancelled.
pub fn end_paused_run_children(
    manager: &Arc<TaskManager>,
    record: &DagRunRecordV1,
    closure: &dyn PausedNodeHoldPort,
) -> Result<(), DagNodeControlError> {
    for node in &record.nodes {
        let Some(task_id) = &node.task_id else {
            continue;
        };
        let Some(task) = manager.get(task_id) else {
            continue;
        };
        if task.status.is_terminal() || task.status == TaskStatus::Cancelled {
            continue;
        }
        manager
            .cancel_task(
                task_id,
                Some(PausedNodeCancelCause::RunCancelled.as_str()),
                senpi_task::steering::CancelOptions {
                    abort: Default::default(),
                    expected_run_epoch: Some(task.notification.run_epoch),
                },
            )
            .map_err(|error| {
                DagNodeControlError::new(
                    DagNodeControlErrorCode::NodeNotContinuable,
                    format!("child cancellation was refused: {error}"),
                )
                .with_node_ids(vec![node.id.clone()])
            })?;
        closure.close_hold(task_id, PausedNodeCancelCause::RunCancelled)?;
    }
    Ok(())
}

pub type PausedNodeCancel = Arc<dyn Fn(&str, &str) -> Result<(), DagNodeControlError> + Send + Sync>;
/// `createPausedNodeCancel`: the runtime's `cancelNode` capability.
pub fn create_paused_node_cancel(
    options: Arc<dyn Fn(&str) -> DagSchedulerOptions + Send + Sync>,
    holds: Option<Arc<dyn PausedNodeHoldPort>>,
    pauses: Arc<DagPauseStore>,
) -> PausedNodeCancel {
    Arc::new(move |run_id, node_id| {
        let Some(holds) = &holds else {
            return Err(DagNodeControlError::new(
                DagNodeControlErrorCode::NodeNotContinuable,
                "The engine's inherited-hold close port is not configured.",
            )
            .with_run_id(run_id.to_string()));
        };
        let context = options(run_id);
        cancel_paused_dag_node_with_store(
            &context,
            &run_id.to_string(),
            &node_id.to_string(),
            holds.as_ref(),
            pauses.as_ref(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_hold_port_refuses_rather_than_assuming_closure() {
        let cancel = create_paused_node_cancel(
            Arc::new(|_| unreachable!("the hold port is checked before the options are read")),
            None,
            Arc::new(DagPauseStore::new(std::env::temp_dir().join("runs"))),
        );
        let error = cancel("dag_1", "n1").expect_err("no hold port refuses");
        assert_eq!(error.code, DagNodeControlErrorCode::NodeNotContinuable);
    }
}
