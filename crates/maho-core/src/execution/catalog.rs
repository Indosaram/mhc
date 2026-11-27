//! Port of senpi packages/coding-agent/src/core/execution/catalog.ts.

use std::collections::HashMap;

use maho_agent::types::ToolExecutionMode;
use serde_json::Value;

use super::catalog_digest::{CatalogTool, tool_schema_digest};

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCatalogEntry {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub output_schema: Option<Value>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub schema_digest: String,
}

pub type ToolCatalog = HashMap<String, ToolCatalogEntry>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogCall {
    pub name: String,
    pub schema_digest: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogFailureReason {
    ToolUnknown,
    SchemaStale,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CatalogCallCheck {
    Ok(ToolCatalogEntry),
    Err {
        reason: CatalogFailureReason,
        name: String,
    },
}

pub fn build_tool_catalog<'a>(tools: impl IntoIterator<Item = &'a CatalogTool>) -> ToolCatalog {
    let mut catalog = HashMap::new();
    for tool in tools {
        catalog.insert(
            tool.name.clone(),
            ToolCatalogEntry {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
                output_schema: tool.output_schema.clone(),
                execution_mode: tool.execution_mode,
                schema_digest: tool_schema_digest(tool),
            },
        );
    }
    catalog
}

pub fn check_catalog_call(catalog: &ToolCatalog, call: &CatalogCall) -> CatalogCallCheck {
    let Some(entry) = catalog.get(&call.name) else {
        return CatalogCallCheck::Err {
            reason: CatalogFailureReason::ToolUnknown,
            name: call.name.clone(),
        };
    };
    if entry.schema_digest != call.schema_digest {
        return CatalogCallCheck::Err {
            reason: CatalogFailureReason::SchemaStale,
            name: call.name.clone(),
        };
    }
    CatalogCallCheck::Ok(entry.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_each_tool_digest_independently() {
        let a = CatalogTool {
            name: "read".into(),
            description: "Read file".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            execution_mode: None,
        };
        let b = CatalogTool {
            name: "bash".into(),
            description: "Run bash".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            execution_mode: Some(ToolExecutionMode::Sequential),
        };
        let catalog = build_tool_catalog([&a, &b]);
        let digest_a = catalog["read"].schema_digest.clone();
        assert!(matches!(
            check_catalog_call(
                &catalog,
                &CatalogCall {
                    name: "read".into(),
                    schema_digest: digest_a.clone(),
                }
            ),
            CatalogCallCheck::Ok(_)
        ));
        assert!(matches!(
            check_catalog_call(
                &catalog,
                &CatalogCall {
                    name: "read".into(),
                    schema_digest: "wrong".into(),
                }
            ),
            CatalogCallCheck::Err {
                reason: CatalogFailureReason::SchemaStale,
                ..
            }
        ));
        assert!(matches!(
            check_catalog_call(
                &catalog,
                &CatalogCall {
                    name: "missing".into(),
                    schema_digest: digest_a,
                }
            ),
            CatalogCallCheck::Err {
                reason: CatalogFailureReason::ToolUnknown,
                ..
            }
        ));
    }
}
