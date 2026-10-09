//! Command argument types for ulw-loop.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateUlwLoopOptions {
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goals: Option<Vec<CreateUlwLoopGoalCandidate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_goal_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_batches_json: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateUlwLoopGoalCandidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub objective: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StartNextOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_failed: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointOptions {
    pub goal_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_goal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_gate: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_active_final_codex_goal: Option<bool>,
}



#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordFinalReviewBlockersOptions {
    pub goal_id: String,
    pub title: String,
    pub objective: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_goal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_goal_json: Option<String>,
}

pub type CreateGoalsOptions = CreateUlwLoopOptions;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddGoalOptions {
    pub title: String,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(rename = "successCriteria", default, skip_serializing_if = "Option::is_none")]
    pub success_criteria: Option<Vec<crate::success_criteria_input::SuccessCriterionInput>>,
}

pub type AddUlwLoopGoalOptions = AddGoalOptions;

pub type RecordEvidenceOptions = crate::evidence::RecordEvidenceArgs;
