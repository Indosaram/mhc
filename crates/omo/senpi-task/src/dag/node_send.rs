//! Send messages to a DAG node's child and fold revived-task completion.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::dag::manager::DagRunRecordV1;
use crate::dag::node_control_context::{control_journal, current_record, owned_record};
use crate::dag::results::{
    DagNodeResultPersistInput, DagNodeResultPersistOutcome, persist_dag_node_result,
};
use crate::dag::scheduler::{DagNodeControlError, DagNodeControlErrorCode, DagSchedulerOptions};
use crate::dag::types::{
    DagNode, DagNodeError, DagNodeErrorCode, DagNodeId, DagNodeState, DagRunEventPayload, DagRunId,
};
use crate::state::TaskStatus;
use crate::steering::{SendInput, SendOutcome};

pub struct DagNodeSendResult {
    pub node_id: DagNodeId,
    pub task_id: Option<String>,
    pub guidance_id: Option<String>,
    pub delivery: DagNodeDeliveryResult,
    pub queue_position: Option<usize>,
    pub settled: Option<std::thread::JoinHandle<Result<(), String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagNodeDeliveryResult {
    Guidance,
    Steer,
    Revive,
    Queued,
}

pub fn send_to_dag_node(
    options: &DagSchedulerOptions,
    run_id: &DagRunId,
    node_id: &DagNodeId,
    message: &str,
) -> Result<DagNodeSendResult, DagNodeControlError> {
    let record = owned_record(options, run_id)?;
    let node = record
        .nodes
        .iter()
        .find(|node| &node.id == node_id)
        .cloned()
        .ok_or_else(|| {
            control_error(
                DagNodeControlErrorCode::NodeNotFound,
                format!("unknown dag node \"{node_id}\""),
                run_id,
                node_id,
            )
        })?;
    if matches!(
        record.status,
        crate::dag::types::DagRunStatus::Completed | crate::dag::types::DagRunStatus::Cancelled
    ) {
        let message = if record.status == crate::dag::types::DagRunStatus::Completed {
            format!(
                "dag run \"{run_id}\" already completed; node \"{node_id}\" cannot be steered or revived. Amend the definition or start a new run."
            )
        } else {
            format!(
                "dag run \"{run_id}\" was cancelled; node \"{node_id}\" cannot be steered or revived. Retry the node to run it again."
            )
        };
        return Err(control_error(
            DagNodeControlErrorCode::NodeNotContinuable,
            message,
            run_id,
            node_id,
        ));
    }
    if record.status == crate::dag::types::DagRunStatus::Paused
        && record.pause.as_ref().is_some_and(|pause| pause.reason == "user_stop")
    {
        let pauses = crate::dag::paused_controls::DagPauseStore::from_store(options.store.clone());
        let guidance = crate::dag::paused_controls::guide_paused_dag(options, run_id, message, Some(node_id.clone()), &pauses)?;
        return Ok(DagNodeSendResult { node_id: node_id.clone(), task_id: None, guidance_id: Some(guidance.id), delivery: DagNodeDeliveryResult::Guidance, queue_position: None, settled: None });
    }
    let task_id = node.task_id.clone().ok_or_else(|| {
        control_error(
            DagNodeControlErrorCode::NodeHasNoTask,
            format!("dag node \"{node_id}\" never started a child; retry the run to dispatch it"),
            run_id,
            node_id,
        )
    })?;
    if options.task_manager.get(&task_id).is_none() {
        return Err(control_error(
            DagNodeControlErrorCode::NodeNotFound,
            format!("dag node \"{node_id}\" points at task {task_id}, which no longer exists"),
            run_id,
            node_id,
        ));
    }
    let outcome = options
        .task_manager
        .send_to_task(&SendInput {
            id_or_name: task_id.clone(),
            message: message.to_string(),
            caller_session_id: Some(record.parent_session_id.clone()),
            all_scope: true,
            ..SendInput::default()
        })
        .map_err(|error| {
            control_error(
                DagNodeControlErrorCode::NodeNotContinuable,
                error.to_string(),
                run_id,
                node_id,
            )
        })?;
    fold_send_outcome(options, &record, &node, &task_id, outcome)
}

pub fn fold_send_outcome(
    options: &DagSchedulerOptions,
    record: &DagRunRecordV1,
    node: &DagNode,
    task_id: &str,
    outcome: SendOutcome,
) -> Result<DagNodeSendResult, DagNodeControlError> {
    let node_id = node.id.clone();
    let run_id = record.run_id.clone();
    match outcome {
        SendOutcome::Held { .. } => Ok(DagNodeSendResult {
            node_id,
            task_id: Some(task_id.into()),
            guidance_id: None,
            delivery: DagNodeDeliveryResult::Queued,
            queue_position: None,
            settled: None,
        }),
        SendOutcome::Queued { queue_position, .. } => Ok(DagNodeSendResult {
            node_id,
            task_id: Some(task_id.into()),
            guidance_id: None,
            delivery: DagNodeDeliveryResult::Queued,
            queue_position: Some(queue_position),
            settled: None,
        }),
        SendOutcome::Steered { .. } => {
            control_journal(options, record.clone(), BTreeMap::new())
                .and_then(|journal| {
                    journal
                        .append(DagRunEventPayload::NodeSteered {
                            node_id: node_id.clone(),
                            task_id: task_id.into(),
                            delivery: crate::dag::types::DagNodeDelivery::Steer,
                        })
                        .map(|_| ())
                })
                .map_err(|error| {
                    control_error(
                        DagNodeControlErrorCode::InvalidArguments,
                        error.to_string(),
                        &run_id,
                        &node_id,
                    )
                })?;
            Ok(DagNodeSendResult {
                node_id,
                task_id: Some(task_id.into()),
                guidance_id: None,
                delivery: DagNodeDeliveryResult::Steer,
                queue_position: None,
                settled: None,
            })
        }
        SendOutcome::Revived { .. } => {
            let journal =
                control_journal(options, record.clone(), BTreeMap::new()).map_err(|error| {
                    control_error(
                        DagNodeControlErrorCode::InvalidArguments,
                        error.to_string(),
                        &run_id,
                        &node_id,
                    )
                })?;
            journal
                .append(DagRunEventPayload::NodeSteered {
                    node_id: node_id.clone(),
                    task_id: task_id.into(),
                    delivery: crate::dag::types::DagNodeDelivery::Revive,
                })
                .and_then(|_| {
                    journal.append(DagRunEventPayload::NodeTransitioned {
                        node_id: node_id.clone(),
                        from: node.state,
                        to: DagNodeState::Running,
                        reason: crate::dag::types::DagNodeTransitionReason::Revived,
                    })
                })
                .map_err(|error| {
                    control_error(
                        DagNodeControlErrorCode::InvalidArguments,
                        error.to_string(),
                        &run_id,
                        &node_id,
                    )
                })?;
            let watch_options = clone_scheduler_options(options);
            let watch_node = node_id.clone();
            let watch_task = task_id.to_string();
            let settled = std::thread::Builder::new()
                .name("dag-revived-node-watch".into())
                .spawn(move || watch_revived_task(watch_options, watch_node, watch_task))
                .map_err(|error| {
                    control_error(
                        DagNodeControlErrorCode::InvalidArguments,
                        error.to_string(),
                        &run_id,
                        &node_id,
                    )
                })?;
            Ok(DagNodeSendResult {
                node_id,
                task_id: Some(task_id.into()),
                guidance_id: None,
                delivery: DagNodeDeliveryResult::Revive,
                queue_position: None,
                settled: Some(settled),
            })
        }
        SendOutcome::NotFound { .. } => Err(control_error(
            DagNodeControlErrorCode::NodeNotFound,
            format!("dag node \"{node_id}\" points at task {task_id}, which no longer exists"),
            &run_id,
            &node_id,
        )),
        SendOutcome::OneShotAgent { agent, message, .. } => Err(control_error(
            DagNodeControlErrorCode::NodeNotContinuable,
            format!(
                "dag node \"{node_id}\" runs one-shot agent {agent}: {message} Retry the node instead."
            ),
            &run_id,
            &node_id,
        )),
        SendOutcome::ScopeDenied { reason, .. } => Err(control_error(
            DagNodeControlErrorCode::NodeNotContinuable,
            format!("dag node \"{node_id}\" cannot be messaged: {reason} Retry the node instead."),
            &run_id,
            &node_id,
        )),
        SendOutcome::NotContinuable {
            reason, suggestion, ..
        } => Err(control_error(
            DagNodeControlErrorCode::NodeNotContinuable,
            format!(
                "dag node \"{node_id}\" cannot be continued: {reason} {suggestion} Retry the node to run it again."
            ),
            &run_id,
            &node_id,
        )),
        SendOutcome::Stale { reason, .. } => Err(control_error(
            DagNodeControlErrorCode::NodeNotContinuable,
            format!(
                "dag node \"{node_id}\" cannot be continued: {reason} Retry the node to run it again."
            ),
            &run_id,
            &node_id,
        )),
    }
}

fn clone_scheduler_options(options: &DagSchedulerOptions) -> DagSchedulerOptions {
    DagSchedulerOptions {
        store: Arc::clone(&options.store),
        task_manager: Arc::clone(&options.task_manager),
        initial_record: options.initial_record.clone(),
        execution_mode: options.execution_mode.as_ref().map(|mode| {
            crate::dag::scheduler::DagExecutionModeOptions {
                agents: Arc::clone(&mode.agents),
                config: mode.config,
                auto_mode: mode.auto_mode,
                config_is_auto: mode.config_is_auto,
            }
        }),
        ancestry: options.ancestry,
        resume_credential: options.resume_credential.clone(),
        node_spawn_policy: options.node_spawn_policy.clone(),
        subscriber_ring: options.subscriber_ring,
        pre_attached_tasks: None,
        schedule_retry: None,
        now: options.now.clone(),
    }
}

pub fn watch_revived_task(
    options: DagSchedulerOptions,
    node_id: DagNodeId,
    task_id: String,
) -> Result<(), String> {
    let run_id = options.initial_record.run_id.clone();
    let now = options.now.clone().unwrap_or_else(|| {
        Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX))
    });
    let task = match options.task_manager.wait_for(&task_id, None, None) {
        Ok(task) => task,
        Err(error) => {
            fail_revived_node(
                &options,
                &node_id,
                DagNodeErrorCode::TaskError,
                error.to_string(),
                &now,
            )?;
            return Ok(());
        }
    };
    if matches!(
        task.status,
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused
    ) {
        fail_revived_node(
            &options,
            &node_id,
            DagNodeErrorCode::TaskError,
            format!("revived task {task_id} settled nonterminal"),
            &now,
        )?;
        return Ok(());
    }
    if task.status != TaskStatus::Completed {
        let (code, message) = revived_failure(task.status, task.error_message.as_deref());
        fail_revived_node(&options, &node_id, code, message, &now)?;
        return Ok(());
    }
    match persist_dag_node_result(DagNodeResultPersistInput {
        store: &options.store,
        run_id: run_id.clone(),
        node_id: node_id.clone(),
        record: &task,
        now: Some(now.as_ref()),
    }) {
        DagNodeResultPersistOutcome::Failed { diagnostic } => {
            return Err(diagnostic.message().to_string());
        }
        DagNodeResultPersistOutcome::Persisted { .. } => {}
    }
    let current = current_record(&options, &run_id).map_err(|error| error.to_string())?;
    let journal =
        control_journal(&options, current, BTreeMap::new()).map_err(|error| error.to_string())?;
    journal
        .append(DagRunEventPayload::NodeTransitioned {
            node_id,
            from: DagNodeState::Running,
            to: DagNodeState::Completed,
            reason: crate::dag::types::DagNodeTransitionReason::Succeeded,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn fail_revived_node(
    options: &DagSchedulerOptions,
    node_id: &DagNodeId,
    code: DagNodeErrorCode,
    message: String,
    now: &Arc<dyn Fn() -> i64 + Send + Sync>,
) -> Result<(), String> {
    let run_id = options.initial_record.run_id.clone();
    let error = DagNodeError {
        code,
        message,
        node_id: Some(node_id.clone()),
        at: crate::shared::iso_from_ms(now()),
    };
    let mut errors = BTreeMap::new();
    errors.insert(node_id.clone(), error);
    let current = current_record(options, &run_id).map_err(|error| error.to_string())?;
    control_journal(options, current, errors)
        .map_err(|error| error.to_string())?
        .append(DagRunEventPayload::NodeTransitioned {
            node_id: node_id.clone(),
            from: DagNodeState::Running,
            to: DagNodeState::Failed,
            reason: crate::dag::types::DagNodeTransitionReason::Failed,
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn revived_failure(status: TaskStatus, message: Option<&str>) -> (DagNodeErrorCode, String) {
    match status {
        TaskStatus::Error => (
            DagNodeErrorCode::TaskError,
            message.unwrap_or("task failed").into(),
        ),
        TaskStatus::Interrupted => (
            DagNodeErrorCode::TaskInterrupted,
            message.unwrap_or("task was interrupted").into(),
        ),
        TaskStatus::Lost => (
            DagNodeErrorCode::TaskLost,
            message.unwrap_or("task was lost").into(),
        ),
        TaskStatus::Cancelled => (
            DagNodeErrorCode::TaskCancelled,
            message.unwrap_or("task was cancelled").into(),
        ),
        TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused | TaskStatus::Completed => (
            DagNodeErrorCode::TaskError,
            "task in a non-terminal-failure status".into(),
        ),
    }
}

fn control_error(
    code: DagNodeControlErrorCode,
    message: String,
    run_id: &DagRunId,
    node_id: &DagNodeId,
) -> DagNodeControlError {
    DagNodeControlError::new(code, message)
        .with_run_id(run_id.clone())
        .with_node_ids(vec![node_id.clone()])
}

#[cfg(test)]
#[path = "node_send_tests.rs"]
mod tests;
