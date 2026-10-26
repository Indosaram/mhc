use std::sync::Arc;
use serde_json::Value;
use senpi_task::{agents::{AgentDefinition, AgentResolutionResult, ResolveAgentOptions, resolve_agent, AgentExecutionMode}, category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category}, host::SenpiModelRegistry, manager::{execution_mode::ExecutionMode, types::{ChildPlanner, PlanResolutionCode, PlanResolutionError, ResolvedChildPlan}}, model_exclusion::is_excluded_model, state::{ResolvedModelRecord, ResolvedModelSource}};
use crate::fast_mode_inheritance::{ResolveParentServiceTier, inherit_parent_fast_mode};
use crate::planner_explicit_pin::{ExplicitPinResolution, ResolveDefaultRoute, resolve_explicit_pin};

pub type ResolveModelRegistry = Arc<dyn Fn() -> Option<Arc<dyn SenpiModelRegistry>> + Send + Sync>;
const NO_REGISTRY_MESSAGE: &str = "No senpi model registry is available yet to resolve a task model.";

/// The explicit-model metadata a pin-less fallback would otherwise build by hand.
fn explicit_model_metadata(model: &str) -> Option<ResolvedModelRecord> {
    let parsed = senpi_task::host::parse_model(model)?;
    Some(ResolvedModelRecord::new(ResolvedModelSource::Explicit, &parsed.provider, &parsed.model_id))
}

/// `omoConfig.exclude_models`: the patterns every resolution path honors.
fn exclude_models(config: &Value) -> Vec<String> {
    config.get("exclude_models").and_then(Value::as_array).map(|patterns| patterns.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

/// The `model_unavailable` a refused pin becomes, with the agent's available-target hint attached.
fn pin_failure(error: crate::planner_explicit_pin::ExplicitPinError, available_agents: &[String]) -> PlanResolutionError {
    let mut failure = PlanResolutionError::new(match error.code {
        crate::planner_explicit_pin::ExplicitPinCode::InvalidTarget => PlanResolutionCode::InvalidTarget,
        crate::planner_explicit_pin::ExplicitPinCode::ModelUnavailable => PlanResolutionCode::ModelUnavailable,
    }, error.message);
    failure.available_agents = Some(available_agents.to_vec());
    failure
}

/// `createTaskChildPlanner`: category/agent resolution, explicit-pin exclusion, then fast-mode
/// inheritance. A `-fast` sibling that `exclude_models` keeps out retains the base plan.
pub fn create_task_child_planner(config: Value, agents: Vec<(String, AgentDefinition)>, resolve_registry: ResolveModelRegistry) -> ChildPlanner {
    create_task_child_planner_with_tiers(config, agents, resolve_registry, Arc::new(|| None), None)
}

pub fn create_task_child_planner_with_tiers(
    config: Value,
    agents: Vec<(String, AgentDefinition)>,
    resolve_registry: ResolveModelRegistry,
    resolve_parent_service_tier: ResolveParentServiceTier,
    resolve_default_route: Option<ResolveDefaultRoute>,
) -> ChildPlanner {
    let excluded = exclude_models(&config);
    // Whatever path resolved, the plan then inherits the parent's effective execution tier, and an
    // excluded `-fast` sibling keeps the base plan instead of failing the spawn.
    let finish: Arc<dyn Fn(ResolvedChildPlan) -> ResolvedChildPlan + Send + Sync> = {
        let resolve_registry = resolve_registry.clone();
        let resolve_parent_service_tier = resolve_parent_service_tier.clone();
        let excluded = excluded.clone();
        Arc::new(move |plan: ResolvedChildPlan| {
            let live = resolve_registry();
            let fast = inherit_parent_fast_mode(plan.clone(), live.as_deref(), resolve_parent_service_tier().as_deref());
            if is_excluded_model(&fast.model, &excluded) { plan } else { fast }
        })
    };
    Arc::new(move |spec| {
        let finish = finish.clone();
        let mut available_agents: Vec<_> = agents.iter().filter(|(_, definition)| definition.disable != Some(true)).map(|(name, _)| name.clone()).collect(); available_agents.sort();
        if let Some(name) = &spec.subagent_type {
            let explicit = spec.model.as_deref().filter(|model| !model.is_empty());
            if agents.iter().any(|(agent, definition)| agent == name && definition.disable == Some(true)) && explicit.is_some() {
                let mut error = PlanResolutionError::new(PlanResolutionCode::UnknownTarget, format!("Target \"{name}\" not found.")); error.available_agents = Some(available_agents.clone()); return Err(Box::new(error));
            }
            if let Some(pin) = explicit {
                // An explicit override is parsed ONCE against the live registry and refused typed.
                let live = resolve_registry();
                match resolve_explicit_pin(pin, live.as_deref(), resolve_default_route.as_ref(), Some(&excluded)) {
                    ExplicitPinResolution::Resolved { canonical, metadata, thinking_level } => {
                        match resolve_agent(name, &agents, None, &ResolveAgentOptions { model_override: Some(pin.to_string()), ..ResolveAgentOptions::default() }) {
                            AgentResolutionResult::Resolved(resolution) => {
                                let variant = thinking_level.clone().or_else(|| metadata.reasoning.clone());
                                return Ok(ResolvedChildPlan { model: canonical, requested_model: resolution.requested_model, fallback_models: resolution.fallback_models, resolved_model: Some(metadata), variant, agent_type: Some(resolution.persona.agent_type), instructions: resolution.persona.instructions, tool_allowlist: resolution.persona.tool_allowlist, agent_execution_mode: resolution.persona.agent_execution_mode.map(|mode| match mode { AgentExecutionMode::InProcess => ExecutionMode::InProcess, AgentExecutionMode::Process => ExecutionMode::Process }), allowed_subagents: resolution.persona.allowed_subagents, max_depth: resolution.persona.max_depth.and_then(|depth| u32::try_from(depth).ok()), ..ResolvedChildPlan::default() });
                            }
                            AgentResolutionResult::ModelUnavailable { .. } | AgentResolutionResult::NotFound { .. } => {}
                        }
                    }
                    ExplicitPinResolution::Error(error) => return Err(Box::new(pin_failure(error, &available_agents))),
                }
            }
            let registry = if explicit.is_some() { None } else { resolve_registry() };
            match resolve_agent(name, &agents, registry.as_deref(), &ResolveAgentOptions { omo_config: Some(config.clone()), model_override: explicit.map(str::to_owned), ..ResolveAgentOptions::default() }) {
                AgentResolutionResult::Resolved(resolution) => {
                    let variant = resolution.resolved_model.as_ref().and_then(|model| model.reasoning.clone().or(model.reasoning_effort.clone()).or(model.variant.clone()));
                    let resolved_model = resolution.resolved_model.clone().or_else(|| explicit.and_then(explicit_model_metadata));
                    return Ok(finish(ResolvedChildPlan { model: resolution.model, requested_model: resolution.requested_model, fallback_models: resolution.fallback_models, resolved_model, variant, agent_type: Some(resolution.persona.agent_type), instructions: resolution.persona.instructions, tool_allowlist: resolution.persona.tool_allowlist, agent_execution_mode: resolution.persona.agent_execution_mode.map(|mode| match mode { AgentExecutionMode::InProcess => ExecutionMode::InProcess, AgentExecutionMode::Process => ExecutionMode::Process }), allowed_subagents: resolution.persona.allowed_subagents, max_depth: resolution.persona.max_depth.and_then(|depth| u32::try_from(depth).ok()), ..ResolvedChildPlan::default() }));
                }
                AgentResolutionResult::ModelUnavailable { attempted_model, available_agents, .. } => {
                    let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, if registry.is_none() { NO_REGISTRY_MESSAGE.into() } else { format!("No available model for agent \"{name}\" (attempted {}).", attempted_model.as_deref().unwrap_or("none")) });
                    if registry.is_some() { error.available_agents = Some(available_agents); } return Err(Box::new(error));
                }
                AgentResolutionResult::NotFound { .. } => {}
            }
        }
        if let Some(pin) = spec.model.as_deref().filter(|model| !model.is_empty()) {
            let live = resolve_registry();
            return match resolve_explicit_pin(pin, live.as_deref(), resolve_default_route.as_ref(), Some(&excluded)) {
                ExplicitPinResolution::Resolved { canonical, metadata, thinking_level } => Ok(ResolvedChildPlan { model: canonical, resolved_model: Some(metadata), variant: thinking_level, ..ResolvedChildPlan::default() }),
                ExplicitPinResolution::Error(error) => Err(Box::new(pin_failure(error, &available_agents))),
            };
        }
        let Some(category) = spec.category.as_deref().or(spec.subagent_type.as_deref()) else { return Err(Box::new(PlanResolutionError::new(PlanResolutionCode::InvalidTarget, "A task requires a category, subagent_type, or model."))) };
        let Some(registry) = resolve_registry() else { return Err(Box::new(PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, NO_REGISTRY_MESSAGE))) };
        let resolution = resolve_category(category, &config, registry.as_ref(), &ResolveCategoryOptions::default()).map_err(|error| Box::new(PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, error.to_string())))?;
        match resolution {
            CategoryResolutionResult::Resolved { category, spec, .. } => {
                let variant = spec.reasoning.clone().or(spec.reasoning_effort.clone()).or(spec.variant.clone());
                let mut resolved = ResolvedModelRecord::new(ResolvedModelSource::Category, &spec.provider, &spec.model_id); resolved.display = spec.display_name.unwrap_or_else(|| format!("{}/{}", spec.provider, spec.model_id)); resolved.variant = spec.variant; resolved.reasoning_effort = spec.reasoning_effort; resolved.reasoning = spec.reasoning;
                Ok(finish(ResolvedChildPlan { model: format!("{}/{}", spec.provider, spec.model_id), requested_model: spec.requested_model, fallback_models: spec.fallback_models, resolved_model: Some(resolved), variant, category: Some(category), prompt_append: spec.prompt_append, ..ResolvedChildPlan::default() }))
            }
            CategoryResolutionResult::Disabled { reason, available_categories, .. } => { let mut error = PlanResolutionError::new(PlanResolutionCode::CategoryDisabled, reason); error.available_categories = Some(available_categories); Err(Box::new(error)) }
            CategoryResolutionResult::NotFound { available_categories, .. } => { let mut error = PlanResolutionError::new(PlanResolutionCode::UnknownTarget, format!("Category \"{category}\" not found.")); error.available_agents = Some(available_agents); error.available_categories = Some(available_categories); Err(Box::new(error)) }
            CategoryResolutionResult::ModelUnavailable(unavailable) => { let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, format!("No available model for category \"{category}\" (attempted {}).", unavailable.attempted_model.as_deref().unwrap_or("none"))); error.available_categories = Some(unavailable.available_categories); error.category = Some(category.into()); error.attempted_chain = unavailable.attempted_chain; error.missing_providers = unavailable.missing_providers; Err(Box::new(error)) }
        }
    })
}
