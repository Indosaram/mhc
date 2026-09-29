//! One-shot interaction policies and the plan-gated invocation guard
//! (`agents/interaction-policy.ts`, `agents/invocation-guard.ts`).

use std::collections::BTreeSet;

use super::builtin::MOMUS_SEND_DENIAL_REMINDER;

/// A fire-and-forget agent: task/task_cancel/task_output only, never task_send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentInteractionPolicy {
    pub one_shot: bool,
    pub prompt_contract: &'static str,
    pub send_denial_reminder: &'static str,
}

pub const AGENT_INTERACTION_POLICIES: [(&str, AgentInteractionPolicy); 1] = [(
    "momus",
    AgentInteractionPolicy {
        one_shot: true,
        prompt_contract: "plan-review",
        send_denial_reminder: MOMUS_SEND_DENIAL_REMINDER,
    },
)];

pub fn one_shot_agent_names() -> BTreeSet<&'static str> {
    AGENT_INTERACTION_POLICIES
        .iter()
        .filter(|(_, policy)| policy.one_shot)
        .map(|(name, _)| *name)
        .collect()
}

pub fn interaction_policy_for_agent(agent_name: &str) -> Option<AgentInteractionPolicy> {
    AGENT_INTERACTION_POLICIES
        .iter()
        .find(|(name, _)| *name == agent_name)
        .map(|(_, policy)| *policy)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentInvocationCondition {
    pub requires_skills: &'static [&'static str],
    pub requires_plan_artifact: bool,
    pub forbids_skills: &'static [&'static str],
}

const PLAN_GATE: AgentInvocationCondition = AgentInvocationCondition {
    requires_skills: &["ulw-plan"],
    requires_plan_artifact: true,
    forbids_skills: &["start-work"],
};

pub const AGENT_INVOCATION_CONDITIONS: [(&str, AgentInvocationCondition); 2] =
    [("metis", PLAN_GATE), ("momus", PLAN_GATE)];

pub fn plan_gated_agent_names() -> BTreeSet<&'static str> {
    AGENT_INVOCATION_CONDITIONS
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

/// One plan-artifact path touched in a session; `last_touched_at` is a monotonic per-tracker
/// sequence number, not a wall-clock time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanArtifactReference {
    pub path: String,
    pub count: u64,
    pub last_touched_at: u64,
}

/// Session-scoped skill-invocation facts supplied by the host adapter.
///
/// `has_invoked` observes every channel (SKILL.md read, slash command) and feeds the forbids
/// check; `has_user_requested` is strictly the user-input channel and is the only one that
/// satisfies `requires_skills`; `plan_artifact_references` is sorted by count desc then
/// `last_touched_at` desc.
pub trait SkillInvocationState {
    fn has_invoked(&self, skill: &str) -> bool;
    fn has_user_requested(&self, skill: &str) -> bool;
    fn has_plan_artifact(&self) -> bool;
    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference>;
}

/// The state of a session with no invocations and no plan artifacts.
pub struct EmptySkillInvocations;

impl SkillInvocationState for EmptySkillInvocations {
    fn has_invoked(&self, _skill: &str) -> bool {
        false
    }

    fn has_user_requested(&self, _skill: &str) -> bool {
        false
    }

    fn has_plan_artifact(&self) -> bool {
        false
    }

    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationGuardVerdict {
    Allow,
    Deny { message: String },
}

pub fn invocation_condition_for_agent(agent_name: &str) -> Option<AgentInvocationCondition> {
    AGENT_INVOCATION_CONDITIONS
        .iter()
        .find(|(name, _)| *name == agent_name)
        .map(|(_, condition)| *condition)
}

/// Forbidden skills win over missing requirements; the requirement is satisfied only by a USER
/// request, and every unlock route named in a denial is one only a human can take.
pub fn evaluate_invocation_guard(
    agent_name: &str,
    state: &dyn SkillInvocationState,
) -> InvocationGuardVerdict {
    let Some(condition) = invocation_condition_for_agent(agent_name) else {
        return InvocationGuardVerdict::Allow;
    };

    if let Some(violated) = condition
        .forbids_skills
        .iter()
        .find(|skill| state.has_invoked(skill))
    {
        return InvocationGuardVerdict::Deny {
            message: format!(
                "Agent \"{agent_name}\" is plan-gated and cannot be spawned: the {violated} skill was already invoked in this session, \
                 so plan review is no longer available. Continue the work directly or choose another agent."
            ),
        };
    }

    let missing: Vec<&str> = condition
        .requires_skills
        .iter()
        .copied()
        .filter(|skill| !state.has_user_requested(skill))
        .collect();
    if !missing.is_empty() {
        let names = missing.join(", ");
        return InvocationGuardVerdict::Deny {
            message: format!(
                "Agent \"{agent_name}\" is plan-gated: it is available only after the user explicitly requests the {names} \
                 workflow in this session, and no such request was made. Do not attempt to unlock this gate yourself - retrying \
                 this spawn will keep failing. Continue without plan review (self-review instead), or ask the user to start the \
                 workflow themselves by running /skill:{names} or by asking for a plan in their own words."
            ),
        };
    }

    if condition.requires_plan_artifact && !state.has_plan_artifact() {
        let skills = condition.requires_skills.join(", ");
        return InvocationGuardVerdict::Deny {
            message: format!(
                "Agent \"{agent_name}\" is plan-gated: no plan artifact (.omo/plans/*.md) was touched in this session, so there is no \
                 plan to review yet. Complete the {skills} workflow so it produces a plan file, then retry."
            ),
        };
    }

    InvocationGuardVerdict::Allow
}
