//! Raw agent frontmatter validation and normalization (`agents/schema.ts`).

use serde_json::{Map, Value};

use super::tools::{is_valid_tools_input, normalize_tool_rules};
use super::types::{AgentDefinition, AgentModelCandidate, AgentModelEntry};

const REASONING_EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Validates a raw definition like the TS `RawAgentDefinitionSchema` (unknown keys pass through)
/// and normalizes it; `Err` carries the dotted issue paths.
pub(crate) fn parse_raw_agent_definition(
    name: &str,
    raw: &Value,
    prompt: Option<String>,
) -> Result<AgentDefinition, Vec<String>> {
    let Some(raw) = raw.as_object() else {
        return Err(vec![String::new()]);
    };
    let mut issues = Vec::new();
    let string_fields = [
        "description",
        "prompt",
        "mode",
        "model",
        "variant",
        "executionMode",
        "execution_mode",
    ];
    for key in string_fields {
        check(&mut issues, raw, key, Value::is_string);
    }
    for key in ["disable", "background"] {
        check(&mut issues, raw, key, Value::is_boolean);
    }
    for key in [
        "allowedSubagents",
        "allowed_subagents",
        "disallowedTools",
        "disallowed_tools",
    ] {
        check(&mut issues, raw, key, |value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        });
    }
    for key in ["maxDepth", "max_depth", "maxTurns", "max_turns"] {
        check(&mut issues, raw, key, |value| value.as_u64().is_some());
    }
    check(&mut issues, raw, "reasoningEffort", is_reasoning_effort);
    check(&mut issues, raw, "temperature", |value| {
        value
            .as_f64()
            .is_some_and(|number| (0.0..=2.0).contains(&number))
    });
    check(&mut issues, raw, "tools", is_valid_tools_input);
    if let Some(models) = raw.get("models") {
        match models.as_array() {
            Some(entries) => {
                for (index, entry) in entries.iter().enumerate() {
                    if parse_model_entry(entry).is_none() {
                        issues.push(format!("models.{index}"));
                    }
                }
            }
            None => issues.push("models".to_string()),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_string);
    let strings = |key: &str| {
        raw.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
    };
    let number = |key: &str| raw.get(key).and_then(Value::as_u64);
    let flag = |key: &str| raw.get(key).and_then(Value::as_bool);
    Ok(AgentDefinition {
        name: name.to_string(),
        description: text("description"),
        prompt,
        mode: text("mode"),
        model: text("model"),
        models: raw
            .get("models")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(parse_model_entry).collect()),
        variant: text("variant"),
        reasoning_effort: text("reasoningEffort"),
        temperature: raw.get("temperature").and_then(Value::as_f64),
        tools: normalize_tool_rules(raw.get("tools")),
        disable: flag("disable"),
        background: flag("background"),
        execution_mode: text("executionMode").or_else(|| text("execution_mode")),
        allowed_subagents: strings("allowedSubagents").or_else(|| strings("allowed_subagents")),
        disallowed_tools: strings("disallowedTools").or_else(|| strings("disallowed_tools")),
        max_depth: number("maxDepth").or_else(|| number("max_depth")),
        max_turns: number("maxTurns").or_else(|| number("max_turns")),
    })
}

fn check(
    issues: &mut Vec<String>,
    raw: &Map<String, Value>,
    key: &str,
    valid: impl Fn(&Value) -> bool,
) {
    if raw.get(key).is_some_and(|value| !valid(value)) {
        issues.push(key.to_string());
    }
}

fn is_reasoning_effort(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|effort| REASONING_EFFORTS.contains(&effort))
}

/// A `models[]` entry: a string, or an object with a string `model` and optional tuning keys.
pub(crate) fn parse_model_entry(entry: &Value) -> Option<AgentModelEntry> {
    match entry {
        Value::String(model) => Some(AgentModelEntry::Model(model.clone())),
        Value::Object(object) => {
            let model = object.get("model")?.as_str()?;
            let optional_text = |key: &str| match object.get(key) {
                None => Some(None),
                Some(Value::String(text)) => Some(Some(text.clone())),
                Some(_) => None,
            };
            let reasoning_effort = match object.get("reasoningEffort") {
                None => None,
                Some(value) if is_reasoning_effort(value) => value.as_str().map(str::to_string),
                Some(_) => return None,
            };
            Some(AgentModelEntry::Candidate(AgentModelCandidate {
                model: model.to_string(),
                variant: optional_text("variant")?,
                reasoning: object
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                reasoning_effort,
            }))
        }
        _ => None,
    }
}
