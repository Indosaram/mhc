use std::sync::Arc;
use serde_json::Value;
use senpi_task::{agents::{AgentDefinition, AgentResolutionResult, ResolveAgentOptions, resolve_agent, AgentExecutionMode}, category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category}, host::{SenpiModelRegistry, parse_model}, manager::{execution_mode::ExecutionMode, types::{ChildPlanner, PlanResolutionCode, PlanResolutionError, ResolvedChildPlan}}, state::{ResolvedModelRecord, ResolvedModelSource}};

pub type ResolveModelRegistry = Arc<dyn Fn() -> Option<Arc<dyn SenpiModelRegistry>> + Send + Sync>;
const NO_REGISTRY_MESSAGE: &str = "No senpi model registry is available yet to resolve a task model.";
fn explicit_model_metadata(model: &str) -> Option<ResolvedModelRecord> {
    let parsed = parse_model(model)?;
    Some(ResolvedModelRecord::new(ResolvedModelSource::Explicit, &parsed.provider, &parsed.model_id))
}
pub fn create_task_child_planner(config: Value, agents: Vec<(String, AgentDefinition)>, resolve_registry: ResolveModelRegistry) -> ChildPlanner {
    Arc::new(move |spec| {
        let mut available_agents: Vec<_> = agents.iter().filter(|(_, definition)| definition.disable != Some(true)).map(|(name, _)| name.clone()).collect(); available_agents.sort();
        if let Some(name) = &spec.subagent_type {
            let explicit = spec.model.as_deref().filter(|model| !model.is_empty());
            if agents.iter().any(|(agent, definition)| agent == name && definition.disable == Some(true)) && explicit.is_some() {
                let mut error = PlanResolutionError::new(PlanResolutionCode::UnknownTarget, format!("Target \"{name}\" not found.")); error.available_agents = Some(available_agents.clone()); return Err(Box::new(error));
            }
            let registry = if explicit.is_some() { None } else { resolve_registry() };
            match resolve_agent(name, &agents, registry.as_deref(), &ResolveAgentOptions { model_override: explicit.map(str::to_owned) }) {
                AgentResolutionResult::Resolved(resolution) => {
                    let variant = resolution.resolved_model.as_ref().and_then(|model| model.reasoning.clone().or(model.reasoning_effort.clone()).or(model.variant.clone()));
                    let resolved_model = resolution.resolved_model.clone().or_else(|| explicit.and_then(explicit_model_metadata));
                    return Ok(ResolvedChildPlan { model: resolution.model, requested_model: resolution.requested_model, fallback_models: resolution.fallback_models, resolved_model, variant, agent_type: Some(resolution.persona.agent_type), instructions: resolution.persona.instructions, tool_allowlist: resolution.persona.tool_allowlist, agent_execution_mode: resolution.persona.agent_execution_mode.map(|mode| match mode { AgentExecutionMode::InProcess => ExecutionMode::InProcess, AgentExecutionMode::Process => ExecutionMode::Process }), allowed_subagents: resolution.persona.allowed_subagents, max_depth: resolution.persona.max_depth.and_then(|depth| u32::try_from(depth).ok()), ..ResolvedChildPlan::default() });
                }
                AgentResolutionResult::ModelUnavailable { attempted_model, available_agents, .. } => {
                    let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, if registry.is_none() { NO_REGISTRY_MESSAGE.into() } else { format!("No available model for agent \"{name}\" (attempted {}).", attempted_model.as_deref().unwrap_or("none")) });
                    if registry.is_some() { error.available_agents = Some(available_agents); } return Err(Box::new(error));
                }
                AgentResolutionResult::NotFound { .. } => {}
            }
        }
        if let Some(model) = spec.model.as_deref().filter(|model| !model.is_empty()) { return Ok(ResolvedChildPlan { model: model.into(), resolved_model: explicit_model_metadata(model), ..ResolvedChildPlan::default() }); }
        let Some(category) = spec.category.as_deref().or(spec.subagent_type.as_deref()) else { return Err(Box::new(PlanResolutionError::new(PlanResolutionCode::InvalidTarget, "A task requires a category, subagent_type, or model."))) };
        let Some(registry) = resolve_registry() else { return Err(Box::new(PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, NO_REGISTRY_MESSAGE))) };
        let resolution = resolve_category(category, &config, registry.as_ref(), &ResolveCategoryOptions::default()).map_err(|error| Box::new(PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, error.to_string())))?;
        match resolution {
            CategoryResolutionResult::Resolved { category, spec, .. } => {
                let variant = spec.reasoning.clone().or(spec.reasoning_effort.clone()).or(spec.variant.clone());
                let mut resolved = ResolvedModelRecord::new(ResolvedModelSource::Category, &spec.provider, &spec.model_id); resolved.display = spec.display_name.unwrap_or_else(|| format!("{}/{}", spec.provider, spec.model_id)); resolved.variant = spec.variant; resolved.reasoning_effort = spec.reasoning_effort; resolved.reasoning = spec.reasoning;
                Ok(ResolvedChildPlan { model: format!("{}/{}", spec.provider, spec.model_id), requested_model: spec.requested_model, fallback_models: spec.fallback_models, resolved_model: Some(resolved), variant, category: Some(category), prompt_append: spec.prompt_append, ..ResolvedChildPlan::default() })
            }
            CategoryResolutionResult::Disabled { reason, available_categories, .. } => { let mut error = PlanResolutionError::new(PlanResolutionCode::CategoryDisabled, reason); error.available_categories = Some(available_categories); Err(Box::new(error)) }
            CategoryResolutionResult::NotFound { available_categories, .. } => { let mut error = PlanResolutionError::new(PlanResolutionCode::UnknownTarget, format!("Category \"{category}\" not found.")); error.available_agents = Some(available_agents); error.available_categories = Some(available_categories); Err(Box::new(error)) }
            CategoryResolutionResult::ModelUnavailable(unavailable) => { let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, format!("No available model for category \"{category}\" (attempted {}).", unavailable.attempted_model.as_deref().unwrap_or("none"))); error.available_categories = Some(unavailable.available_categories); error.category = Some(category.into()); error.attempted_chain = unavailable.attempted_chain; error.missing_providers = unavailable.missing_providers; Err(Box::new(error)) }
        }
    })
}
