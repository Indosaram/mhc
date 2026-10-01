//! `tools/task/spawn-policy.ts`: single composition point for every subagent_type spawn
//! restriction, used by both the single and batch spawn paths: the plan gate first (invocation
//! guard), then the plan-review prompt contract.

use crate::manager::TaskManager;

/// Outcome of the plan-review prompt contract for one spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReviewContractOutcome {
    Deny { message: String },
    Prompt { prompt: String },
}

/// The gate/contract seams the policy composes (`invocationGateDenial`, `planReviewContractOutcome`).
/// Both hooks default to "no restriction", mirroring the TS deps object that wires neither hook.
pub trait SpawnPolicyDeps {
    fn invocation_gate_denial(&self, _subagent_type: &str, _session_id: &str) -> Option<String> {
        None
    }

    fn plan_review_contract_outcome(
        &self,
        _subagent_type: &str,
        _caller_prompt: &str,
        _session_id: &str,
    ) -> Option<PlanReviewContractOutcome> {
        None
    }
}

/// The manager itself carries no spawn-policy seams: it is used directly as `policy` in tests
/// that never exercise plan-gated agents, mirroring the TS fakes that pass no
/// `invocationGateDenial` / `planReviewContractOutcome` hooks.
impl SpawnPolicyDeps for TaskManager {
    fn invocation_gate_denial(&self, _subagent_type: &str, _session_id: &str) -> Option<String> {
        None
    }

    fn plan_review_contract_outcome(
        &self,
        _subagent_type: &str,
        _caller_prompt: &str,
        _session_id: &str,
    ) -> Option<PlanReviewContractOutcome> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnPolicyVerdict {
    Allow,
    Deny { message: String },
    Force { prompt: String },
}

pub fn evaluate_spawn_policy(
    deps: &dyn SpawnPolicyDeps,
    subagent_type: &str,
    caller_prompt: &str,
    session_id: &str,
) -> SpawnPolicyVerdict {
    if let Some(message) = deps.invocation_gate_denial(subagent_type, session_id) {
        return SpawnPolicyVerdict::Deny { message };
    }
    match deps.plan_review_contract_outcome(subagent_type, caller_prompt, session_id) {
        Some(PlanReviewContractOutcome::Deny { message }) => SpawnPolicyVerdict::Deny { message },
        Some(PlanReviewContractOutcome::Prompt { prompt }) => SpawnPolicyVerdict::Force { prompt },
        None => SpawnPolicyVerdict::Allow,
    }
}
