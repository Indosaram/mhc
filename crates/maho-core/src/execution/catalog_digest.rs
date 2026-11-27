//! Port of senpi packages/coding-agent/src/core/execution/catalog-digest.ts.

use maho_agent::canonical_json;
use maho_agent::types::{AgentTool, ToolExecutionMode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub output_schema: Option<Value>,
    pub execution_mode: Option<ToolExecutionMode>,
}

impl From<&AgentTool> for CatalogTool {
    fn from(tool: &AgentTool) -> Self {
        Self {
            name: tool.tool.name.clone(),
            description: tool.tool.description.clone(),
            parameters: tool.tool.parameters.clone(),
            output_schema: tool.tool.output_schema.clone(),
            execution_mode: tool.execution_mode,
        }
    }
}

/// 32-byte SHA-256 hex digest of one tool's schema in the executor's canonical JSON form.
pub fn tool_schema_digest(tool: &CatalogTool) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("name".to_owned(), json!(tool.name));
    obj.insert("description".to_owned(), json!(tool.description));
    obj.insert("parameters".to_owned(), tool.parameters.clone());
    if let Some(output_schema) = &tool.output_schema {
        obj.insert("outputSchema".to_owned(), output_schema.clone());
    }
    if let Some(mode) = tool.execution_mode {
        obj.insert("executionMode".to_owned(), json!(mode));
    }
    let canonical = canonical_json(&Value::Object(obj));
    hex::encode(Sha256::digest(canonical.as_bytes()))
}
