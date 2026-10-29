//! Nesting depth admission (`manager/depth-policy.ts`).

pub struct DepthPolicyInput<'a> {
    pub child_depth: u32,
    pub max_depth: u32,
    pub target_agent_type: Option<&'a str>,
    pub allowed_subagents: Option<&'a [String]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthDecision {
    WithinDepth,
    AllowedSubagent,
    Denied { reason: String },
}

impl DepthDecision {
    pub fn allowed(&self) -> bool {
        !matches!(self, Self::Denied { .. })
    }
}

pub fn decide_depth_policy(input: &DepthPolicyInput<'_>) -> DepthDecision {
    if let (Some(target), Some(allowed)) = (input.target_agent_type, input.allowed_subagents)
        && allowed.iter().any(|agent| agent == target)
    {
        return DepthDecision::AllowedSubagent;
    }
    if input.child_depth <= input.max_depth {
        return DepthDecision::WithinDepth;
    }
    DepthDecision::Denied {
        reason: format!(
            "Task nesting depth {} exceeds maxDepth {}.",
            input.child_depth, input.max_depth
        ),
    }
}
