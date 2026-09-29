use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::models::model_catalog_cycles::find_model_catalog_cycles;

pub const MODEL_CATALOG_CYCLE: &str = "model_catalog_cycle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReferenceDiagnostic {
    pub kind: &'static str,
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolveModelReferencesResult {
    pub diagnostics: Vec<ModelReferenceDiagnostic>,
    pub view: Value,
}

fn catalog_reference(
    model: &str,
    reasoning: Option<&Value>,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) -> Option<Value> {
    let entry = catalog?.get(model)?;
    if cycle_names.contains(model) {
        return None;
    }
    let mut resolved = Map::new();
    resolved.insert("model".into(), entry.get("model")?.clone());
    if let Some(reasoning) = reasoning
        .cloned()
        .or_else(|| entry.get("reasoning").cloned())
    {
        resolved.insert("reasoning".into(), reasoning);
    }
    Some(Value::Object(resolved))
}

fn resolve_model_entry(
    entry: &Value,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) -> Value {
    match entry {
        Value::String(name) => {
            let Some(resolved) = catalog_reference(name, None, catalog, cycle_names) else {
                return entry.clone();
            };
            if resolved.get("reasoning").is_none() {
                resolved
                    .get("model")
                    .cloned()
                    .unwrap_or_else(|| entry.clone())
            } else {
                resolved
            }
        }
        Value::Object(map) => {
            let Some(name) = map.get("model").and_then(Value::as_str) else {
                return entry.clone();
            };
            let Some(resolved) =
                catalog_reference(name, map.get("reasoning"), catalog, cycle_names)
            else {
                return entry.clone();
            };
            let mut out = map.clone();
            if let Some(model) = resolved.get("model") {
                out.insert("model".into(), model.clone());
            }
            if map.get("reasoning").is_none()
                && let Some(reasoning) = resolved.get("reasoning")
            {
                out.insert("reasoning".into(), reasoning.clone());
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn resolve_fallback_models(
    fallback_models: &Value,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) -> Value {
    match fallback_models {
        Value::String(name) => {
            let Some(resolved) = catalog_reference(name, None, catalog, cycle_names) else {
                return fallback_models.clone();
            };
            if resolved.get("reasoning").is_none() {
                resolved
                    .get("model")
                    .cloned()
                    .unwrap_or_else(|| fallback_models.clone())
            } else {
                Value::Array(vec![resolved])
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|entry| resolve_model_entry(entry, catalog, cycle_names))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn resolve_model_and_reasoning(
    definition: &Map<String, Value>,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) -> Option<Value> {
    let name = definition.get("model")?.as_str()?;
    catalog_reference(name, definition.get("reasoning"), catalog, cycle_names)
}

fn apply_resolved_model(
    resolved: &mut Map<String, Value>,
    definition: &Map<String, Value>,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) {
    let Some(resolved_model) = resolve_model_and_reasoning(definition, catalog, cycle_names) else {
        return;
    };
    if let Some(model) = resolved_model.get("model") {
        resolved.insert("model".into(), model.clone());
    }
    if definition.get("reasoning").is_none()
        && let Some(reasoning) = resolved_model.get("reasoning")
    {
        resolved.insert("reasoning".into(), reasoning.clone());
    }
}

fn resolve_agent_definition(
    definition: &Value,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) -> Value {
    let Value::Object(map) = definition else {
        return definition.clone();
    };
    let mut resolved = map.clone();
    apply_resolved_model(&mut resolved, map, catalog, cycle_names);
    if let Some(Value::Array(models)) = map.get("models") {
        resolved.insert(
            "models".into(),
            Value::Array(
                models
                    .iter()
                    .map(|entry| resolve_model_entry(entry, catalog, cycle_names))
                    .collect(),
            ),
        );
    }
    Value::Object(resolved)
}

fn resolve_category_definition(
    definition: &Value,
    catalog: Option<&Map<String, Value>>,
    cycle_names: &HashSet<String>,
) -> Value {
    let Value::Object(map) = definition else {
        return definition.clone();
    };
    let mut resolved = map.clone();
    apply_resolved_model(&mut resolved, map, catalog, cycle_names);
    if let Some(Value::Array(models)) = map.get("models") {
        resolved.insert(
            "models".into(),
            Value::Array(
                models
                    .iter()
                    .map(|entry| resolve_model_entry(entry, catalog, cycle_names))
                    .collect(),
            ),
        );
    }
    if let Some(fallback_models) = map.get("fallback_models") {
        let resolved_fallback = resolve_fallback_models(fallback_models, catalog, cycle_names);
        resolved.insert("fallback_models".into(), resolved_fallback);
    }
    Value::Object(resolved)
}

fn cycle_diagnostics(catalog: Option<&Map<String, Value>>) -> Vec<ModelReferenceDiagnostic> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    find_model_catalog_cycles(catalog)
        .into_iter()
        .map(|name| {
            let self_reference = catalog
                .get(&name)
                .and_then(|entry| entry.get("model"))
                .and_then(Value::as_str)
                .map(|model| model == name)
                .unwrap_or(false);
            let message = if self_reference {
                format!("Model catalog entry \"{name}\" references itself")
            } else {
                format!("Model catalog entry \"{name}\" participates in a reference cycle")
            };
            ModelReferenceDiagnostic {
                kind: MODEL_CATALOG_CYCLE,
                message,
                path: format!("models.{name}.model"),
            }
        })
        .collect()
}

pub fn resolve_model_references(view: &Value) -> ResolveModelReferencesResult {
    let catalog = view.get("models").and_then(Value::as_object);
    let diagnostics = cycle_diagnostics(catalog);
    let cycle_names: HashSet<String> = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.path.split('.').nth(1).map(str::to_string))
        .collect();

    let mut resolved_view = view.as_object().cloned().unwrap_or_default();

    if let Some(Value::Object(agents)) = view.get("agents") {
        let resolved_agents: Map<String, Value> = agents
            .iter()
            .map(|(name, definition)| {
                (
                    name.clone(),
                    resolve_agent_definition(definition, catalog, &cycle_names),
                )
            })
            .collect();
        resolved_view.insert("agents".into(), Value::Object(resolved_agents));
    }

    if let Some(Value::Object(categories)) = view.get("categories") {
        let resolved_categories: Map<String, Value> = categories
            .iter()
            .map(|(name, definition)| {
                (
                    name.clone(),
                    resolve_category_definition(definition, catalog, &cycle_names),
                )
            })
            .collect();
        resolved_view.insert("categories".into(), Value::Object(resolved_categories));
    }

    ResolveModelReferencesResult {
        diagnostics,
        view: Value::Object(resolved_view),
    }
}
