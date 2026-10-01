//! Port of `components/exploration-rules.ts`.
//!
//! senpi reads the entry through the rule-activation extension's shared parser; that crate is owned
//! by another todo, so this port parses the project-rules shape the transcript needs and ignores
//! every other kind, which is exactly what the caller does with the result.
use serde_json::Value;

use super::custom_entry::CustomEntryComponent;
use super::tool_execution::ToolExecutionComponent;
use std::cell::RefCell;
use std::rc::Rc;

pub const RULE_ACTIVATION_ENTRY_TYPE: &str = "rule-activation";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRulesActivation {
    pub target_path: String,
    pub rules: Vec<String>,
    pub tool_call_id: Option<String>,
}

pub fn parse_project_rules_activation(value: &Value) -> Option<ProjectRulesActivation> {
    if !value.is_object() || value.get("kind").and_then(Value::as_str) != Some("project-rules") {
        return None;
    }
    let target_path = value.get("targetPath").and_then(Value::as_str).filter(|path| !path.is_empty())?;
    let rules = value.get("rules").and_then(Value::as_array)?;
    if rules.is_empty() || rules.iter().any(|rule| rule.as_str().is_none_or(str::is_empty)) {
        return None;
    }
    let tool_call_id = value
        .get("toolCallId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Some(ProjectRulesActivation {
        target_path: target_path.to_owned(),
        rules: rules.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        tool_call_id,
    })
}

pub fn project_rules_of_call(
    card: &CustomEntryComponent,
    calls: &[(Rc<RefCell<ToolExecutionComponent>>, super::exploration_call::ExplorationCall)],
) -> Option<Vec<String>> {
    let entry = &card.custom_entry;
    if entry.get("customType").and_then(Value::as_str) != Some(RULE_ACTIVATION_ENTRY_TYPE) {
        return None;
    }
    let null = Value::Null;
    let details = parse_project_rules_activation(entry.get("data").unwrap_or(&null))?;
    let tool_call_id = details.tool_call_id?;
    let matches_call = calls.iter().any(|(component, _)| {
        let (_, call_id, _) = component.borrow().identity();
        call_id == tool_call_id
    });
    if matches_call { Some(details.rules) } else { None }
}
