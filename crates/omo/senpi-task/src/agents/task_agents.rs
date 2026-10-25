//! The canonical merged task-agent set (`agents/task-agents.ts`).

use serde_json::Value;

use super::builtin::{
    BUILTIN_AGENT_DEFAULTS, curated_readonly_agent_names, ulw_reviewer_agent_names,
};
use super::loader::map_omo_config_agents;
use super::types::AgentDefinition;

/// The agents a task spawn can address: the builtins overlaid with omo.json `agents`, the curated
/// read-only agents and the ulw reviewers pinned in-process. `omo doctor --models` reads the same set.
pub fn resolve_task_agents(config: &Value) -> Vec<(String, AgentDefinition)> {
    let mut merged: Vec<(String, AgentDefinition)> = BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| (definition.name.clone(), definition.clone()))
        .collect();
    for (name, definition) in map_omo_config_agents(config) {
        match merged
            .iter()
            .position(|(existing, _)| existing == &name)
        {
            Some(index) => {
                let base = merged[index].1.clone();
                merged[index].1 = base.overlaid_with(definition);
            }
            None => merged.push((name, definition)),
        }
    }
    for name in curated_readonly_agent_names()
        .into_iter()
        .chain(ulw_reviewer_agent_names())
    {
        if let Some((_, definition)) = merged
            .iter_mut()
            .find(|(existing, _)| existing.as_str() == name)
        {
            definition.execution_mode = Some("in-process".to_string());
        }
    }
    merged
}
