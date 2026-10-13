//! `dag/node-activity.ts`: best-effort transcript activity timestamp projection.

use std::fs;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::state::parse_task_id;
use crate::store::event_log_path;

/// Read the transcript mtime for a task as an ISO timestamp; return `None` on any fault.
pub fn read_dag_node_activity_at(state_dir: &Path, task_id: &str) -> Option<String> {
    let task_id = parse_task_id(task_id).ok()?;
    let modified = fs::metadata(event_log_path(state_dir, task_id))
        .ok()?
        .modified()
        .ok()?;
    let timestamp: DateTime<Utc> = modified.into();
    Some(timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
}
