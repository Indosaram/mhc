//! SDK request and response types.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::checkpoint::{CheckpointUlwLoopArgs, CheckpointUlwLoopResult};
use crate::checkpoint_template::CheckpointTemplate;
use crate::command_types::{
    AddGoalOptions, CreateGoalsOptions, RecordEvidenceOptions, RecordFinalReviewBlockersOptions,
};
use crate::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::plan_crud::{StartNextResult, UlwLoopPlanSummary};
use crate::review_blockers::RecordFinalReviewBlockersResult;
use crate::sdk::manifest::ToolkitManifest;
use crate::steering_types::{SteerUlwLoopResult, UlwLoopSteeringProposal};
use crate::surface::UlwLoopToolkitSurface;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolkitContext {
    pub cwd: String,
    pub session_id: String,
    pub surface: UlwLoopToolkitSurface,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolkitError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusResult {
    pub plan: UlwLoopPlan,
    pub summary: UlwLoopPlanSummary,
    #[serde(rename = "nextActions")]
    pub next_actions: Vec<String>,
    #[serde(rename = "evidenceRoot")]
    pub evidence_root: String,
    #[serde(rename = "currentAttemptDir", default, skip_serializing_if = "Option::is_none")]
    pub current_attempt_dir: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CriteriaResult {
    #[serde(rename = "goalId")]
    pub goal_id: String,
    pub criteria: Vec<UlwLoopSuccessCriterion>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolkitResponse<T> {
    Success {
        ok: bool,
        operation: String,
        result: T,
        #[serde(rename = "nextActions")]
        next_actions: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    Failure {
        ok: bool,
        operation: String,
        error: ToolkitError,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointArgs {
    #[serde(rename = "goalId", default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(rename = "codexGoalJson", default, skip_serializing_if = "Option::is_none")]
    pub codex_goal_json: Option<String>,
    #[serde(rename = "qualityGateJson", default, skip_serializing_if = "Option::is_none")]
    pub quality_gate_json: Option<String>,
    #[serde(rename = "printTemplate", default, skip_serializing_if = "Option::is_none")]
    pub print_template: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompleteGoalsArgs {
    #[serde(rename = "retryFailed", default)]
    pub retry_failed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", content = "args")]
pub enum ToolkitDispatchRequest {
    #[serde(rename = "help")]
    Help,
    #[serde(rename = "create-goals")]
    CreateGoals(CreateGoalsOptions),
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "complete-goals")]
    CompleteGoals(Option<CompleteGoalsArgs>),
    #[serde(rename = "checkpoint")]
    Checkpoint(CheckpointArgs),
    #[serde(rename = "steer")]
    Steer(UlwLoopSteeringProposal),
    #[serde(rename = "add-goal")]
    AddGoal(AddGoalOptions),
    #[serde(rename = "criteria")]
    Criteria {
        #[serde(rename = "goalId")]
        goal_id: String,
    },
    #[serde(rename = "record-evidence")]
    RecordEvidence(RecordEvidenceOptions),
    #[serde(rename = "record-review-blockers")]
    RecordReviewBlockers(RecordFinalReviewBlockersOptions),
}
