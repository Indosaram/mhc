//! Port of senpi packages/coding-agent/src/core/remote-catalog-merge.ts.

use maho_ai::model::Model;
use serde_json::Value;

const MODEL_CAPABILITY_FIELDS: [&str; 9] = [
    "contextWindow",
    "maxTokens",
    "input",
    "reasoning",
    "thinkingLevelMap",
    "upstreamModelId",
    "serviceTier",
    "recoverTextToolCalls",
    "compat",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCatalogConflict {
    pub provider_id: String,
    pub model_id: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteCatalogMerge {
    pub models: Vec<Model>,
    pub conflicts: Vec<RemoteCatalogConflict>,
}

fn model_value(model: &Model) -> Value {
    serde_json::to_value(model).unwrap_or(Value::Null)
}

pub fn merge_remote_catalog_models(provider_id: &str, baseline: &[Model], dynamic: &[Model]) -> RemoteCatalogMerge {
    let mut merged = baseline.to_vec();
    let mut conflicts = Vec::new();
    for model in dynamic {
        let index = merged.iter().position(|entry| entry.id == model.id);
        let Some(index) = index else {
            merged.push(model.clone());
            continue;
        };
        let static_model = merged[index].clone();
        let static_value = model_value(&static_model);
        let dynamic_value = model_value(model);
        let fields: Vec<String> = MODEL_CAPABILITY_FIELDS
            .iter()
            .filter(|field| static_value.get(*field) != dynamic_value.get(*field))
            .map(|field| (*field).to_owned())
            .collect();
        if !fields.is_empty() {
            conflicts.push(RemoteCatalogConflict {
                provider_id: provider_id.to_owned(),
                model_id: model.id.clone(),
                fields,
            });
        }
        let mut replacement = static_model;
        replacement.name = model.name.clone();
        replacement.cost = model.cost.clone();
        merged[index] = replacement;
    }
    RemoteCatalogMerge { models: merged, conflicts }
}

fn is_finite_number(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some()
}

fn is_input_array(value: Option<&Value>) -> bool {
    match value.and_then(Value::as_array) {
        Some(items) => items.iter().all(|modality| {
            matches!(modality.as_str(), Some("text") | Some("image") | Some("video"))
        }),
        None => false,
    }
}

fn is_model_cost(value: Option<&Value>) -> bool {
    let Some(cost) = value.and_then(Value::as_object) else { return false };
    is_finite_number(cost.get("input"))
        && is_finite_number(cost.get("output"))
        && is_finite_number(cost.get("cacheRead"))
        && is_finite_number(cost.get("cacheWrite"))
}

fn is_model_catalog_entry(value: &Value) -> bool {
    let Some(object) = value.as_object() else { return false };
    object.get("id").and_then(Value::as_str).is_some()
        && object.get("name").and_then(Value::as_str).is_some()
        && object.get("api").and_then(Value::as_str).is_some()
        && object.get("provider").and_then(Value::as_str).is_some()
        && object.get("baseUrl").and_then(Value::as_str).is_some()
        && object.get("reasoning").and_then(Value::as_bool).is_some()
        && is_input_array(object.get("input"))
        && is_model_cost(object.get("cost"))
        && is_finite_number(object.get("contextWindow"))
        && is_finite_number(object.get("maxTokens"))
}

pub fn parse_remote_catalog(provider_id: &str, value: &Value) -> Result<Vec<Model>, String> {
    let entries: Vec<Value> = match value {
        Value::Array(items) => items.clone(),
        Value::Object(object) => match object.get("models") {
            Some(Value::Array(items)) => items.clone(),
            _ => object.values().cloned().collect(),
        },
        _ => Vec::new(),
    };
    if !entries.iter().all(is_model_catalog_entry) {
        return Err(format!("Invalid model catalog for provider \"{provider_id}\""));
    }
    entries
        .into_iter()
        .map(|mut entry| {
            if let Some(object) = entry.as_object_mut() {
                object.insert("provider".into(), Value::String(provider_id.to_owned()));
            }
            serde_json::from_value::<Model>(entry).map_err(|error| error.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(id: &str, name: &str, context_window: u64) -> Model {
        serde_json::from_value(json!({
            "id": id, "name": name, "api": "openai-completions", "provider": "p", "baseUrl": "",
            "reasoning": false, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": context_window, "maxTokens": 100
        }))
        .expect("model")
    }

    #[test]
    fn a_new_dynamic_model_is_appended() {
        let baseline = vec![model("a", "A", 1000)];
        let dynamic = vec![model("b", "B", 2000)];
        let merged = merge_remote_catalog_models("p", &baseline, &dynamic);
        assert_eq!(merged.models.len(), 2);
        assert!(merged.conflicts.is_empty());
    }

    #[test]
    fn a_capability_conflict_is_reported() {
        let baseline = vec![model("a", "A", 1000)];
        let dynamic = vec![model("a", "A2", 5000)];
        let merged = merge_remote_catalog_models("p", &baseline, &dynamic);
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].fields.contains(&"contextWindow".to_owned()));
    }

    #[test]
    fn only_name_and_cost_are_adopted_from_dynamic() {
        let baseline = vec![model("a", "A", 1000)];
        let dynamic = vec![model("a", "Renamed", 5000)];
        let merged = merge_remote_catalog_models("p", &baseline, &dynamic);
        assert_eq!(merged.models[0].name, "Renamed");
        assert_eq!(merged.models[0].context_window, 1000);
    }

    #[test]
    fn parsing_rejects_an_invalid_entry() {
        assert!(parse_remote_catalog("p", &json!([{ "id": "a" }])).is_err());
    }

    #[test]
    fn parsing_accepts_an_array_and_stamps_the_provider() {
        let value = json!([{ "id": "a", "name": "A", "api": "openai-completions", "provider": "other", "baseUrl": "",
            "reasoning": false, "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000, "maxTokens": 100 }]);
        let models = parse_remote_catalog("p", &value).expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].provider, "p");
    }
}
