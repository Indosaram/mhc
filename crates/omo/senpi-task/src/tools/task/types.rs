//! `tools/task/types.ts`: the task tool's resolved spawn items, skills, and details payloads.

use serde::Serialize;

use crate::state::{IsolationMergeMode, ResolvedModelRecord, TaskRunStats};

/// The resolved target of one spawn item: category XOR subagent_type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnTarget {
    Category(String),
    SubagentType(String),
}

/// One spawn after batch/single resolution and inheritance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpawnItem {
    pub prompt: String,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub name: Option<String>,
    pub model: Option<String>,
    /// The isolation flags after item-over-params inheritance; `None` means "not requested here".
    pub isolated: Option<bool>,
    pub apply: Option<bool>,
    pub merge: Option<IsolationMergeMode>,
    pub load_skills: Vec<String>,
    pub target: SpawnTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    pub name: String,
    pub content: String,
    pub location: Option<String>,
}

/// v1 load_skills contract: a ready-to-prepend block plus which names resolved vs went missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillResolution {
    pub prepend: String,
    pub resolved: Vec<String>,
    pub missing: Vec<String>,
    pub skills: Option<Vec<LoadedSkill>>,
}

/// Resolves named skills relative to a cwd.
pub type SkillLoader = dyn Fn(&[String], &str) -> SkillResolution + Send + Sync;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TaskSkillSummary {
    pub requested: Vec<String>,
    pub resolved: Vec<String>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TaskToolItemDetail {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModelRecord>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_in_background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<TaskSkillSummary>,
}

/// The only task tool mode: `"spawn"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskToolMode {
    #[default]
    Spawn,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TaskToolDetails {
    pub task_id: String,
    pub status: String,
    pub mode: TaskToolMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_attempts: Option<Vec<ResolvedModelRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_in_background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<TaskToolItemDetail>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_stats: Option<TaskRunStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<TaskSkillSummary>,
}
