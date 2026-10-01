//! Port of `tools/output/snapshot.ts`.

use chrono::DateTime;

use crate::state::TaskRecord;
use crate::tools::output::transcript::session_dir::child_session_dir;
use crate::tools::output::types::{LostBreadcrumbs, SuspendedDetails, TaskSnapshot};

pub const LOST_EXPLANATION: &str = "The task was marked lost: its process disappeared before a terminal result was recorded (crash, host restart, or an evicted resident child). Inspect the pid and session dir below; no result was captured.";

pub const SUSPENDED_EXPLANATION: &str = "suspended (resumes with session)";

const SUSPENDED_RESIDENCIES: [&str; 2] = ["persisted_only", "rpc_detached"];

/// Record snapshot for task_output status view (pi-task task-status result fields). For a `lost` task
/// it attaches read-only breadcrumbs (pid + the child's session dir) so the caller can investigate
/// without task_output ever reviving or touching child state.
pub fn build_task_snapshot(record: &TaskRecord, state_dir: &str, now: i64) -> TaskSnapshot {
    TaskSnapshot {
        task_id: record.task_id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        task_summary: record.task_summary.clone(),
        status: record.status,
        residency_state: record.residency_state,
        suspended: is_suspended(record).then(|| SuspendedDetails {
            explanation: SUSPENDED_EXPLANATION.to_string(),
        }),
        execution_mode: record.execution_mode.clone(),
        model: record.model.clone(),
        resolved_model: record.resolved_model.clone(),
        agent_type: record.agent_type.clone(),
        category: record.category.clone(),
        parent_session_id: record.parent_session_id.clone(),
        root_session_id: record.root_session_id.clone(),
        age_ms: age_ms(record, now),
        pid: record.pid,
        child_session_id: record.child_session_id.clone(),
        final_response: record.final_response.clone(),
        error_message: record.error_message.clone(),
        run_stats: record.run_stats.clone(),
        lost: (record.status.as_str() == "lost").then(|| lost_breadcrumbs(record, state_dir)),
    }
}

fn is_suspended(record: &TaskRecord) -> bool {
    SUSPENDED_RESIDENCIES.contains(&record.residency_state.as_str())
}

fn lost_breadcrumbs(record: &TaskRecord, state_dir: &str) -> LostBreadcrumbs {
    LostBreadcrumbs {
        explanation: LOST_EXPLANATION.to_string(),
        session_dir: child_session_dir(state_dir, &record.task_id),
        pid: record.pid,
    }
}

fn age_ms(record: &TaskRecord, now: i64) -> i64 {
    match DateTime::parse_from_rfc3339(&record.created_at) {
        Ok(created) => (now - created.timestamp_millis()).max(0),
        Err(_) => 0,
    }
}
