//! Task agent definitions, loading, policies and resolution (`agents/*`).

mod builtin;
mod loader;
mod policy;
mod resolve;
mod schema;
mod tools;
mod types;

pub use builtin::{
    AGENT_FALLBACK_CHAINS, BUILTIN_AGENT_DEFAULTS, agent_fallback_chain, builtin_agent,
    curated_readonly_agent_names,
};
pub use loader::{
    AgentRegistry, load_agents, load_agents_with_registry, map_omo_config_agents, register_agent,
};
pub use policy::{
    AGENT_INTERACTION_POLICIES, AGENT_INVOCATION_CONDITIONS, AgentInteractionPolicy,
    AgentInvocationCondition, EmptySkillInvocations, InvocationGuardVerdict, PlanArtifactReference,
    SkillInvocationState, evaluate_invocation_guard, interaction_policy_for_agent,
    invocation_condition_for_agent, one_shot_agent_names, plan_gated_agent_names,
};
pub use resolve::{
    AgentExecutionMode, AgentPersona, AgentResolutionResult, ResolveAgentOptions, ResolvedAgent,
    resolve_agent,
};
pub use tools::{normalize_tool_rules, resolve_tool_rule};
pub use types::{
    AgentDefinition, AgentLoaderDiagnostic, AgentLoaderDiagnosticKind, AgentModelCandidate,
    AgentModelEntry, AgentToolRule, LoadAgentsOptions, LoadAgentsResult, agent_model_candidates,
};

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
