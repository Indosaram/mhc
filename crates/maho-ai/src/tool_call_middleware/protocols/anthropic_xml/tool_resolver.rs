//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/tool-resolver.ts.

use std::collections::HashMap;

use crate::types::Tool;

// CC-SDK MCP-server decoration (`mcp__server__tool`) and hashed-prefix decoration
// (`mcp_<hash>-Name`) collapse onto the same alphanumeric alias key as the
// registered snake_case tool name.
fn strip_cc_mcp_prefix(name: &str) -> String {
    let mut result = name;
    loop {
        let Some(rest) = result.strip_prefix("mcp__") else { break };
        match rest.find("__") {
            Some(idx) => result = &rest[idx + 2..],
            None => break,
        }
    }
    result.to_string()
}

fn strip_hashed_mcp_prefix(name: &str) -> String {
    if !name.to_ascii_lowercase().starts_with("mcp_") {
        return name.to_string();
    }
    let rest = &name[4..];
    let hash_len = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).count();
    if hash_len == 0 {
        return name.to_string();
    }
    match rest[hash_len..].strip_prefix('-') {
        Some(after) => after.to_string(),
        None => name.to_string(),
    }
}

fn to_alias_key(name: &str) -> String {
    let stripped = strip_hashed_mcp_prefix(&strip_cc_mcp_prefix(name));
    stripped.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

pub struct ToolResolver<'a> {
    exact: HashMap<&'a str, &'a Tool>,
    insensitive: HashMap<String, Option<&'a Tool>>,
    alias: HashMap<String, Option<&'a Tool>>,
}

impl<'a> ToolResolver<'a> {
    pub fn new(tools: &'a [Tool]) -> Self {
        let exact: HashMap<&str, &Tool> = tools.iter().map(|t| (t.name.as_str(), t)).collect();
        let mut insensitive: HashMap<String, Option<&Tool>> = HashMap::new();
        let mut alias: HashMap<String, Option<&Tool>> = HashMap::new();
        for tool in tools {
            let normalized = tool.name.to_lowercase();
            insensitive
                .entry(normalized)
                .and_modify(|existing| {
                    if !existing.is_some_and(|t| std::ptr::eq(t, tool)) {
                        *existing = None;
                    }
                })
                .or_insert(Some(tool));
            let alias_key = to_alias_key(&tool.name);
            alias
                .entry(alias_key)
                .and_modify(|existing| {
                    if !existing.is_some_and(|t| std::ptr::eq(t, tool)) {
                        *existing = None;
                    }
                })
                .or_insert(Some(tool));
        }
        Self { exact, insensitive, alias }
    }

    pub fn resolve(&self, tool_name: &str) -> Option<&'a Tool> {
        if let Some(tool) = self.exact.get(tool_name) {
            return Some(tool);
        }
        if let Some(Some(tool)) = self.insensitive.get(&tool_name.to_lowercase()) {
            return Some(tool);
        }
        self.alias.get(&to_alias_key(tool_name)).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn resolves_exact_name() {
        let tools = vec![tool("get_weather")];
        let resolver = ToolResolver::new(&tools);
        assert_eq!(resolver.resolve("get_weather").map(|t| t.name.as_str()), Some("get_weather"));
    }

    #[test]
    fn resolves_case_insensitively_when_unambiguous() {
        let tools = vec![tool("get_weather")];
        let resolver = ToolResolver::new(&tools);
        assert_eq!(resolver.resolve("GET_WEATHER").map(|t| t.name.as_str()), Some("get_weather"));
    }

    #[test]
    fn resolves_cc_sdk_mcp_decorated_name_via_alias() {
        let tools = vec![tool("get_weather")];
        let resolver = ToolResolver::new(&tools);
        assert_eq!(resolver.resolve("mcp__server__get_weather").map(|t| t.name.as_str()), Some("get_weather"));
    }

    #[test]
    fn resolves_hashed_mcp_prefix_name_via_alias() {
        let tools = vec![tool("get_weather")];
        let resolver = ToolResolver::new(&tools);
        assert_eq!(resolver.resolve("mcp_ab12cd-GetWeather").map(|t| t.name.as_str()), Some("get_weather"));
    }

    #[test]
    fn returns_none_for_ambiguous_alias_collision() {
        let tools = vec![tool("Get_Weather"), tool("get_weather")];
        let resolver = ToolResolver::new(&tools);
        assert!(resolver.resolve("GET_WEATHER").is_none());
    }

    #[test]
    fn returns_none_for_unknown_tool() {
        let tools = vec![tool("get_weather")];
        let resolver = ToolResolver::new(&tools);
        assert!(resolver.resolve("unknown").is_none());
    }
}
