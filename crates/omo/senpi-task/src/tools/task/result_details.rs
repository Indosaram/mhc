//! `tools/task/result-details.ts`: details payloads built from records and start results.

use serde::Serialize;
use serde_json::Value;

use crate::manager::execution_mode::ExecutionMode;
use crate::progress::ToolProgressDetails;
use crate::state::{TaskRecord, TaskRunStats};
use crate::tools::task::start_presentation::StartedView;
use crate::tools::task::types::{TaskSkillSummary, TaskToolDetails, TaskToolMode};

/// Task params minus `prompt`/`tasks`, plus a required prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SingleSpawnParams {
    pub prompt: String,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub run_in_background: Option<bool>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub load_skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordLifecycle {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    pub residency_state: String,
    pub depth: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordSummary {
    pub task_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub execution_mode: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_stats: Option<TaskRunStats>,
    #[serde(flatten)]
    pub lifecycle: Option<RecordLifecycle>,
}

pub fn record_summary(record: &TaskRecord, include_lifecycle: bool) -> RecordSummary {
    RecordSummary {
        task_id: record.task_id.clone(),
        status: record.status.as_str().to_string(),
        task_summary: record.task_summary.clone(),
        name: record.name.clone(),
        category: record.category.clone(),
        execution_mode: record.execution_mode.as_str().to_string(),
        model: record.model.clone(),
        run_stats: record.run_stats.clone(),
        lifecycle: include_lifecycle.then(|| RecordLifecycle {
            description: record.description.clone(),
            agent_type: record.agent_type.clone(),
            residency_state: record.residency_state.as_str().to_string(),
            depth: Value::from(record.depth),
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
        }),
    }
}

pub fn record_details(record: &TaskRecord, mode: TaskToolMode) -> TaskToolDetails {
    TaskToolDetails {
        task_id: record.task_id.clone(),
        status: record.status.as_str().to_string(),
        mode,
        task_summary: record.task_summary.clone(),
        name: record.name.clone(),
        category: record.category.clone(),
        subagent_type: record.agent_type.clone(),
        execution_mode: Some(record.execution_mode.as_str().to_string()),
        model: Some(record.model.clone()),
        resolved_model: record.resolved_model.clone(),
        fallback_attempts: record.fallback_attempts.clone(),
        run_in_background: Some(false),
        run_stats: record.run_stats.clone(),
        ..TaskToolDetails::default()
    }
}

pub fn started_details(
    started: &impl StartedView,
    params: &SingleSpawnParams,
    execution_mode: ExecutionMode,
    skills: Option<TaskSkillSummary>,
) -> TaskToolDetails {
    TaskToolDetails {
        task_id: started.task_id().to_string(),
        status: started.status().to_string(),
        mode: TaskToolMode::Spawn,
        task_summary: params.task_summary.clone(),
        name: Some(started.name().to_string()),
        category: params.category.clone(),
        subagent_type: params.subagent_type.clone(),
        execution_mode: Some(execution_mode.as_str().to_string()),
        model: params.model.clone(),
        resolved_model: started.resolved_model().cloned(),
        run_in_background: Some(params.run_in_background == Some(true)),
        queue_position: started.queue_position(),
        skills,
        ..TaskToolDetails::default()
    }
}

/// `TaskToolDetails & ToolProgressDetails`.
pub struct PartialTaskToolDetails {
    pub details: TaskToolDetails,
    pub progress: ToolProgressDetails,
}

pub fn partial_details(
    started: &impl StartedView,
    params: &SingleSpawnParams,
    execution_mode: ExecutionMode,
    progress: ToolProgressDetails,
    skills: Option<TaskSkillSummary>,
) -> PartialTaskToolDetails {
    PartialTaskToolDetails {
        details: started_details(started, params, execution_mode, skills),
        progress,
    }
}
