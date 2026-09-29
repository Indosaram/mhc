//! Tool-rule normalization and last-match-wins lookup (`agents/tools.ts`).

use serde_json::Value;

use super::types::AgentToolRule;

const TOOL_ACTIONS: [&str; 3] = ["allow", "deny", "ask"];

/// Whether `value` matches the TS `ToolsInputSchema` union.
pub(crate) fn is_valid_tools_input(value: &Value) -> bool {
    match value {
        Value::Array(entries) => entries.iter().all(is_valid_tool_rule_entry),
        Value::Object(record) => record.values().all(is_valid_tool_permission),
        _ => false,
    }
}

fn is_tool_action(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|action| TOOL_ACTIONS.contains(&action))
}

fn is_valid_tool_permission(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::String(_) => is_tool_action(value),
        Value::Object(commands) => commands.values().all(is_tool_action),
        _ => false,
    }
}

fn is_valid_tool_rule_entry(entry: &Value) -> bool {
    match entry {
        Value::String(_) => true,
        Value::Object(object) => {
            let optional_string = |key: &str| object.get(key).is_none_or(Value::is_string);
            let optional_bool = |key: &str| object.get(key).is_none_or(Value::is_boolean);
            optional_string("pattern")
                && optional_string("name")
                && optional_string("tool")
                && optional_bool("allow")
                && optional_bool("deny")
                && object.get("action").is_none_or(is_tool_action)
        }
        _ => false,
    }
}

/// Converts a validated `tools` input (record or array form) into ordered rules; `None` when the
/// input is absent. A nested command map yields `"<tool> <pattern>"` compound rules; only
/// `"allow"` grants, so `"ask"` overrides a broader allow.
pub fn normalize_tool_rules(input: Option<&Value>) -> Option<Vec<AgentToolRule>> {
    let rules = match input? {
        Value::Array(entries) => entries.iter().flat_map(normalize_tool_rule_entry).collect(),
        Value::Object(record) => record
            .iter()
            .flat_map(|(tool, value)| normalize_tool_permission(tool, value))
            .collect(),
        _ => Vec::new(),
    };
    Some(rules)
}

fn normalize_tool_permission(tool: &str, value: &Value) -> Vec<AgentToolRule> {
    match value {
        Value::Bool(allow) => vec![AgentToolRule::new(tool, *allow)],
        Value::String(action) => vec![AgentToolRule::new(tool, action == "allow")],
        Value::Object(commands) => commands
            .iter()
            .map(|(command_pattern, action)| {
                AgentToolRule::new(
                    &format!("{tool} {command_pattern}"),
                    action.as_str() == Some("allow"),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn normalize_tool_rule_entry(entry: &Value) -> Vec<AgentToolRule> {
    match entry {
        Value::String(entry) => match entry.strip_prefix('!').or_else(|| entry.strip_prefix('-')) {
            Some(pattern) => vec![AgentToolRule::new(pattern, false)],
            None => vec![AgentToolRule::new(entry, true)],
        },
        Value::Object(object) => {
            let text = |key: &str| object.get(key).and_then(Value::as_str);
            let Some(pattern) = text("pattern")
                .or_else(|| text("name"))
                .or_else(|| text("tool"))
            else {
                return Vec::new();
            };
            if let Some(action) = text("action") {
                return vec![AgentToolRule::new(pattern, action == "allow")];
            }
            if object.get("deny").and_then(Value::as_bool) == Some(true) {
                return vec![AgentToolRule::new(pattern, false)];
            }
            let allow = object.get("allow").and_then(Value::as_bool).unwrap_or(true);
            vec![AgentToolRule::new(pattern, allow)]
        }
        _ => Vec::new(),
    }
}

/// The decision of the last rule matching `tool_name`; `None` when no rule matches.
pub fn resolve_tool_rule(rules: &[AgentToolRule], tool_name: &str) -> Option<bool> {
    rules
        .iter()
        .rev()
        .find(|rule| tool_pattern_matches(&rule.pattern, tool_name))
        .map(|rule| rule.allow)
}

fn tool_pattern_matches(pattern: &str, tool_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) => tool_name.starts_with(prefix),
        None => pattern == tool_name,
    }
}
