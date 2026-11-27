//! Port of senpi `packages/coding-agent/src/core/tool-info.ts`.

use std::collections::{HashMap, HashSet};

use maho_ext_api::{SourceInfo, ToolDefinition, ToolExposure, ToolInfo};

pub fn tool_infos_of(
    definitions: &HashMap<String, (ToolDefinition, SourceInfo)>,
    eval_only: &HashSet<String>,
) -> Vec<ToolInfo> {
    definitions
        .values()
        .map(|(definition, source_info)| {
            let mut info = tool_info_of(definition, source_info);
            if eval_only.contains(&info.name) {
                info.exposure = ToolExposure::Eval;
            }
            info
        })
        .collect()
}

pub fn tool_info_of(definition: &ToolDefinition, source_info: &SourceInfo) -> ToolInfo {
    ToolInfo {
        name: definition.name.clone(),
        label: definition.label.clone(),
        description: definition.description.clone(),
        parameters: definition.parameters.clone(),
        prompt_guidelines: definition.prompt_guidelines.clone(),
        source_info: source_info.clone(),
        exposure: definition.exposure.unwrap_or(ToolExposure::Direct),
        search_text: definition.search_text.clone(),
        search_keywords: definition.search_keywords.clone().unwrap_or_default(),
        search_group: definition.search_group.clone(),
        allow_lazy_activation: definition.allow_lazy_activation.unwrap_or(false),
        kernel_prelude: definition.kernel_prelude.clone(),
    }
}
