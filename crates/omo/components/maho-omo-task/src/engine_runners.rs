use std::collections::BTreeMap;
use serde_json::Value;
use senpi_task::agents::{AgentDefinition, BUILTIN_AGENT_DEFAULTS, curated_readonly_agent_names, map_omo_config_agents};

pub const TASK_CHILD_UI_ONLY_TOOL_NAMES: [&str; 2] = ["memory", "memory_apply_patch"];
pub struct InProcessRunnerBuildContext {
    pub shared_parent_tools:Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef>,
    pub max_depth:u32,
    pub create_session:senpi_task::runners::in_process::runner::CreateChildSession,
    pub parent_registry:senpi_task::manager::parent_registry_context::ParentModelRegistryResolver,
}
pub fn build_in_process_runner(build:InProcessRunnerBuildContext)->std::sync::Arc<dyn senpi_task::manager::types::ManagedRunner> {
    let runner=senpi_task::runners::in_process::InProcessRunner::new(senpi_task::runners::in_process::InProcessRunnerOptions {
        shared_parent_tools:build.shared_parent_tools,ui_only_tool_names:TASK_CHILD_UI_ONLY_TOOL_NAMES.iter().map(|name| (*name).into()).collect(),max_depth:Some(build.max_depth.saturating_add(1).max(1)),create_session:build.create_session,
    });
    senpi_task::manager::runner::create_in_process_managed_runner(runner,senpi_task::manager::parent_registry_context::create_parent_registry_session_context(build.parent_registry))
}
pub fn resolve_task_agents(config: &Value) -> BTreeMap<String, AgentDefinition> {
    let mut merged: BTreeMap<_, _> = BUILTIN_AGENT_DEFAULTS.iter().map(|definition| (definition.name.clone(), definition.clone())).collect();
    for (name, overlay) in map_omo_config_agents(config) {
        let definition = merged.entry(name).or_insert_with(|| AgentDefinition::named(&overlay.name));
        macro_rules! overlay_fields { ($($field:ident),* $(,)?) => { $(if overlay.$field.is_some() { definition.$field = overlay.$field; })* }; }
        overlay_fields!(description, prompt, mode, model, models, variant, reasoning_effort, temperature, tools, disable, background, execution_mode, allowed_subagents, disallowed_tools, max_depth, max_turns);
    }
    for name in curated_readonly_agent_names() { if let Some(definition) = merged.get_mut(name) { definition.execution_mode = Some("in-process".into()); } }
    merged
}
