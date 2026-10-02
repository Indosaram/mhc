use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
use maho_ext_api::{ToolDefinition, ToolExecutionMode, ToolResult};
use serde_json::json;

pub fn order_active_set(names: &[String], reference: &[String], catalog_names: &BTreeSet<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let unique = names.iter().filter(|name| seen.insert((*name).clone())).cloned().collect::<Vec<_>>();
    let rank = reference.iter().enumerate().map(|(index, name)| (name, index)).collect::<BTreeMap<_, _>>();
    let mut base = unique.iter().filter(|name| !catalog_names.contains(*name)).cloned().collect::<Vec<_>>();
    base.sort_by_key(|name| rank.get(name).copied().unwrap_or(usize::MAX));
    let mut catalog = unique.into_iter().filter(|name| catalog_names.contains(name)).collect::<Vec<_>>();
    catalog.sort_by_cached_key(|name| name.encode_utf16().collect::<Vec<_>>());
    base.extend(catalog); base
}

pub struct McpStubPromotion {
    pub full: ToolDefinition,
    pub promote: Arc<dyn Fn() + Send + Sync>,
}

pub fn build_mcp_stub_definition(name: &str, promotion: Option<McpStubPromotion>) -> ToolDefinition {
    let description = if promotion.is_some() {
        format!("Deferred MCP tool. Call {name} with its real arguments; it activates and runs on this first call (tool_search lists its schema).")
    } else {
        format!("Inactive MCP tool. Run tool_search to activate {name}, then call it on your next turn.")
    };
    let name_owned = name.to_owned();
    let promotion = promotion.map(Arc::new);
    let mut definition = ToolDefinition::new(name, &description, json!({"type":"object","properties":{},"additionalProperties":true}), Arc::new(move |call| {
        let promotion = promotion.clone(); let name = name_owned.clone();
        Box::pin(async move {
            match promotion {
                Some(promotion) => {(promotion.promote)(); (promotion.full.execute)(call).await}
                None => Ok(ToolResult::text(format!("{name} is not active. Use tool_search to activate it, then call it."))),
            }
        })
    }));
    definition.execution_mode = Some(ToolExecutionMode::Parallel); definition
}
