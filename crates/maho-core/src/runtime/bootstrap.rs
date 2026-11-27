//! Port of senpi `packages/coding-agent/src/core/runtime/bootstrap.ts`.

use std::collections::{BTreeMap, HashMap};

use maho_agent::types::AgentTool;
use maho_ext_api::{SourceInfo, ToolDefinition};

#[derive(Clone)]
pub struct ToolDefinitionEntry {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

#[derive(Default)]
pub struct ToolRegistryBuild {
    pub definitions: HashMap<String, ToolDefinitionEntry>,
    pub tool_registry: BTreeMap<String, AgentTool>,
    pub extension_tools: Vec<ToolDefinition>,
}

impl ToolRegistryBuild {
    pub fn is_allowed_tool(&self, name: &str) -> bool {
        self.tool_registry.contains_key(name)
    }
}
