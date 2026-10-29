//! Steering types for ulw-loop.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UlwLoopSteeringInvariantResult {
    pub accepted: bool,
    #[serde(rename = "structuralInvariantAccepted")]
    pub structural_invariant_accepted: bool,
    #[serde(rename = "evidenceBackedNecessity")]
    pub evidence_backed_necessity: bool,
    #[serde(rename = "noEasierCompletion")]
    pub no_easier_completion: bool,
    #[serde(rename = "rejectedReasons", default)]
    pub rejected_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasons: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopSteeringChildGoal {
    pub title: String,
    pub objective: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UlwLoopSteeringAfterPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(rename = "pendingGoalIds", default, skip_serializing_if = "Option::is_none")]
    pub pending_goal_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<UlwLoopSteeringChildGoal>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UlwLoopSteeringProposal {
    pub kind: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(rename = "goalId", default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(rename = "targetGoalId", default, skip_serializing_if = "Option::is_none")]
    pub target_goal_id: Option<String>,
    #[serde(rename = "targetGoalIds", default, skip_serializing_if = "Option::is_none")]
    pub target_goal_ids: Option<Vec<String>>,
    #[serde(rename = "criterionId", default, skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
    #[serde(rename = "expectedEvidence", default, skip_serializing_if = "Option::is_none")]
    pub expected_evidence: Option<String>,
    #[serde(rename = "userModel", default, skip_serializing_if = "Option::is_none")]
    pub user_model: Option<String>,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(rename = "childGoals", default, skip_serializing_if = "Option::is_none")]
    pub child_goals: Option<Vec<UlwLoopSteeringChildGoal>>,
    #[serde(rename = "revisedTitle", default, skip_serializing_if = "Option::is_none")]
    pub revised_title: Option<String>,
    #[serde(rename = "revisedObjective", default, skip_serializing_if = "Option::is_none")]
    pub revised_objective: Option<String>,
    #[serde(rename = "pendingOrder", default, skip_serializing_if = "Option::is_none")]
    pub pending_order: Option<Vec<String>>,
    #[serde(rename = "blockedReason", default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<UlwLoopSteeringAfterPayload>,
    #[serde(rename = "directiveText", default, skip_serializing_if = "Option::is_none")]
    pub directive_text: Option<String>,
    #[serde(rename = "promptSignature", default, skip_serializing_if = "Option::is_none")]
    pub prompt_signature: Option<String>,
    #[serde(rename = "idempotencyKey", default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(skip)]
    pub raw_proposal: Option<Value>,
}

pub type UlwLoopSteeringInvariantAudit = UlwLoopSteeringInvariantResult;
pub type UlwLoopSteeringMutationKind = String;
pub type UlwLoopSteeringSource = String;

fn default_source() -> String {
    "cli".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopSteeringPlanSnapshot {
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "activeGoalId", default, skip_serializing_if = "Option::is_none")]
    pub active_goal_id: Option<String>,
    #[serde(rename = "goalCount")]
    pub goal_count: usize,
    #[serde(rename = "goalIds")]
    pub goal_ids: Vec<String>,
    pub goals: Vec<crate::domain_types::UlwLoopItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopSteeringAudit {
    pub kind: String,
    pub source: String,
    #[serde(rename = "targetGoalIds")]
    pub target_goal_ids: Vec<String>,
    #[serde(rename = "criterionId", default, skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<UlwLoopSteeringPlanSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<UlwLoopSteeringPlanSnapshot>,
    pub evidence: String,
    pub rationale: String,
    pub invariant: UlwLoopSteeringInvariantResult,
    #[serde(rename = "directiveText", default, skip_serializing_if = "Option::is_none")]
    pub directive_text: Option<String>,
    #[serde(rename = "promptSignature", default, skip_serializing_if = "Option::is_none")]
    pub prompt_signature: Option<String>,
    #[serde(rename = "idempotencyKey", default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduped: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SteerUlwLoopResult {
    pub plan: crate::domain_types::UlwLoopPlan,
    pub accepted: bool,
    pub audit: UlwLoopSteeringAudit,
    #[serde(rename = "rejectedReasons")]
    pub rejected_reasons: Vec<String>,
    pub deduped: bool,
}
