//! Retry failed DAG nodes and re-enter settled runs with a fresh scheduler.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::journal::DagJournal;
use crate::dag::manager::{DagAmendParams, DagManagerOptions, DagRunRecordV1, create_dag_manager};
use crate::dag::node_control_context::{control_journal, owned_record, read_record};
use crate::dag::scheduler::{
    DagNodeControlError, DagNodeControlErrorCode, DagSchedulerOptions, create_dag_scheduler,
};
use crate::dag::store::DagStoreError;
use crate::dag::types::{
    DagNode, DagNodeId, DagNodeRetryAutomatic, DagNodeState, DagRunEventPayload, DagRunId,
    DagRunStatus,
};

pub struct DagRunReentry {
    pub record: DagRunRecordV1,
    pub scheduler: Arc<crate::dag::scheduler::DagSchedulerContext>,
}

const RETRYABLE: [DagNodeState; 3] = [
    DagNodeState::Failed,
    DagNodeState::Cancelled,
    DagNodeState::Skipped,
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DagRetryOptions {
    pub prompt: Option<String>,
}

pub fn reenter_dag_run(options: DagSchedulerOptions) -> Result<DagRunReentry, DagNodeControlError> {
    let run_id = options.initial_record.run_id.clone();
    let current = read_record(&options.store, &run_id)
        .map_err(|error| {
            control_error(
                DagNodeControlErrorCode::InvalidArguments,
                error.to_string(),
                &run_id,
                vec![],
            )
        })?
        .unwrap_or_else(|| options.initial_record.clone());
    if current.pause.as_ref().is_some_and(|p| p.reason == "user_stop") {
        return Err(control_error(
            DagNodeControlErrorCode::RunPausedByUser,
            "only the user's Resume may restart this run".to_string(),
            &run_id,
            vec![],
        ));
    }
    let mut record = current.clone();
    if matches!(
        current.status,
        DagRunStatus::Completed | DagRunStatus::Failed | DagRunStatus::Cancelled
    ) {
        let journal = control_journal(&options, current.clone(), Default::default()).map_err(|error| {
            control_error(
                DagNodeControlErrorCode::InvalidArguments,
                error.to_string(),
                &run_id,
                vec![],
            )
        })?;
        journal.append(DagRunEventPayload::RunResumed {
            generation: current.generation + 1,
        }).map_err(|error| {
            control_error(
                DagNodeControlErrorCode::InvalidArguments,
                error.to_string(),
                &run_id,
                vec![],
            )
        })?;
        record = journal.snapshot();
    }
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        initial_record: record.clone(),
        pre_attached_tasks: None,
        schedule_retry: None,
        ..options
    }).map_err(|error| {
        control_error(
            DagNodeControlErrorCode::InvalidArguments,
            error.to_string(),
            &run_id,
            vec![],
        )
    })?;
    let scheduler_for_run = Arc::clone(&scheduler);
    let _ = std::thread::Builder::new()
        .name("dag-node-reentry".into())
        .spawn(move || {
            let _ = scheduler_for_run.run();
        });
    Ok(DagRunReentry { record, scheduler })
}

pub fn retry_dag_nodes(
    options: DagSchedulerOptions,
    run_id: DagRunId,
    node_ids: Option<&[DagNodeId]>,
    retry_options: Option<DagRetryOptions>,
) -> Result<DagRunReentry, DagNodeControlError> {
    let record = owned_record(&options, &run_id)?;
    if matches!(record.status, DagRunStatus::Running | DagRunStatus::Pending) {
        return Err(control_error(
            DagNodeControlErrorCode::RunStillActive,
            format!(
                "dag run \"{run_id}\" is still active; wait for it to settle before retrying a node"
            ),
            &run_id,
            vec![],
        ));
    }
    if retry_options
        .as_ref()
        .and_then(|options| options.prompt.as_ref())
        .is_some()
        && (node_ids.is_none() || node_ids.is_some_and(|ids| ids.len() != 1))
    {
        return Err(control_error(
            DagNodeControlErrorCode::InvalidArguments,
            "a retry prompt override requires exactly one explicit node id".to_string(),
            &run_id,
            node_ids.map_or_else(Vec::new, <[DagNodeId]>::to_vec),
        ));
    }
    let targets = node_ids.map_or_else(|| default_targets(&record), <[DagNodeId]>::to_vec);
    if targets.is_empty() {
        return Err(control_error(
            DagNodeControlErrorCode::NodeNotRetryable,
            format!("dag run \"{run_id}\" has no failed or cancelled node to retry"),
            &run_id,
            vec![],
        ));
    }
    assert_retryable(&record, &run_id, &targets)?;
    let prompt_changed = retry_options
        .as_ref()
        .and_then(|options| options.prompt.as_ref())
        .is_some();
    let mut current = record;
    if let Some(prompt) = retry_options.and_then(|options| options.prompt) {
        let original = current.clone();
        for target in &targets {
            current = amend_retry_prompt(&options, &original, target, prompt.clone())?;
        }
    }
    if current.status == DagRunStatus::Paused
        && current.pause.as_ref().is_some_and(|p| p.reason == "user_stop")
    {
        let journal = control_journal(&options, current, Default::default()).map_err(|error| {
            control_error(
                DagNodeControlErrorCode::InvalidArguments,
                error.to_string(),
                &run_id,
                vec![],
            )
        })?;
        journal
            .append(DagRunEventPayload::NodeRetryStaged {
                node_ids: targets.clone(),
            })
            .map_err(|error| {
                control_error(
                    DagNodeControlErrorCode::InvalidArguments,
                    error.to_string(),
                    &run_id,
                    vec![],
                )
            })?;
        let staged = journal.snapshot();
        let scheduler = create_dag_scheduler(DagSchedulerOptions {
            initial_record: staged.clone(),
            pre_attached_tasks: None,
            schedule_retry: None,
            ..options
        })
        .map_err(|error| {
            control_error(
                DagNodeControlErrorCode::InvalidArguments,
                error.to_string(),
                &run_id,
                vec![],
            )
        })?;
        return Ok(DagRunReentry {
            record: staged,
            scheduler,
        });
    }
    let journal = control_journal(&options, current, Default::default()).map_err(|error| {
        control_error(
            DagNodeControlErrorCode::InvalidArguments,
            error.to_string(),
            &run_id,
            vec![],
        )
    })?;
    reset_dag_nodes(&journal, &targets, prompt_changed, None).map_err(|error| {
        control_error(
            DagNodeControlErrorCode::InvalidArguments,
            error.to_string(),
            &run_id,
            targets.clone(),
        )
    })?;
    let snapshot = journal.snapshot();
    reenter_dag_run(DagSchedulerOptions {
        initial_record: snapshot,
        ..options
    })
}

/// Shared reset for manual run re-entry and infrastructure retries inside the live scheduler.
pub fn reset_dag_nodes(
    journal: &DagJournal<DagRunRecordV1>,
    targets: &[DagNodeId],
    prompt_changed: bool,
    automatic: Option<DagNodeRetryAutomatic>,
) -> Result<(), DagStoreError> {
    for node_id in targets {
        let snapshot = journal.snapshot();
        let node = find_node(&snapshot, node_id)
            .ok_or_else(|| DagStoreError::Message(format!("unknown dag node \"{node_id}\"")))?;
        journal.append(DagRunEventPayload::NodeRetried {
            node_id: node_id.clone(),
            prior_task_id: node.task_id,
            exec_attempt: node.exec_attempt.unwrap_or(0) + 1,
            prompt_changed,
            automatic: automatic.clone(),
        })?;
    }
    for node in skipped_dependents(&journal.snapshot(), targets) {
        journal.append(DagRunEventPayload::NodeTransitioned {
            node_id: node.id,
            from: DagNodeState::Skipped,
            to: DagNodeState::Blocked,
            reason: crate::dag::types::DagNodeTransitionReason::Retried,
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "node_retry_tests.rs"]
mod tests;

fn amend_retry_prompt(
    options: &DagSchedulerOptions,
    record: &DagRunRecordV1,
    node_id: &DagNodeId,
    prompt: String,
) -> Result<DagRunRecordV1, DagNodeControlError> {
    let definition = DagDefinition {
        key: record.definition.key.clone(),
        name: record.definition.name.clone(),
        nodes: record
            .definition
            .nodes
            .iter()
            .map(|persisted| {
                let (target, label, depends_on, task_summary, description, load_skills) =
                    match &persisted.target {
                        crate::dag::manager::DagPersistedNodeTarget::Category { category } => (
                            crate::dag::types::DagNodeTarget::Category(category.clone()),
                            persisted.label.clone(),
                            persisted.depends_on.clone(),
                            persisted.task_summary.clone(),
                            persisted.description.clone(),
                            persisted.load_skills.clone(),
                        ),
                        crate::dag::manager::DagPersistedNodeTarget::SubagentType {
                            subagent_type,
                            model,
                        } => (
                            crate::dag::types::DagNodeTarget::SubagentType {
                                subagent_type: subagent_type.clone(),
                                model: model.clone(),
                            },
                            persisted.label.clone(),
                            persisted.depends_on.clone(),
                            persisted.task_summary.clone(),
                            persisted.description.clone(),
                            persisted.load_skills.clone(),
                        ),
                    };
                DagNodeInput {
                    id: persisted.id.clone(),
                    prompt: if &persisted.id == node_id {
                        prompt.clone()
                    } else {
                        persisted.prompt.clone()
                    },
                    target,
                    label,
                    depends_on,
                    task_summary,
                    description,
                    load_skills,
                }
            })
            .collect(),
    };
    let manager = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&options.store),
        now: options.now.clone(),
        new_run_id: None,
        materialize_skills: None,
        settings: None,
    });
    let amended = manager.amend(DagAmendParams {
        run_id: Some(record.run_id.clone()),
        key: None,
        definition,
        parent_session_id: record.parent_session_id.clone(),
    });
    match amended {
        Ok(amended) => Ok(amended),
        Err(_error) => Err(control_error(
            DagNodeControlErrorCode::InvalidArguments,
            format!("dag node \"{node_id}\" prompt override was rejected"),
            &record.run_id,
            vec![node_id.clone()],
        )),
    }
}

fn default_targets(record: &DagRunRecordV1) -> Vec<DagNodeId> {
    let seeds: Vec<_> = record
        .nodes
        .iter()
        .filter(|node| matches!(node.state, DagNodeState::Failed | DagNodeState::Cancelled))
        .map(|node| node.id.clone())
        .collect();
    let mut targets = seeds.clone();
    targets.extend(
        skipped_dependents(record, &seeds)
            .into_iter()
            .map(|node| node.id),
    );
    targets
}

pub fn skipped_dependents(record: &DagRunRecordV1, seeds: &[DagNodeId]) -> Vec<DagNode> {
    let mut selected: BTreeSet<_> = seeds.iter().cloned().collect();
    let mut restored = Vec::new();
    loop {
        let additions: Vec<_> = record
            .nodes
            .iter()
            .filter(|node| {
                node.state == DagNodeState::Skipped
                    && !selected.contains(&node.id)
                    && node
                        .depends_on
                        .iter()
                        .any(|dependency| selected.contains(dependency))
            })
            .cloned()
            .collect();
        if additions.is_empty() {
            break;
        }
        for node in additions {
            selected.insert(node.id.clone());
            restored.push(node);
        }
    }
    restored
}

fn assert_retryable(
    record: &DagRunRecordV1,
    run_id: &DagRunId,
    targets: &[DagNodeId],
) -> Result<(), DagNodeControlError> {
    let selected: BTreeSet<_> = targets.iter().cloned().collect();
    for id in targets {
        let Some(node) = find_node(record, id) else {
            return Err(control_error(
                DagNodeControlErrorCode::NodeNotFound,
                format!("unknown dag node \"{id}\""),
                run_id,
                vec![id.clone()],
            ));
        };
        if node.state == DagNodeState::Completed {
            return Err(control_error(
                DagNodeControlErrorCode::NodeNotRetryable,
                format!("dag node \"{id}\" already completed; amend the definition to re-run it"),
                run_id,
                vec![id.clone()],
            ));
        }
        if !RETRYABLE.contains(&node.state) {
            return Err(control_error(
                DagNodeControlErrorCode::NodeNotRetryable,
                format!(
                    "dag node \"{id}\" is {}; only failed, cancelled, or skipped nodes can be retried",
                    node.state.as_str()
                ),
                run_id,
                vec![id.clone()],
            ));
        }
        if node.state == DagNodeState::Skipped {
            let blocking = blocking_ancestors(record, &node)
                .into_iter()
                .filter(|ancestor| !selected.contains(ancestor))
                .collect::<Vec<_>>();
            if !blocking.is_empty() {
                let joined = blocking.join(", ");
                let mut ids = vec![id.clone()];
                ids.extend(blocking);
                return Err(control_error(
                    DagNodeControlErrorCode::NodeNotRetryable,
                    format!(
                        "dag node \"{id}\" was skipped because {joined} did not complete; retry those in the same call"
                    ),
                    run_id,
                    ids,
                ));
            }
        }
    }
    Ok(())
}

fn blocking_ancestors(record: &DagRunRecordV1, node: &DagNode) -> Vec<DagNodeId> {
    let mut queue: std::collections::VecDeque<DagNodeId> =
        node.depends_on.iter().cloned().collect();
    let mut seen = BTreeSet::from([node.id.clone()]);
    let mut blocking = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(ancestor) = find_node(record, &id) {
            if matches!(
                ancestor.state,
                DagNodeState::Failed | DagNodeState::Cancelled
            ) {
                blocking.insert(id);
            }
            if ancestor.state == DagNodeState::Skipped {
                queue.extend(ancestor.depends_on);
            }
        }
    }
    blocking.into_iter().collect()
}

fn find_node(record: &DagRunRecordV1, id: &DagNodeId) -> Option<DagNode> {
    record.nodes.iter().find(|node| &node.id == id).cloned()
}

fn control_error(
    code: DagNodeControlErrorCode,
    message: String,
    run_id: &DagRunId,
    ids: Vec<DagNodeId>,
) -> DagNodeControlError {
    DagNodeControlError::new(code, message)
        .with_run_id(run_id.clone())
        .with_node_ids(ids)
}
