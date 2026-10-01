//! `tools/task/categories.ts`: dynamic category and agent surfaces for the task tool description.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::agents::AgentDefinition;
use crate::category::{BUILTIN_CATEGORY_DEFAULTS, category_gate_model};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCategoryInfo {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAgentInfo {
    pub name: String,
    pub description: Option<String>,
}

/// Reads an optional-or-plain text field uniformly.
trait TextField {
    fn text(&self) -> Option<&str>;
}

impl TextField for String {
    fn text(&self) -> Option<&str> {
        Some(self)
    }
}

impl TextField for Option<String> {
    fn text(&self) -> Option<&str> {
        self.as_deref()
    }
}

impl TextField for &str {
    fn text(&self) -> Option<&str> {
        Some(*self)
    }
}

impl TextField for Option<&str> {
    fn text(&self) -> Option<&str> {
        *self
    }
}

/// Reads an optional-or-plain boolean flag uniformly (`=== true`).
trait FlagField {
    fn is_set(&self) -> bool;
}

impl FlagField for bool {
    fn is_set(&self) -> bool {
        *self
    }
}

impl FlagField for Option<bool> {
    fn is_set(&self) -> bool {
        *self == Some(true)
    }
}

fn builtin_description(name: &str) -> Option<String> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .find(|definition| definition.name == name)
        .and_then(|definition| definition.description.text().map(str::to_string))
}

/// Dynamic category surface for the tool description: builtin categories merged with omo.json ones,
/// disabled categories dropped, user descriptions winning over the builtin default text.
pub fn list_task_categories(config: &Value) -> Vec<TaskCategoryInfo> {
    let empty = Map::new();
    let user_categories = config
        .get("categories")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_string())
        .collect();
    names.extend(user_categories.keys().cloned());

    let mut entries = Vec::new();
    for name in names {
        let user_config = user_categories.get(&name);
        if user_config.and_then(|config| config.get("disable")) == Some(&Value::Bool(true)) {
            continue;
        }
        let description = user_config
            .and_then(|config| config.get("description"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| builtin_description(&name));
        let annotated = if user_config.is_none() {
            with_gate_annotation(&name, description)
        } else {
            description
        };
        entries.push(TaskCategoryInfo {
            name,
            description: annotated,
        });
    }
    entries
}

/// A builtin-only gated category stays listed with its required model, because the live registry is
/// not available when the task tool description is built; the spawn-time resolver owns the real gate.
fn with_gate_annotation(name: &str, description: Option<String>) -> Option<String> {
    let Some(gate_model) = category_gate_model(name) else {
        return description;
    };
    let annotation = format!("(requires {gate_model})");
    Some(match description {
        None => annotation,
        Some(description) => format!("{description} {annotation}"),
    })
}

/// Agent types surfaced from the loader; disabled definitions are hidden.
pub fn list_task_agents(agents: &BTreeMap<String, AgentDefinition>) -> Vec<TaskAgentInfo> {
    agents
        .values()
        .filter(|agent| !agent.disable.is_set())
        .map(|agent| TaskAgentInfo {
            name: agent.name.to_string(),
            description: agent.description.text().map(str::to_string),
        })
        .collect()
}
