use crate::bridges::schema_bridge::EvalSchemaToolInfo;
use maho_tools::definition::ToolExposure;
use std::collections::HashSet;

pub trait ReachableToolRegistry {
    fn get_active_tools(&self) -> Result<Vec<String>, String>;
    fn get_all_tools(&self) -> Result<Vec<EvalSchemaToolInfo>, String>;
}

impl ReachableToolRegistry for maho_ext_api::ExtensionApi {
    fn get_active_tools(&self) -> Result<Vec<String>, String> {
        self.get_active_tools().map_err(|error| error.to_string())
    }

    fn get_all_tools(&self) -> Result<Vec<EvalSchemaToolInfo>, String> {
        self.get_all_tools()
            .map(|tools| tools.into_iter().map(EvalSchemaToolInfo::from).collect())
            .map_err(|error| error.to_string())
    }
}

/// The tools a cell can call: the model-visible active tools plus every registered tool whose
/// declared exposure is `eval`. Propagates `get_active_tools` failure when unbound while ignoring
/// `get_all_tools` errors (`reachable-tools.ts`).
pub fn try_reachable_tool_names(
    registry: &dyn ReachableToolRegistry,
) -> Result<HashSet<String>, String> {
    let mut names: HashSet<String> = registry.get_active_tools()?.into_iter().collect();
    for tool in registered_tools(registry) {
        if tool.exposure == Some(ToolExposure::Eval) {
            names.insert(tool.name);
        }
    }
    Ok(names)
}

/// The tools a cell can call: the model-visible active tools plus every registered tool whose
/// declared exposure is `eval`.
pub fn reachable_tool_names(registry: &dyn ReachableToolRegistry) -> HashSet<String> {
    try_reachable_tool_names(registry).unwrap_or_else(|_| {
        registered_tools(registry)
            .into_iter()
            .filter(|tool| tool.exposure == Some(ToolExposure::Eval))
            .map(|tool| tool.name)
            .collect()
    })
}

/// The tools the session withholds from the model and routes through eval only, in registration
/// order (`senpi#6886`).
pub fn eval_only_tool_names(registry: &dyn ReachableToolRegistry) -> Vec<String> {
    let active: HashSet<String> = registry
        .get_active_tools()
        .unwrap_or_default()
        .into_iter()
        .collect();
    registered_tools(registry)
        .into_iter()
        .filter(|tool| match tool.eval_only {
            Some(flag) => flag,
            None => tool.exposure == Some(ToolExposure::Eval) && !active.contains(&tool.name),
        })
        .map(|tool| tool.name)
        .collect()
}

/// An unreadable registry adds nothing, like `active_kernel_preludes`.
fn registered_tools(registry: &dyn ReachableToolRegistry) -> Vec<EvalSchemaToolInfo> {
    registry.get_all_tools().unwrap_or_default()
}
