//! Port of senpi `packages/coding-agent/src/core/runtime/active-tools.ts`.

use std::collections::{HashMap, HashSet};

use maho_ext_api::{ToolDefinition, ToolExposure};

const DEFAULT_TOOL_NAMES: [&str; 4] = ["read", "bash", "edit", "write"];
const EVAL_ONLY_TOOL_NAMES: &[&str] = &["workflow", "monitor"];
const EVAL_TOOL_NAME: &str = "eval";

pub fn tool_exposure_of(definitions: &HashMap<String, ToolDefinition>, name: &str) -> ToolExposure {
    definitions
        .get(name)
        .and_then(|d| d.exposure)
        .unwrap_or(ToolExposure::Direct)
}

pub fn is_activated_on_registration(
    definitions: &HashMap<String, ToolDefinition>,
    name: &str,
) -> bool {
    let Some(definition) = definitions.get(name) else {
        return false;
    };
    matches!(
        definition.exposure.unwrap_or(ToolExposure::Direct),
        ToolExposure::Direct | ToolExposure::Eval
    )
}

pub fn resolve_eval_only_tool_names(
    definitions: &HashMap<String, ToolDefinition>,
    override_set: Option<&HashSet<String>>,
) -> HashSet<String> {
    if let Some(override_set) = override_set {
        return override_set.clone();
    }
    let mut names: HashSet<String> = EVAL_ONLY_TOOL_NAMES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    for def in definitions.values() {
        if def.exposure == Some(ToolExposure::Eval) {
            names.insert(def.name.clone());
        }
    }
    names
}

pub fn armed_eval_only_tool_names(
    eval_only: Option<&HashSet<String>>,
    registered_tools: &HashSet<String>,
) -> Option<HashSet<String>> {
    let eval_only = eval_only?;
    registered_tools
        .contains(EVAL_TOOL_NAME)
        .then(|| eval_only.clone())
}

pub fn registered_eval_only_tool_names(
    armed_eval_only: Option<&HashSet<String>>,
    registered_tools: &HashSet<String>,
) -> HashSet<String> {
    let Some(armed) = armed_eval_only else {
        return HashSet::new();
    };
    armed
        .iter()
        .filter(|name| registered_tools.contains(*name))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ToolSelection {
    pub tools: Option<Vec<String>>,
    pub no_all_tools: bool,
    pub has_no_tools: bool,
    pub exclude_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedToolSelection {
    pub default_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub excluded_tool_names: Option<Vec<String>>,
    pub initial_active_tool_names: Vec<String>,
}

pub fn resolve_tool_selection(
    selection: &ToolSelection,
    configured_default_tool_names: Option<&[String]>,
) -> ResolvedToolSelection {
    let excluded: HashSet<&str> = selection
        .exclude_tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(String::as_str)
        .collect();
    let default_tool_names = if selection.tools.is_none() && !selection.has_no_tools {
        configured_default_tool_names.map(|list| list.to_vec())
    } else {
        None
    };
    let allowed_tool_names = if let Some(tools) = &selection.tools {
        Some(tools.clone())
    } else if selection.no_all_tools {
        Some(Vec::new())
    } else {
        None
    };
    let base_initial: Vec<String> = if let Some(tools) = &selection.tools {
        tools.clone()
    } else if selection.has_no_tools {
        Vec::new()
    } else if let Some(configured) = configured_default_tool_names {
        configured.to_vec()
    } else {
        DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect()
    };
    let initial_active_tool_names = base_initial
        .into_iter()
        .filter(|name| !excluded.contains(name.as_str()))
        .collect();
    ResolvedToolSelection {
        default_tool_names,
        allowed_tool_names,
        excluded_tool_names: selection.exclude_tools.clone(),
        initial_active_tool_names,
    }
}

#[derive(Debug, Clone)]
pub struct DirectActiveTools {
    pub active: Vec<String>,
    pub withheld: Vec<String>,
}

pub fn direct_active_tool_names(
    requested: &[String],
    _definitions: &HashMap<String, ToolDefinition>,
    registered: &HashSet<String>,
    armed_eval_only: Option<&HashSet<String>>,
) -> DirectActiveTools {
    let active = requested
        .iter()
        .filter(|name| {
            !armed_eval_only.is_some_and(|set| set.contains(*name)) && registered.contains(*name)
        })
        .cloned()
        .collect();
    let withheld = match armed_eval_only {
        Some(armed) => requested
            .iter()
            .filter(|name| armed.contains(*name))
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    DirectActiveTools { active, withheld }
}
