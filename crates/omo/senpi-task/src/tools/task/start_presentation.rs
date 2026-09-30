//! `tools/task/start-presentation.ts`: the model-facing text for background starts.

use crate::manager::types::StartedTask;
use crate::state::ResolvedModelRecord;

/// The fields of a started result the presentation (and details) layer reads.
pub trait StartedView {
    fn task_id(&self) -> &str;
    fn status(&self) -> &str;
    fn name(&self) -> &str;
    fn queue_position(&self) -> Option<u64>;
    fn resolved_model(&self) -> Option<&ResolvedModelRecord> {
        None
    }
}

impl StartedView for StartedTask {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn status(&self) -> &str {
        self.status.as_str()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn queue_position(&self) -> Option<u64> {
        self.queue_position.map(|position| position as u64)
    }

    fn resolved_model(&self) -> Option<&ResolvedModelRecord> {
        self.resolved_model.as_ref()
    }
}

/// A plain started summary, for callers that do not hold a manager `StartedTask`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartedSummary {
    pub task_id: String,
    pub status: String,
    pub name: String,
    pub queue_position: Option<u64>,
}

impl StartedView for StartedSummary {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn status(&self) -> &str {
        &self.status
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn queue_position(&self) -> Option<u64> {
        self.queue_position
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartLabels {
    pub task_summary: Option<String>,
    pub description: Option<String>,
}

pub fn background_start_text(started: &impl StartedView, labels: &StartLabels) -> String {
    let queue = started
        .queue_position()
        .map(|position| format!(" queued at position {position}"))
        .unwrap_or_default();
    let label = labels
        .task_summary
        .as_deref()
        .or(labels.description.as_deref())
        .unwrap_or(started.name());
    let task_id = started.task_id();
    let status = started.status();
    if label == task_id {
        return format!(
            "Started task {task_id} ({status}){queue}. Completion is automatically delivered. End your turn if no independent work remains; otherwise keep working. Use task_send only to steer it."
        );
    }
    format!(
        "Started task {label} ({task_id}, {status}){queue}. Completion is automatically delivered. End your turn if no independent work remains; otherwise keep working. Use task_send only to steer it."
    )
}

pub fn background_conversion_text(
    started: &impl StartedView,
    labels: &StartLabels,
    budget_seconds: i64,
) -> String {
    let prefix = format!(
        "Foreground wait reached the prompt-cache-safe budget ({budget_seconds}s) for task {}; the task continues in background. Completion will arrive as a notification; steer with task_send, read with task_output.",
        started.task_id()
    );
    format!("{prefix}\n\n{}", background_start_text(started, labels))
}
