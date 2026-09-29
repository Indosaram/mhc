use super::id::{TaskIdSpaceExhaustedError, create_task_id};
use super::types::{ResidencyState, TaskNotification, TaskRecord, TaskRecordInput, TaskStatus};

pub(crate) fn iso_timestamp_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Builds a pending, resident record; only the declared input fields are copied.
pub fn create_task_record(
    input: TaskRecordInput,
    now_ms: Option<u64>,
) -> Result<TaskRecord, TaskIdSpaceExhaustedError> {
    let timestamp = iso_timestamp_now();
    let task_id = create_task_id(now_ms)?;
    let TaskRecordInput {
        name,
        task_summary,
        description,
        parent_session_id,
        root_session_id,
        depth,
        agent_type,
        category,
        execution_mode,
        model,
        requested_model,
        fallback_models,
        fallback_attempts,
        resolved_model,
        tool_allow,
        tool_deny,
        notify_on_terminal,
        pending_steering,
        owner,
    } = input;
    Ok(TaskRecord {
        task_id: task_id.to_string(),
        status: TaskStatus::Pending,
        residency_state: ResidencyState::Resident,
        parent_session_id,
        root_session_id,
        depth,
        execution_mode,
        model,
        notify_on_terminal,
        created_at: timestamp.clone(),
        updated_at: timestamp,
        notification: TaskNotification::default(),
        name,
        task_summary,
        description,
        agent_type,
        category,
        tool_allow,
        tool_deny,
        requested_model,
        fallback_models,
        fallback_attempts,
        resolved_model,
        spawn_spec: None,
        owner,
        pending_steering: pending_steering.filter(|entries| !entries.is_empty()),
        pid: None,
        host_pid: None,
        child_session_id: None,
        final_response: None,
        error_message: None,
        killed: None,
        run_stats: None,
    })
}
