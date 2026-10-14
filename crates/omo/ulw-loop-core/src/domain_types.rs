//! Core domain models and ledger records.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopSuccessCriterion {
    pub id: String,
    pub scenario: String,
    #[serde(rename = "userModel")]
    pub user_model: String,
    #[serde(rename = "expectedEvidence")]
    pub expected_evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub essential: Option<bool>,
    #[serde(rename = "capturedEvidence")]
    pub captured_evidence: Option<String>,
    pub status: String,
    #[serde(rename = "capturedAt", default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopItem {
    pub id: String,
    pub title: String,
    pub objective: String,
    pub status: String,
    #[serde(rename = "successCriteria", default)]
    pub success_criteria: Vec<UlwLoopSuccessCriterion>,
    pub attempt: u32,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(rename = "completedAt", default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(rename = "failedAt", default, skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<String>,
    #[serde(rename = "reviewBlockedAt", default, skip_serializing_if = "Option::is_none")]
    pub review_blocked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(rename = "failureReason", default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(rename = "steeringStatus", default, skip_serializing_if = "Option::is_none")]
    pub steering_status: Option<String>,
    #[serde(rename = "supersededBy", default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Vec<String>>,
    #[serde(rename = "blockedReason", default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(rename = "blockerSignature", default, skip_serializing_if = "Option::is_none")]
    pub blocker_signature: Option<String>,
    #[serde(rename = "blockerOccurrenceCount", default, skip_serializing_if = "Option::is_none")]
    pub blocker_occurrence_count: Option<u32>,
    #[serde(rename = "requiredExternalDecision", default, skip_serializing_if = "Option::is_none")]
    pub required_external_decision: Option<String>,
    #[serde(rename = "nonRetriable", default, skip_serializing_if = "Option::is_none")]
    pub non_retriable: Option<bool>,
    #[serde(rename = "steeringEvidence", default, skip_serializing_if = "Option::is_none")]
    pub steering_evidence: Option<String>,
    #[serde(rename = "steeringRationale", default, skip_serializing_if = "Option::is_none")]
    pub steering_rationale: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopAggregateCompletion {
    pub status: String,
    #[serde(rename = "completedAt")]
    pub completed_at: String,
    pub evidence: String,
    #[serde(rename = "codexGoal", default, skip_serializing_if = "Option::is_none")]
    pub codex_goal: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UlwLoopValidationBatch {
    #[serde(rename = "batchId")]
    pub batch_id: String,
    #[serde(rename = "memberIds")]
    pub member_ids: Vec<String>,
    #[serde(rename = "finalGoalId")]
    pub final_goal_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopPlan {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(rename = "ledgerResetRevision", default, skip_serializing_if = "Option::is_none")]
    pub ledger_reset_revision: Option<u64>,
    #[serde(rename = "evidenceLayoutVersion", default, skip_serializing_if = "Option::is_none")]
    pub evidence_layout_version: Option<u32>,
    // Pin `plan-io.ts:73-81`: only `version` and `goals` are validated upstream; the rest of the
    // parsed document is trusted as-is, so these fields tolerate absence instead of invalidating it.
    #[serde(rename = "createdAt", default)]
    pub created_at: String,
    #[serde(rename = "updatedAt", default)]
    pub updated_at: String,
    #[serde(rename = "briefPath", default)]
    pub brief_path: String,
    #[serde(rename = "goalsPath", default)]
    pub goals_path: String,
    #[serde(rename = "ledgerPath", default)]
    pub ledger_path: String,
    #[serde(rename = "codexGoalMode", default, skip_serializing_if = "Option::is_none")]
    pub codex_goal_mode: Option<String>,
    #[serde(rename = "codexObjective", default, skip_serializing_if = "Option::is_none")]
    pub codex_objective: Option<String>,
    #[serde(rename = "codexObjectiveAliases", default, skip_serializing_if = "Option::is_none")]
    pub codex_objective_aliases: Option<Vec<String>>,
    #[serde(rename = "acknowledgedDriverObjectives", default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_driver_objectives: Option<Vec<String>>,
    #[serde(rename = "aggregateCompletion", default, skip_serializing_if = "Option::is_none")]
    pub aggregate_completion: Option<UlwLoopAggregateCompletion>,
    #[serde(rename = "activeGoalId", default, skip_serializing_if = "Option::is_none")]
    pub active_goal_id: Option<String>,
    #[serde(rename = "validationBatches", default, skip_serializing_if = "Option::is_none")]
    pub validation_batches: Option<Vec<UlwLoopValidationBatch>>,
    #[serde(default)]
    pub goals: Vec<UlwLoopItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopManualQaArtifactRef {
    pub id: String,
    pub kind: String,
    pub description: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopManualQaSurfaceEvidence {
    pub id: String,
    #[serde(rename = "criterionRef")]
    pub criterion_ref: String,
    pub surface: String,
    pub invocation: String,
    pub verdict: String,
    #[serde(rename = "artifactRefs")]
    pub artifact_refs: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopManualQaAdversarialCase {
    pub id: String,
    #[serde(rename = "criterionRef")]
    pub criterion_ref: String,
    pub scenario: String,
    #[serde(rename = "expectedBehavior")]
    pub expected_behavior: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "artifactRefs")]
    pub artifact_refs: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopQualityGateManualQa {
    pub by: String,
    pub status: String,
    pub evidence: String,
    #[serde(rename = "surfaceEvidence", default)]
    pub surface_evidence: Vec<UlwLoopManualQaSurfaceEvidence>,
    #[serde(rename = "adversarialCases", default)]
    pub adversarial_cases: Vec<UlwLoopManualQaAdversarialCase>,
    #[serde(rename = "artifactRefs", default)]
    pub artifact_refs: Vec<UlwLoopManualQaArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopQualityGateGateReview {
    pub by: String,
    pub recommendation: String,
    #[serde(rename = "reportPath")]
    pub report_path: String,
    pub evidence: String,
    #[serde(default)]
    pub blockers: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopQualityGateCodeReview {
    pub by: String,
    pub recommendation: String,
    #[serde(rename = "codeQualityStatus")]
    pub code_quality_status: String,
    #[serde(rename = "reportPath")]
    pub report_path: String,
    pub evidence: String,
    #[serde(default)]
    pub blockers: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopQualityGateIteration {
    #[serde(rename = "fullRerun")]
    pub full_rerun: bool,
    pub status: String,
    #[serde(rename = "rerunCommands")]
    pub rerun_commands: Vec<String>,
    pub evidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopQualityGateCriteriaCoverage {
    #[serde(rename = "totalCriteria")]
    pub total_criteria: usize,
    #[serde(rename = "passCount")]
    pub pass_count: usize,
    #[serde(rename = "originalIntent")]
    pub original_intent: String,
    #[serde(rename = "desiredOutcome")]
    pub desired_outcome: String,
    #[serde(rename = "userOutcomeReview")]
    pub user_outcome_review: String,
    #[serde(rename = "adversarialClassesCovered", default)]
    pub adversarial_classes_covered: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopQualityGate {
    pub surface: String,
    #[serde(rename = "manualQa")]
    pub manual_qa: UlwLoopQualityGateManualQa,
    #[serde(rename = "gateReview")]
    pub gate_review: UlwLoopQualityGateGateReview,
    #[serde(rename = "codeReview", default, skip_serializing_if = "Option::is_none")]
    pub code_review: Option<UlwLoopQualityGateCodeReview>,
    pub iteration: UlwLoopQualityGateIteration,
    #[serde(rename = "criteriaCoverage")]
    pub criteria_coverage: UlwLoopQualityGateCriteriaCoverage,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UlwLoopLedgerEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub at: String,
    pub kind: String,
    #[serde(rename = "goalId", default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(rename = "criterionId", default, skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "criterionStatus", default, skip_serializing_if = "Option::is_none")]
    pub criterion_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "codexGoal", default, skip_serializing_if = "Option::is_none")]
    pub codex_goal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(rename = "capturedEvidence", default, skip_serializing_if = "Option::is_none")]
    pub captured_evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<String>>,
    #[serde(rename = "qualityGate", default, skip_serializing_if = "Option::is_none")]
    pub quality_gate: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering: Option<crate::steering_types::UlwLoopSteeringAudit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    #[serde(rename = "mutationKind", default, skip_serializing_if = "Option::is_none")]
    pub mutation_kind: Option<String>,
    #[serde(rename = "idempotencyKey", default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(rename = "blockerSignature", default, skip_serializing_if = "Option::is_none")]
    pub blocker_signature: Option<String>,
    #[serde(rename = "blockerOccurrenceCount", default, skip_serializing_if = "Option::is_none")]
    pub blocker_occurrence_count: Option<u32>,
    #[serde(rename = "requiredExternalDecision", default, skip_serializing_if = "Option::is_none")]
    pub required_external_decision: Option<String>,
}
